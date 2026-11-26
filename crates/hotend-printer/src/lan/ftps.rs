//! Upload of a checked job over implicit FTPS.
//!
//! Guarantees:
//! - the control and the data connection both use the pinned TLS configuration
//!   ([`super::tls`]), and the data connection goes to the address of the control
//!   connection, whatever address the server names;
//! - the bytes are hashed while they are sent; if they are not the checked job's bytes the
//!   remote file is removed and the upload fails;
//! - the remote name is the job's cleaned name, at the root of the storage;
//! - no error text contains what the server sent or the access code.
//!
//! Protocol facts: OpenBambuAPI `ftp.md` (implicit TLS, user `bblp`, the access code as
//! password).

use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use suppaftp::types::FileType;
use suppaftp::{FtpError, RustlsConnector, RustlsFtpStream, Status, TlsConnector};

use super::Endpoint;
use crate::job::{CheckedJob, hex};
use crate::printer::PrinterError;
use crate::secret::AccessCode;

/// How long one read or write on an FTPS connection may take.
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// The rustls connector with read and write timeouts on every stream it wraps.
#[derive(Debug)]
struct TimedConnector(RustlsConnector);

impl TlsConnector for TimedConnector {
    type Stream = <RustlsConnector as TlsConnector>::Stream;

    fn connect(&self, domain: &str, stream: TcpStream) -> suppaftp::FtpResult<Self::Stream> {
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
            .map_err(FtpError::ConnectionError)?;
        self.0.connect(domain, stream)
    }
}

/// Hashes what is read through it.
struct Hashing<R> {
    inner: R,
    hasher: Sha256,
    bytes: u64,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.hasher.update(&buffer[..read]);
        self.bytes += read as u64;
        Ok(read)
    }
}

/// An error for the user: the step and the kind of failure, never the server's text.
fn failed(step: &str, error: &FtpError) -> PrinterError {
    let why = match error {
        FtpError::ConnectionError(io) => format!("connection problem ({})", io.kind()),
        FtpError::SecureError(_) => "the secure connection was refused".to_owned(),
        FtpError::UnexpectedResponse(response) => {
            format!("the printer answered {}", response.status as u32)
        }
        FtpError::BadResponse => "the printer's answer could not be read".to_owned(),
        _ => "the transfer could not be set up".to_owned(),
    };
    PrinterError::Upload(format!("{step}: {why}"))
}

/// Sends `job` to the storage behind `endpoint` under its name on a printer.
pub(crate) fn upload(
    endpoint: Endpoint,
    config: Arc<rustls::ClientConfig>,
    username: &str,
    code: &AccessCode,
    job: &CheckedJob,
) -> Result<(), PrinterError> {
    let mut source = Hashing {
        inner: job.open_verified()?,
        hasher: Sha256::new(),
        bytes: 0,
    };
    let address = SocketAddr::from((endpoint.ip(), endpoint.ftps_port()));
    let host = endpoint.ip().to_string();
    let mut ftp = RustlsFtpStream::connect_secure_implicit(
        address,
        TimedConnector(RustlsConnector::from(config)),
        &host,
    )
    .map_err(|error| match &error {
        FtpError::ConnectionError(io) => PrinterError::Unreachable(io.kind().to_string()),
        _ => failed("connecting", &error),
    })?;
    // The data connection goes where the control connection went, not where the server
    // says, and gets the same timeouts.
    ftp.set_passive_nat_workaround(true);
    let allowed = endpoint.ip();
    let mut ftp = ftp.passive_stream_builder(move |address: SocketAddr| {
        if address.ip() != std::net::IpAddr::V4(allowed) {
            return Err(FtpError::ConnectionError(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            )));
        }
        TcpStream::connect_timeout(&address, IO_TIMEOUT).map_err(FtpError::ConnectionError)
    });

    ftp.login(username, code.expose()).map_err(|error| match &error {
        FtpError::UnexpectedResponse(response) if response.status == Status::NotLoggedIn => {
            PrinterError::Refused(
                "the printer did not accept the access code; read it again from the printer's screen"
                    .to_owned(),
            )
        }
        _ => failed("signing in", &error),
    })?;
    // Encrypt the data connection too (RFC 4217).
    ftp.custom_command("PBSZ 0", &[Status::CommandOk])
        .map_err(|error| failed("securing the transfer", &error))?;
    ftp.custom_command("PROT P", &[Status::CommandOk])
        .map_err(|error| failed("securing the transfer", &error))?;
    ftp.transfer_type(FileType::Binary)
        .map_err(|error| failed("preparing the transfer", &error))?;

    let name = job.remote_name();
    let sent = ftp.put_file(&name, &mut source);
    let hash = hex(&source.hasher.finalize());
    let intact = hash == job.sha256() && source.bytes == job.summary().size_bytes;
    // The size the printer stored, when it can say.
    let stored = ftp.size(&name).ok();
    let complete = stored.is_none_or(|size| size as u64 == source.bytes);
    let result = match sent {
        Err(error) => Err(failed("sending the file", &error)),
        Ok(_) if !intact => Err(PrinterError::Upload(
            "the file changed while it was being sent".to_owned(),
        )),
        Ok(_) if !complete => Err(PrinterError::Upload(
            "the printer stored a different number of bytes than were sent".to_owned(),
        )),
        Ok(_) => Ok(()),
    };
    if result.is_err() {
        // Nothing half-written or wrong stays startable under the job's name.
        let _ = ftp.rm(&name);
    }
    let _ = ftp.quit();
    result
}
