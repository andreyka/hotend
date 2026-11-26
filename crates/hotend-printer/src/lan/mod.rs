//! The real LAN transport: MQTT over TLS 1.2 for reports and commands, implicit FTPS for
//! uploads, one pinned certificate for both.
//!
//! Two features, both off by default:
//! - `lan-status`: the read-only status listener ([`listen`]) and the certificate checks
//!   ([`tls`]). This is all the application (`hotend-core`) enables; a build with only it
//!   contains no `LanPrinter`, no command publishing and no FTPS client.
//! - `lan` (implies `lan-status`): adds the commanding transport (`LanPrinter`, `pair`,
//!   `Endpoint`, FTPS upload; private module `command`). Only this crate's own tests
//!   enable it.
//!
//! **The commanding side connects to loopback addresses only.** `Endpoint` refuses every
//! other address, so `LanPrinter` can be exercised against stand-in servers inside a test
//! process and cannot reach a real printer. Lifting that is a deliberate change to
//! `Endpoint`, to be made only as a decision of its own and with the checklist in this
//! crate's `README.md`.
//!
//! **The read-only side** ([`listen`], for status only) is the one
//! exception: [`StatusListener`] connects to one private-range address the person
//! configured ([`ListenTarget`]), subscribes to the report topic and has no code path that
//! publishes. See its module documentation.
//!
//! How a printer is used:
//! 1. `pair`: the user reads the serial number from the printer and confirms it; the
//!    printer must present a certificate that names that serial and was issued by Bambu
//!    Lab ([`tls::Issuers`], read from a file the user supplies: Hotend does not ship
//!    Bambu Lab's certificates), and prove it holds its key.
//!    The certificate's fingerprint is recorded in a `Pairing`, which is not a secret and
//!    can be stored.
//! 2. `LanPrinter::connect`: from then on only that certificate is accepted
//!    ([`tls`]). The access code signs in as the profile's fixed user; it is held as an
//!    `AccessCode`, which cannot be printed, serialised or cloned.
//! 3. The result is a `Printer`: status from the report topic through the hostile-input
//!    parser, uploads over FTPS with the bytes hashed as they are sent, commands from the
//!    closed `Command` list on the request topic.
//!
//! Guarantees:
//! - a connection that drops is not silently re-opened: the printer then reports
//!   `PrinterError::Unreachable` and a new `LanPrinter::connect` is needed, so no
//!   command waits in a queue to be delivered later;
//! - a command is sent only while the session is connected;
//! - incoming packets are capped at the report parser's limit.
//!
//! Protocol facts: OpenBambuAPI `mqtt.md`, `ftp.md`, `tls.md`; ha-bambulab
//! `pybambu/bambu_client.py` (MIT). All unverified on an X2D.

#[cfg(feature = "lan")]
mod command;
#[cfg(feature = "lan")]
mod ftps;
pub mod listen;
pub mod tls;

#[cfg(feature = "lan")]
pub use self::command::{Endpoint, LanPrinter, Pairing, pair, pair_with_issuers};
pub use self::listen::{
    CertificateSeen, ListenOptions, ListenTarget, ListenerState, StatusListener, Trust,
    listen_address_allowed, parse_listen_address, probe_certificate,
};

use std::io::ErrorKind;
use std::net::Ipv4Addr;
use std::sync::Mutex;
use std::time::Duration;

use self::tls::{CertificateProblem, Seen};
use crate::ident::Serial;

/// How long connecting, signing in and receiving the first report may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Why the LAN transport could not be used.
#[derive(Debug, Clone, thiserror::Error)]
pub enum LanError {
    /// The address is not one this version may connect to.
    #[error(
        "this version of Hotend does not connect to printers on the network ({0} is not a loopback address); use Bambu Connect or export to a USB drive"
    )]
    NotAllowed(Ipv4Addr),
    /// Nothing answers at the address.
    #[error(
        "the printer cannot be reached ({0}); check that it is switched on, on the same network, and in LAN Only mode"
    )]
    Unreachable(String),
    /// The certificate is not this printer's.
    #[error(
        "{0}. Nothing was sent. Check the serial number on the printer's screen and pair again only if you trust this network"
    )]
    Certificate(#[from] CertificateProblem),
    /// The secure connection failed for another reason.
    #[error("the secure connection to the printer failed ({0})")]
    Tls(String),
    /// The printer refused the access code.
    #[error("the printer did not accept the access code; read it again from the printer's screen")]
    AccessCodeRefused,
    /// The printer accepted the connection but sent no status.
    #[error(
        "the printer accepted the connection but sent no status; check that Developer Mode is on"
    )]
    NoReport,
    /// The profile's transport facts cannot be used.
    #[error("the printer profile's transport settings cannot be used: {0}")]
    Profile(String),
    /// The file with the certificate authorities cannot be used. Hotend does not ship
    /// Bambu Lab's certificates; the user supplies them.
    #[error(
        "the file with Bambu Lab's certificate authorities ({0}) cannot be read, holds no certificate, or holds one that is not Bambu Lab's. Nothing was sent. Hotend does not include that file; save Bambu Lab's printer certificate authorities as a PEM file and name it"
    )]
    NoIssuers(String),
    /// The status listener's address is not on the person's own network.
    #[error(
        "Hotend reads printer status only from an address on your own network (10.x.x.x, 172.16.x.x to 172.31.x.x or 192.168.x.x); {0} is not one. Nothing was connected to"
    )]
    NotPrivate(Ipv4Addr),
    /// The status listener's address is not an IPv4 address written as four numbers. The
    /// text is not repeated: it may be anything the person typed.
    #[error(
        "the printer's address must be an IPv4 address written as four numbers, such as 192.168.1.50; Hotend does not look up names. Nothing was connected to"
    )]
    NotAnAddress,
    /// The printer accepted the sign-in and refused the subscription to its reports.
    #[error(
        "the printer refused to send its status reports to Hotend (the subscription was refused)"
    )]
    SubscriptionRefused,
}

fn io_error(error: &std::io::Error, seen: &Mutex<Seen>) -> LanError {
    if let Some(problem) = seen.lock().ok().and_then(|seen| seen.problem.clone()) {
        return LanError::Certificate(problem);
    }
    match error.kind() {
        ErrorKind::InvalidData | ErrorKind::UnexpectedEof | ErrorKind::ConnectionAborted => {
            LanError::Tls(error.kind().to_string())
        }
        kind => LanError::Unreachable(kind.to_string()),
    }
}

fn topic(template: &str, serial: &Serial) -> Result<String, LanError> {
    if !template.contains("{serial}") || template.contains(['#', '+']) {
        return Err(LanError::Profile(
            "a topic must name the serial and no wildcard".to_owned(),
        ));
    }
    Ok(template.replace("{serial}", serial.as_str()))
}
