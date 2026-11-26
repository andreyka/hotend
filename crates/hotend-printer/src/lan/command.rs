//! The commanding LAN transport (feature `lan`, off by default and never enabled by the
//! application): [`LanPrinter`] sends commands over MQTT and files over implicit FTPS, and
//! connects to loopback addresses only ([`Endpoint`]). See the parent module documentation.

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rumqttc::{
    Client, ConnectReturnCode, ConnectionError, Event, MqttOptions, Packet, QoS, RecvTimeoutError,
    TlsConfiguration, Transport,
};
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;

use super::ftps;
use super::tls::{self, CertificateProblem, Expect, Issuers, Seen};
use super::{CONNECT_TIMEOUT, LanError, io_error, topic};
use crate::delivery::Capabilities;
use crate::ident::{PrinterIdentity, Serial};
use crate::job::CheckedJob;
use crate::printer::{Printer, PrinterError, UploadedJob};
use crate::profile::PrinterProfile;
use crate::protocol::command::{Command, FilamentSource, Qos};
use crate::protocol::report::{self, MAX_REPORT_BYTES};
use crate::secret::AccessCode;
use crate::status::PrinterStatus;

/// Where a printer listens. Loopback only in this version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    ip: Ipv4Addr,
    mqtt_port: u16,
    ftps_port: u16,
}

impl Endpoint {
    /// The printer at `ip` on the ports of `profile`.
    pub fn new(ip: Ipv4Addr, profile: &PrinterProfile) -> Result<Self, LanError> {
        let transport = &profile.family().transport;
        Self::with_ports(ip, transport.mqtt_port, transport.ftps_port)
    }

    /// The printer at `ip` on these ports (stand-in servers listen on free ports).
    pub fn with_ports(ip: Ipv4Addr, mqtt_port: u16, ftps_port: u16) -> Result<Self, LanError> {
        if !Self::allowed(ip) {
            return Err(LanError::NotAllowed(ip));
        }
        Ok(Self {
            ip,
            mqtt_port,
            ftps_port,
        })
    }

    /// The one place that decides which addresses may be connected to.
    fn allowed(ip: Ipv4Addr) -> bool {
        ip.is_loopback()
    }

    /// The address.
    pub fn ip(&self) -> Ipv4Addr {
        self.ip
    }

    /// The MQTT port.
    pub fn mqtt_port(&self) -> u16 {
        self.mqtt_port
    }

    /// The FTPS port.
    pub fn ftps_port(&self) -> u16 {
        self.ftps_port
    }
}

/// A printer the user paired: its serial and the certificate it showed then. Not a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pairing {
    serial: Serial,
    certificate_sha256: String,
}

impl Pairing {
    /// The serial number the user confirmed.
    pub fn serial(&self) -> &Serial {
        &self.serial
    }

    /// SHA-256 of the printer's certificate, lower-case hex.
    pub fn certificate_sha256(&self) -> &str {
        &self.certificate_sha256
    }
}

/// Pairs with the printer at `endpoint`, whose serial number the user read from the
/// printer and confirmed as `confirmed_serial`.
///
/// Only a TLS handshake is made; nothing is sent, the access code included. The printer
/// must present a certificate whose common name is the confirmed serial and that chains to
/// one of the certificate authorities in `authorities`, a PEM file the user supplies with
/// Bambu Lab's (Hotend does not ship them). Every certificate in it must be one of Bambu
/// Lab's known authorities ([`tls::BAMBU_LAB_CA_SHA256`]). Without such a file nothing is
/// connected to: [`LanError::NoIssuers`].
pub fn pair(
    endpoint: Endpoint,
    confirmed_serial: &Serial,
    authorities: &std::path::Path,
) -> Result<Pairing, LanError> {
    let issuers = Issuers::bambu_lab_from_file(authorities)
        .ok_or_else(|| LanError::NoIssuers(authorities.display().to_string()))?;
    pair_with_issuers(endpoint, confirmed_serial, &issuers)
}

/// [`pair`] with the certificate authorities already read: for the loopback stand-ins of
/// the tests, whose certificates a test authority signs.
pub fn pair_with_issuers(
    endpoint: Endpoint,
    confirmed_serial: &Serial,
    issuers: &Issuers,
) -> Result<Pairing, LanError> {
    let expect = Expect::Serial(confirmed_serial.clone(), issuers.clone());
    let (config, seen) =
        tls::client_config(expect).map_err(|error| LanError::Tls(error.to_string()))?;
    let address = SocketAddr::from((endpoint.ip, endpoint.mqtt_port));
    let mut socket = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
        .map_err(|error| LanError::Unreachable(error.kind().to_string()))?;
    socket
        .set_read_timeout(Some(CONNECT_TIMEOUT))
        .and_then(|()| socket.set_write_timeout(Some(CONNECT_TIMEOUT)))
        .map_err(|error| LanError::Unreachable(error.kind().to_string()))?;
    let mut connection =
        rustls::ClientConnection::new(config, ServerName::IpAddress(endpoint.ip.into()))
            .map_err(|error| LanError::Tls(error.to_string()))?;
    while connection.is_handshaking() {
        connection
            .complete_io(&mut socket)
            .map_err(|error| io_error(&error, &seen))?;
    }
    connection.send_close_notify();
    let _ = connection.complete_io(&mut socket);
    let fingerprint = seen
        .lock()
        .ok()
        .and_then(|seen| seen.fingerprint.clone())
        .ok_or(LanError::Certificate(CertificateProblem::Unreadable))?;
    Ok(Pairing {
        serial: confirmed_serial.clone(),
        certificate_sha256: fingerprint,
    })
}

/// What the receiving thread shares with the printer object.
#[derive(Debug, Default)]
struct Session {
    status: PrinterStatus,
    connected: bool,
    /// Why the session ended, in words for the user.
    ended: Option<String>,
    failure: Option<Failure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Failure {
    Certificate(CertificateProblem),
    AccessCode,
    Tls(String),
    Unreachable(String),
}

struct Shared {
    session: Mutex<Session>,
    changed: Condvar,
    stop: AtomicBool,
}

static CLIENTS: AtomicU64 = AtomicU64::new(0);

/// A printer reached over the local network. See the module documentation.
pub struct LanPrinter {
    identity: PrinterIdentity,
    profile: PrinterProfile,
    endpoint: Endpoint,
    code: AccessCode,
    tls: Arc<rustls::ClientConfig>,
    client: Client,
    shared: Arc<Shared>,
    request_topic: String,
    sequence: u64,
}

impl std::fmt::Debug for LanPrinter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanPrinter")
            .field("identity", &self.identity)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

fn classify(error: &ConnectionError, seen: &Mutex<Seen>) -> Failure {
    if let Some(problem) = seen.lock().ok().and_then(|seen| seen.problem.clone()) {
        return Failure::Certificate(problem);
    }
    match error {
        ConnectionError::ConnectionRefused(
            ConnectReturnCode::BadUserNamePassword | ConnectReturnCode::NotAuthorized,
        ) => Failure::AccessCode,
        ConnectionError::ConnectionRefused(_) => {
            Failure::Unreachable("the printer refused the connection".to_owned())
        }
        ConnectionError::Tls(_) => Failure::Tls("the handshake failed".to_owned()),
        ConnectionError::Io(io) => match io.kind() {
            ErrorKind::InvalidData => Failure::Tls("the handshake failed".to_owned()),
            kind => Failure::Unreachable(kind.to_string()),
        },
        ConnectionError::NetworkTimeout | ConnectionError::FlushTimeout => {
            Failure::Unreachable("timed out".to_owned())
        }
        _ => Failure::Unreachable("the connection broke".to_owned()),
    }
}

impl LanPrinter {
    /// Connects to a paired printer and waits for its first status report.
    ///
    /// `name` is the user's name for the printer. The access code is kept for uploads and
    /// wiped when the printer object is dropped.
    pub fn connect(
        endpoint: Endpoint,
        pairing: &Pairing,
        code: AccessCode,
        profile: PrinterProfile,
        name: &str,
    ) -> Result<Self, LanError> {
        let transport = profile.family().transport.clone();
        if transport.tls != "1.2" {
            return Err(LanError::Profile(format!(
                "TLS {} is not supported",
                transport.tls
            )));
        }
        let serial = pairing.serial.clone();
        let report_topic = topic(&transport.report_topic, &serial)?;
        let request_topic = topic(&transport.request_topic, &serial)?;
        let (tls, seen) = tls::client_config(Expect::Pinned(
            serial.clone(),
            pairing.certificate_sha256.clone(),
        ))
        .map_err(|error| LanError::Tls(error.to_string()))?;

        let id = format!(
            "hotend-{}-{}",
            std::process::id(),
            CLIENTS.fetch_add(1, Ordering::Relaxed)
        );
        let mut options = MqttOptions::new(id, endpoint.ip.to_string(), endpoint.mqtt_port);
        options
            .set_transport(Transport::Tls(TlsConfiguration::Rustls(Arc::clone(&tls))))
            .set_credentials(transport.username.clone(), code.expose())
            .set_keep_alive(Duration::from_secs(30))
            .set_max_packet_size(MAX_REPORT_BYTES + 4096, 64 * 1024)
            .set_clean_session(true);
        let (client, mut connection) = Client::new(options, 32);

        let shared = Arc::new(Shared {
            session: Mutex::new(Session::default()),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let worker = {
            let shared = Arc::clone(&shared);
            let client = client.clone();
            let profile = profile.clone();
            let pushall = Command::RequestStatus.to_wire(0, &profile).payload;
            let request_topic = request_topic.clone();
            move || {
                let update = |change: &mut dyn FnMut(&mut Session)| {
                    if let Ok(mut session) = shared.session.lock() {
                        change(&mut session);
                    }
                    shared.changed.notify_all();
                };
                let mut connected = false;
                let ended = loop {
                    if shared.stop.load(Ordering::SeqCst) {
                        break None;
                    }
                    // Until the session is up, one poll covers the whole connection attempt.
                    let wait = if connected {
                        Duration::from_millis(250)
                    } else {
                        CONNECT_TIMEOUT
                    };
                    match connection.recv_timeout(wait) {
                        Err(RecvTimeoutError::Timeout) if connected => {}
                        Err(RecvTimeoutError::Timeout) => {
                            break Some(Failure::Unreachable("timed out".to_owned()));
                        }
                        Err(RecvTimeoutError::Disconnected) => break None,
                        Ok(Ok(Event::Incoming(Packet::ConnAck(_)))) => {
                            connected = true;
                            let asked = client
                                .try_subscribe(report_topic.as_str(), QoS::AtMostOnce)
                                .and_then(|()| {
                                    client.try_publish(
                                        request_topic.as_str(),
                                        QoS::AtMostOnce,
                                        false,
                                        pushall.clone(),
                                    )
                                });
                            if asked.is_err() {
                                break Some(Failure::Unreachable(
                                    "the connection broke".to_owned(),
                                ));
                            }
                            update(&mut |session| session.connected = true);
                        }
                        Ok(Ok(Event::Incoming(Packet::Publish(publish))))
                            if publish.topic == report_topic =>
                        {
                            // Hostile input: a report that cannot be read is dropped.
                            if let Ok(message) = report::parse(&publish.payload)
                                && let Some(print) = message.print
                            {
                                update(&mut |session| session.status.apply(&print, &profile));
                            }
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => break Some(classify(&error, &seen)),
                    }
                };
                update(&mut |session| {
                    session.connected = false;
                    session.failure = ended.clone();
                    session.ended = Some(match &ended {
                        None => "the connection was closed".to_owned(),
                        Some(Failure::Certificate(problem)) => problem.to_string(),
                        Some(Failure::AccessCode) => "the access code was refused".to_owned(),
                        Some(Failure::Tls(why) | Failure::Unreachable(why)) => why.clone(),
                    });
                });
            }
        };
        std::thread::Builder::new()
            .name("hotend-lan-mqtt".to_owned())
            .spawn(worker)
            .map_err(|error| LanError::Unreachable(error.kind().to_string()))?;

        let printer = Self {
            identity: PrinterIdentity {
                serial,
                model: profile.id().to_owned(),
                name: report::clean_text(name),
            },
            profile,
            endpoint,
            code,
            tls,
            client,
            shared,
            request_topic,
            sequence: 0,
        };
        printer.wait_for_first_report()?;
        Ok(printer)
    }

    fn wait_for_first_report(&self) -> Result<(), LanError> {
        let started = Instant::now();
        let mut session = self
            .shared
            .session
            .lock()
            .map_err(|_| LanError::Unreachable("internal state lost".to_owned()))?;
        loop {
            if let Some(failure) = session.failure.clone() {
                return Err(match failure {
                    Failure::Certificate(problem) => LanError::Certificate(problem),
                    Failure::AccessCode => LanError::AccessCodeRefused,
                    Failure::Tls(why) => LanError::Tls(why),
                    Failure::Unreachable(why) => LanError::Unreachable(why),
                });
            }
            if session.ended.is_some() {
                return Err(LanError::Unreachable(
                    "the connection was closed".to_owned(),
                ));
            }
            if session.status.reports > 0 {
                return Ok(());
            }
            let left = CONNECT_TIMEOUT.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err(if session.connected {
                    LanError::NoReport
                } else {
                    LanError::Unreachable("timed out".to_owned())
                });
            }
            session = self
                .shared
                .changed
                .wait_timeout(session, left)
                .map_err(|_| LanError::Unreachable("internal state lost".to_owned()))?
                .0;
        }
    }

    /// Where the printer listens.
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// Waits until a report newer than `after` (a [`PrinterStatus::reports`] count) arrived,
    /// at most `timeout`. Returns the latest status either way.
    pub fn wait_for_report(
        &mut self,
        after: u64,
        timeout: Duration,
    ) -> Result<PrinterStatus, PrinterError> {
        let started = Instant::now();
        let lost = || PrinterError::Unreachable("internal state lost".to_owned());
        let mut session = self.shared.session.lock().map_err(|_| lost())?;
        loop {
            if !session.connected {
                return Err(PrinterError::Unreachable(
                    session
                        .ended
                        .clone()
                        .unwrap_or_else(|| "not connected".to_owned()),
                ));
            }
            let left = timeout.saturating_sub(started.elapsed());
            if session.status.reports > after || left.is_zero() {
                return Ok(session.status.clone());
            }
            session = self
                .shared
                .changed
                .wait_timeout(session, left)
                .map_err(|_| lost())?
                .0;
        }
    }

    fn connected(&self) -> Result<(), PrinterError> {
        let session = self
            .shared
            .session
            .lock()
            .map_err(|_| PrinterError::Unreachable("internal state lost".to_owned()))?;
        if session.connected {
            Ok(())
        } else {
            Err(PrinterError::Unreachable(
                session
                    .ended
                    .clone()
                    .unwrap_or_else(|| "not connected".to_owned()),
            ))
        }
    }
}

impl Drop for LanPrinter {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let _ = self.client.try_disconnect();
    }
}

impl Printer for LanPrinter {
    fn identity(&self) -> &PrinterIdentity {
        &self.identity
    }

    fn profile(&self) -> &PrinterProfile {
        &self.profile
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::lan()
    }

    fn status(&mut self) -> Result<PrinterStatus, PrinterError> {
        self.wait_for_report(0, Duration::ZERO)
    }

    fn upload(&mut self, job: &CheckedJob) -> Result<UploadedJob, PrinterError> {
        self.connected()?;
        ftps::upload(
            self.endpoint,
            Arc::clone(&self.tls),
            &self.profile.family().transport.username,
            &self.code,
            job,
        )?;
        Ok(UploadedJob::new(job, &self.identity))
    }

    fn send(&mut self, command: Command) -> Result<(), PrinterError> {
        self.connected()?;
        // How an AMS HT is named in the flat `ams_mapping` has not been tried on a printer
        // (the profile's `flat_ht`); a wrong value makes the printer pause, not start.
        if let Command::StartPreparedJob(start) = &command {
            let rules = &self.profile.family().ams;
            let from_ht = start.sources().iter().any(
                |source| matches!(source, FilamentSource::Ams { unit, .. } if rules.is_ht(*unit)),
            );
            if from_ht {
                return Err(PrinterError::NotSupported(
                    "starting a print that takes filament from an AMS HT is not proven on a \
                     real printer yet. Load the spool in a regular AMS or on the external \
                     holder, or send the file with Bambu Connect"
                        .to_owned(),
                ));
            }
        }
        self.sequence += 1;
        let wire = command.to_wire(self.sequence, &self.profile);
        let qos = match wire.qos {
            Qos::AtMostOnce => QoS::AtMostOnce,
            Qos::AtLeastOnce => QoS::AtLeastOnce,
        };
        self.client
            .try_publish(self.request_topic.as_str(), qos, false, wire.payload)
            .map_err(|_| PrinterError::Unreachable("the connection is busy or closed".to_owned()))
    }
}
