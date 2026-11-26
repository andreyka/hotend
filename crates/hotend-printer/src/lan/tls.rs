//! The TLS side of the LAN transport: one client configuration for MQTT and FTPS.
//!
//! Guarantees:
//! - only TLS 1.2 is offered (Hotend's choice, stated in the profile);
//! - the server's certificate must carry the serial number the user confirmed as its common
//!   name, and the server must prove it holds the certificate's key (the handshake
//!   signature is verified with the provider's algorithms);
//! - at pairing the certificate must also chain to one of the [`Issuers`]: for a real
//!   printer Bambu Lab's certificate authorities, read from a file the user supplies
//!   ([`Issuers::from_file`]; OpenBambuAPI `tls.md` says to trust them and then check the
//!   common name). Hotend does not ship that file: the certificates are Bambu Lab's, and
//!   OpenBambuAPI, the collection that publishes them, is under the GFDL; neither was shown
//!   to be compatible with this project's licence. No text or file of OpenBambuAPI is in
//!   this tree: what is used from it are facts ([`crate::protocol`] says which, and
//!   `profiles/bambu.toml` gives the source of each row). A serial
//!   number is not a secret (the printer announces it), so a certificate that merely names it
//!   is not enough: whoever is pinned is sent the access code on every later connection;
//! - after pairing, the certificate must be the very one seen at pairing (SHA-256 of the
//!   DER certificate), whatever its common name says;
//! - why a certificate was refused is kept for the error the user sees;
//! - for the status listener ([`super::listen`]) the same checks run in its two modes: the
//!   chain to the [`Issuers`] of the person's file plus the serial, or a pinned fingerprint
//!   plus the serial. Its first look (`Expect::Probe`) accepts any certificate that names the
//!   serial, only to show its fingerprint and subject; nothing is sent over that handshake.
//!
//! The server name sent is the IP address, so no SNI extension goes out. OpenBambuAPI
//! `tls.md` mentions SNI for the name check, which is done here by hand against the serial;
//! whether a printer insists on the extension is on the first-printer checklist.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::verify_server_cert_signed_by_trust_anchor;
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::ParsedCertificate;
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::ident::Serial;
use crate::job::hex;

/// Why the printer's certificate was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CertificateProblem {
    /// The certificate cannot be read.
    #[error("the device sent a certificate that cannot be read")]
    Unreadable,
    /// The certificate is for another serial number.
    #[error(
        "the device identifies itself as `{found}`, not as the printer with the serial number you confirmed"
    )]
    WrongSerial {
        /// The common name found, cleaned.
        found: String,
    },
    /// The certificate is not the one seen at pairing.
    #[error("the device's certificate is not the one this printer had when it was paired")]
    Changed,
    /// The certificate was not issued by the printer's maker.
    #[error(
        "the device's certificate was not issued by Bambu Lab, so it may not be your printer even though it names its serial number"
    )]
    UntrustedIssuer,
}

/// The certificate authorities a printer's certificate must chain to at pairing.
#[derive(Debug, Clone)]
pub struct Issuers(Arc<RootCertStore>);

/// The largest certificate file [`Issuers::from_file`] reads.
pub const MAX_ISSUERS_FILE_BYTES: u64 = 1024 * 1024;

/// SHA-256 of the DER form of each of Bambu Lab's printer certificate authorities known to
/// this version, lower-case hex: BBL CA (to 2032), BBL CA2 RSA and BBL CA2 ECC, each
/// self-signed (to 2050) and signed by BBL CA (to 2035). Computed on 2026-11-18 from the
/// certificates OpenBambuAPI publishes (`examples/ca_cert.pem`); only these numbers are
/// kept, not the certificates. Which of them signs an X2D's certificate is not known.
pub const BAMBU_LAB_CA_SHA256: [&str; 5] = [
    "030bca81cece18b7eff3cfd2b75d09d3efca893bc069609e37fa04257fe4d840",
    "e98f19578b3f124ace6b8a247ffeda52dc99c89fd4e7d20c82829977b7f33502",
    "1f99d846718c8d333ac319cb8e70919ab63236090fbadde909ba4a6f0867a683",
    "3d13019b45a4fa8a6cb8dc0fd7becbb19bb2bf0c2fcb9434a9345fa58680da0b",
    "e898483224f9092f575d627433bb21ed9d125a77e3635b0da6fc18fe850a02d7",
];

impl Issuers {
    /// The certificate authorities in a PEM file the user supplies: for a real printer,
    /// Bambu Lab's. `None` when the path is not a plain file of at most
    /// [`MAX_ISSUERS_FILE_BYTES`] or holds no certificate that can be used; pairing then
    /// refuses ([`super::LanError::NoIssuers`]).
    pub fn from_file(path: &Path) -> Option<Self> {
        let metadata = std::fs::symlink_metadata(path).ok()?;
        if !metadata.is_file() || metadata.len() > MAX_ISSUERS_FILE_BYTES {
            return None;
        }
        Self::from_pem(&std::fs::read_to_string(path).ok()?)
    }

    /// [`Issuers::from_file`] for a real printer: every certificate in the file must be one
    /// of Bambu Lab's ([`BAMBU_LAB_CA_SHA256`]). `None` otherwise, so a file of some other
    /// authority, which would let any device it signed be paired, is refused whole.
    pub fn bambu_lab_from_file(path: &Path) -> Option<Self> {
        let metadata = std::fs::symlink_metadata(path).ok()?;
        if !metadata.is_file() || metadata.len() > MAX_ISSUERS_FILE_BYTES {
            return None;
        }
        let pem = std::fs::read_to_string(path).ok()?;
        for certificate in CertificateDer::pem_slice_iter(pem.as_bytes()) {
            let certificate = certificate.ok()?;
            if !BAMBU_LAB_CA_SHA256.contains(&fingerprint(certificate.as_ref()).as_str()) {
                return None;
            }
        }
        Self::from_pem(&pem)
    }

    /// The certificates of a PEM text. `None` when it holds none that can be used.
    ///
    /// Pairing a real printer with anything but Bambu Lab's authorities gives up the check
    /// that the device is one.
    pub fn from_pem(pem: &str) -> Option<Self> {
        let mut store = RootCertStore::empty();
        for certificate in CertificateDer::pem_slice_iter(pem.as_bytes()) {
            store.add(certificate.ok()?).ok()?;
        }
        (!store.is_empty()).then(|| Self(Arc::new(store)))
    }

    /// How many authorities there are.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there is none (then no certificate can be paired).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// What the verifier accepts.
#[derive(Debug, Clone)]
pub(crate) enum Expect {
    /// Pairing: a certificate naming this serial that chains to one of the issuers; its
    /// fingerprint is recorded.
    Serial(Serial, Issuers),
    /// After pairing: this serial and this certificate.
    Pinned(Serial, String),
    /// A first look for the status listener ([`super::listen::probe_certificate`]): a
    /// certificate naming this serial, whoever issued it, so that its fingerprint and
    /// subject can be shown to the person. The handshake completes (the server proves it
    /// holds the certificate's key) and is closed at once; nothing is sent over it.
    Probe(Serial),
}

/// What the verifier saw, shared with the code that reports errors.
#[derive(Debug, Default)]
pub(crate) struct Seen {
    pub(crate) fingerprint: Option<String>,
    /// The certificate's subject and issuer as text, cleaned and cut (for the probe).
    pub(crate) subject: Option<String>,
    pub(crate) issuer: Option<String>,
    pub(crate) problem: Option<CertificateProblem>,
}

#[derive(Debug)]
struct PinnedVerifier {
    expect: Expect,
    provider: Arc<CryptoProvider>,
    seen: Arc<Mutex<Seen>>,
}

/// SHA-256 of a DER certificate, lower-case hex.
pub(crate) fn fingerprint(certificate: &[u8]) -> String {
    hex(&Sha256::digest(certificate))
}

/// The certificate's subject common name.
fn common_name(certificate: &[u8]) -> Option<String> {
    let (_, parsed) = x509_parser::parse_x509_certificate(certificate).ok()?;
    let mut names = parsed.subject().iter_common_name();
    let name = names.next()?.as_str().ok()?.to_owned();
    // Two common names would let a certificate claim two serials.
    names.next().is_none().then_some(name)
}

/// The certificate's subject and issuer names as text (`CN=...`), cleaned and cut to a
/// length the interface can show.
fn names(certificate: &[u8]) -> Option<(String, String)> {
    let (_, parsed) = x509_parser::parse_x509_certificate(certificate).ok()?;
    let shown = |name: String| {
        crate::protocol::report::clean_text(&name)
            .chars()
            .take(200)
            .collect()
    };
    Some((
        shown(parsed.subject().to_string()),
        shown(parsed.issuer().to_string()),
    ))
}

fn check(expect: &Expect, certificate: &[u8]) -> Result<String, CertificateProblem> {
    let (Expect::Serial(serial, _) | Expect::Pinned(serial, _) | Expect::Probe(serial)) = expect;
    let name = common_name(certificate).ok_or(CertificateProblem::Unreadable)?;
    if Serial::new(&name).ok().as_ref() != Some(serial) {
        return Err(CertificateProblem::WrongSerial {
            found: crate::protocol::report::clean_text(&name)
                .chars()
                .take(40)
                .collect(),
        });
    }
    let found = fingerprint(certificate);
    match expect {
        Expect::Pinned(_, pinned) if !pinned.eq_ignore_ascii_case(&found) => {
            Err(CertificateProblem::Changed)
        }
        _ => Ok(found),
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let mut verdict = check(&self.expect, end_entity.as_ref());
        if let (Ok(_), Expect::Serial(_, issuers)) = (&verdict, &self.expect) {
            let chained = ParsedCertificate::try_from(end_entity).and_then(|parsed| {
                verify_server_cert_signed_by_trust_anchor(
                    &parsed,
                    &issuers.0,
                    intermediates,
                    now,
                    self.provider.signature_verification_algorithms.all,
                )
            });
            if chained.is_err() {
                verdict = Err(CertificateProblem::UntrustedIssuer);
            }
        }
        if let Ok(mut seen) = self.seen.lock() {
            match &verdict {
                Ok(found) => {
                    seen.fingerprint = Some(found.clone());
                    if let Some((subject, issuer)) = names(end_entity.as_ref()) {
                        seen.subject = Some(subject);
                        seen.issuer = Some(issuer);
                    }
                    seen.problem = None;
                }
                Err(problem) => seen.problem = Some(problem.clone()),
            }
        }
        verdict
            .map(|_| ServerCertVerified::assertion())
            .map_err(|problem| rustls::Error::General(problem.to_string()))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// A TLS 1.2-only client configuration that accepts `expect` and nothing else.
pub(crate) fn client_config(
    expect: Expect,
) -> Result<(Arc<ClientConfig>, Arc<Mutex<Seen>>), rustls::Error> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let verifier = PinnedVerifier {
        expect,
        provider: Arc::clone(&provider),
        seen: Arc::clone(&seen),
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS12])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok((Arc::new(config), seen))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unwrap_used, reason = "a test stops at the first surprise")]
    fn no_authority_is_shipped_and_a_file_without_one_gives_none() {
        // The certificates are not part of the crate (see the module's documentation).
        let source = include_str!("tls.rs");
        assert!(!source.contains(concat!("BEGIN ", "CERTIFICATE")));
        assert!(Issuers::from_pem("no certificate here").is_none());
        let dir = std::env::temp_dir().join(format!("hotend-issuers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.pem");
        std::fs::write(&empty, "no certificate here").unwrap();
        assert!(Issuers::from_file(&empty).is_none());
        assert!(Issuers::from_file(&dir.join("missing.pem")).is_none());
        assert!(Issuers::from_file(&dir).is_none(), "a folder is not a file");
        let big = dir.join("big.pem");
        std::fs::write(&big, vec![b'a'; MAX_ISSUERS_FILE_BYTES as usize + 1]).unwrap();
        assert!(Issuers::from_file(&big).is_none());
        assert!(Issuers::bambu_lab_from_file(&big).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
        for known in BAMBU_LAB_CA_SHA256 {
            assert!(
                known.len() == 64
                    && known
                        .chars()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{known}"
            );
        }
    }
}
