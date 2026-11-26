//! The read-only status listener: Hotend follows what a printer reports and never tells it
//! anything.
//!
//! Guarantees:
//! - **No code path publishes.** [`StatusListener`] has no publish method and holds no MQTT
//!   client that has one: the session is a small loop over a TLS stream, and the only
//!   function that writes MQTT packets ([`send`]) takes an [`Outgoing`], a closed list of
//!   four packets: CONNECT (the sign-in), SUBSCRIBE (to `device/<serial>/report` only),
//!   PINGREQ (keep-alive) and DISCONNECT. No `pushall` is asked for, so the first view may
//!   be partial until the printer's next complete report; partial reports are merged
//!   ([`PrinterStatus::apply`]). A report the printer sends with QoS 1 is not acknowledged
//!   (a PUBACK would be a fifth packet); the subscription asks for QoS 0.
//! - **One address, on the person's own network.** A [`ListenTarget`] is an IPv4 address in
//!   a private range (10/8, 172.16/12, 192.168/16) or loopback (the tests' stand-in
//!   broker). Public addresses are refused, and names are never looked up: an address is
//!   written as four numbers ([`parse_listen_address`], which also refuses loopback, is for
//!   what the person types).
//! - **Never unchecked TLS.** The printer's certificate must name the configured serial and
//!   either chain to the certificate authorities in a file the person supplies
//!   ([`Trust::Authorities`]; Hotend does not ship Bambu Lab's: see [`super::tls`]) or be
//!   the very certificate the person confirmed earlier ([`Trust::Pinned`], SHA-256 of the
//!   DER certificate). The handshake completes before the sign-in is written, so the access
//!   code is never sent to a device whose certificate was refused.
//! - **A first look sends nothing.** [`probe_certificate`] makes a TLS handshake only and
//!   returns the certificate's fingerprint and subject for the person to confirm. Storing
//!   that confirmation is the caller's business (the core does it on a separate call).
//! - The access code is held as an [`AccessCode`] inside the session thread, written once
//!   into the CONNECT packet and wiped from that packet and its buffer right after; it is
//!   never part of a state, an error or a `Debug` output.
//! - A session that ends is not re-opened: the state says why it ended, and a new
//!   [`StatusListener::connect`] is needed. Incoming packets are capped at the report
//!   parser's limit plus a small header allowance; reports are parsed as hostile input.
//!
//! ```compile_fail
//! // The listener has no way to send a message to the printer.
//! fn tell(listener: &hotend_printer::lan::StatusListener) {
//!     listener.publish("device/X/request", b"{}");
//! }
//! ```
//!
//! What the certificate's subject proves: nothing on its own. Its common name is the serial
//! number, which the printer announces on the network, so any device can make a certificate
//! that names it. With the authorities file the chain proves Bambu Lab issued it; with a pin
//! the fingerprint proves only that it is the certificate the person confirmed the first
//! time (trust on first use), so the first confirmation should be made on a network the
//! person trusts.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use rumqttc::mqttbytes;
use rumqttc::{Connect, ConnectReturnCode, Packet, QoS, Subscribe, SubscribeReasonCode};
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};
use serde::Serialize;
use zeroize::Zeroize;

use super::tls::{self, CertificateProblem, Expect, Issuers, Seen};
use super::{CONNECT_TIMEOUT, LanError};
use crate::ident::Serial;
use crate::profile::PrinterProfile;
use crate::protocol::report::{self, MAX_REPORT_BYTES};
use crate::secret::AccessCode;
use crate::status::PrinterStatus;

/// Largest MQTT packet read: the report parser's limit and room for the topic and header.
const MAX_PACKET_BYTES: usize = MAX_REPORT_BYTES + 4096;
/// How long one read waits before the loop looks at its stop flag and its keep-alive.
const POLL: Duration = Duration::from_millis(200);

static CLIENTS: AtomicU64 = AtomicU64::new(0);

/// Whether the status listener may connect to `ip`: an address in a private IPv4 range, or
/// loopback (where the tests run their stand-in broker). Everything else is refused.
pub fn listen_address_allowed(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback()
}

/// The printer's address as the person typed it: an IPv4 address written as four numbers,
/// in a private range. Names are not looked up, and loopback is refused (a printer is not
/// this computer).
pub fn parse_listen_address(text: &str) -> Result<Ipv4Addr, LanError> {
    let ip: Ipv4Addr = text.trim().parse().map_err(|_| LanError::NotAnAddress)?;
    if !ip.is_private() {
        return Err(LanError::NotPrivate(ip));
    }
    Ok(ip)
}

/// The one printer the listener connects to: address, MQTT port and serial number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenTarget {
    ip: Ipv4Addr,
    port: u16,
    serial: Serial,
}

impl ListenTarget {
    /// The printer at `ip` with this serial, on the MQTT port of `profile`.
    pub fn new(ip: Ipv4Addr, serial: Serial, profile: &PrinterProfile) -> Result<Self, LanError> {
        Self::with_port(ip, profile.family().transport.mqtt_port, serial)
    }

    /// The printer at `ip` on `port` (a stand-in broker listens on a free port).
    pub fn with_port(ip: Ipv4Addr, port: u16, serial: Serial) -> Result<Self, LanError> {
        if !listen_address_allowed(ip) {
            return Err(LanError::NotPrivate(ip));
        }
        Ok(Self { ip, port, serial })
    }

    /// The address.
    pub fn ip(&self) -> Ipv4Addr {
        self.ip
    }

    /// The MQTT port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The serial number the certificate must name.
    pub fn serial(&self) -> &Serial {
        &self.serial
    }
}

/// How the printer's certificate is checked. There is no unchecked mode.
#[derive(Debug, Clone)]
pub enum Trust {
    /// It must chain to one of these authorities (for a real printer: Bambu Lab's, from the
    /// file the person supplies) and name the serial.
    Authorities(Issuers),
    /// It must be the certificate with this SHA-256 (lower-case hex of the DER form), the
    /// one the person confirmed, and name the serial.
    Pinned(String),
}

/// What a first look at the printer's certificate found, for the person to confirm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CertificateSeen {
    /// SHA-256 of the DER certificate, lower-case hex: what a pin stores.
    pub sha256: String,
    /// The subject as text (`CN=<serial>`, ...), cleaned. See the module documentation for
    /// what it can and cannot prove.
    pub subject: String,
    /// The issuer as text, cleaned. Not checked here.
    pub issuer: String,
    /// The serial the certificate names (it was checked to be the configured one).
    pub serial: String,
}

/// How the session is kept alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenOptions {
    /// The MQTT keep-alive; a PINGREQ is sent every half of it, and a printer silent for
    /// twice as long is taken as gone.
    pub keep_alive: Duration,
}

impl Default for ListenOptions {
    fn default() -> Self {
        Self {
            keep_alive: Duration::from_secs(30),
        }
    }
}

/// The listener's state: the merged status and whether the session is up.
#[derive(Debug, Clone, Default)]
pub struct ListenerState {
    /// Everything the printer reported, merged (partial reports included).
    pub status: PrinterStatus,
    /// Signed in and subscribed.
    pub connected: bool,
    /// Why the session ended, in words for the person; `None` while it runs.
    pub ended: Option<String>,
    /// Reports that could not be read (too large, not JSON) and were dropped.
    pub dropped_reports: u64,
    failure: Option<LanError>,
}

impl ListenerState {
    /// How the session ended, when it ended with an error.
    pub fn failure(&self) -> Option<&LanError> {
        self.failure.as_ref()
    }
}

struct Shared {
    state: Mutex<ListenerState>,
    changed: Condvar,
    stop: AtomicBool,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, ListenerState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn update(&self, change: impl FnOnce(&mut ListenerState)) {
        change(&mut self.state());
        self.changed.notify_all();
    }
}

/// A read-only session with one printer. See the module documentation.
pub struct StatusListener {
    target: ListenTarget,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for StatusListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatusListener")
            .field("target", &self.target)
            .field("connected", &self.shared.state().connected)
            .finish_non_exhaustive()
    }
}

fn unreachable(error: &std::io::Error) -> LanError {
    LanError::Unreachable(error.kind().to_string())
}

/// Takes one look at the printer's certificate: a TLS handshake, closed at once. Nothing
/// is sent over it, the access code included. The certificate must name the target's
/// serial ([`CertificateProblem::WrongSerial`] otherwise); whoever issued it, its
/// fingerprint and subject are returned for the person to confirm.
pub fn probe_certificate(target: &ListenTarget) -> Result<CertificateSeen, LanError> {
    let (config, seen) = tls::client_config(Expect::Probe(target.serial.clone()))
        .map_err(|error| LanError::Tls(error.to_string()))?;
    let address = SocketAddr::from((target.ip, target.port));
    let mut socket =
        TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).map_err(|e| unreachable(&e))?;
    socket
        .set_read_timeout(Some(CONNECT_TIMEOUT))
        .and_then(|()| socket.set_write_timeout(Some(CONNECT_TIMEOUT)))
        .map_err(|e| unreachable(&e))?;
    let mut connection = ClientConnection::new(config, ServerName::IpAddress(target.ip.into()))
        .map_err(|error| LanError::Tls(error.to_string()))?;
    while connection.is_handshaking() {
        connection
            .complete_io(&mut socket)
            .map_err(|error| super::io_error(&error, &seen))?;
    }
    connection.send_close_notify();
    let _ = connection.complete_io(&mut socket);
    let seen = seen.lock().unwrap_or_else(|p| p.into_inner());
    let sha256 = seen
        .fingerprint
        .clone()
        .ok_or(LanError::Certificate(CertificateProblem::Unreadable))?;
    Ok(CertificateSeen {
        sha256,
        subject: seen.subject.clone().unwrap_or_default(),
        issuer: seen.issuer.clone().unwrap_or_default(),
        serial: target.serial.as_str().to_owned(),
    })
}

impl StatusListener {
    /// Connects to the printer, signs in with `code` and subscribes to its report topic.
    /// Returns once the subscription is acknowledged; reports then arrive as the printer
    /// sends them ([`StatusListener::wait_for_update`]).
    pub fn connect(
        target: ListenTarget,
        trust: Trust,
        code: AccessCode,
        profile: PrinterProfile,
        options: ListenOptions,
    ) -> Result<Self, LanError> {
        let transport = profile.family().transport.clone();
        if transport.tls != "1.2" {
            return Err(LanError::Profile(format!(
                "TLS {} is not supported",
                transport.tls
            )));
        }
        let report_topic = super::topic(&transport.report_topic, &target.serial)?;
        let expect = match trust {
            Trust::Authorities(issuers) => Expect::Serial(target.serial.clone(), issuers),
            Trust::Pinned(sha256) => {
                let sha256 = sha256.trim().to_ascii_lowercase();
                if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(LanError::Tls(
                        "the confirmed certificate fingerprint is not a SHA-256 in hex".to_owned(),
                    ));
                }
                Expect::Pinned(target.serial.clone(), sha256)
            }
        };
        let (config, seen) =
            tls::client_config(expect).map_err(|error| LanError::Tls(error.to_string()))?;
        let shared = Arc::new(Shared {
            state: Mutex::new(ListenerState::default()),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let keep_alive = options
            .keep_alive
            .clamp(Duration::from_secs(1), Duration::from_secs(600));
        let session = Session {
            target: target.clone(),
            config,
            seen,
            code,
            username: transport.username,
            report_topic,
            profile,
            keep_alive,
            shared: Arc::clone(&shared),
        };
        let thread = std::thread::Builder::new()
            .name("hotend-lan-status".to_owned())
            .spawn(move || session.run())
            .map_err(|error| LanError::Unreachable(error.kind().to_string()))?;
        let listener = Self {
            target,
            shared,
            thread: Some(thread),
        };
        listener.wait_until_subscribed()?;
        Ok(listener)
    }

    fn wait_until_subscribed(&self) -> Result<(), LanError> {
        // The session gives up after CONNECT_TIMEOUT itself; the margin covers its last poll.
        let deadline = Instant::now() + CONNECT_TIMEOUT + Duration::from_secs(2);
        let mut state = self.shared.state();
        loop {
            if let Some(failure) = &state.failure {
                return Err(failure.clone());
            }
            if state.ended.is_some() {
                return Err(LanError::Unreachable(
                    "the connection was closed".to_owned(),
                ));
            }
            if state.connected {
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(LanError::Unreachable("timed out".to_owned()));
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// The printer this listener follows.
    pub fn target(&self) -> &ListenTarget {
        &self.target
    }

    /// The state now.
    pub fn state(&self) -> ListenerState {
        self.shared.state().clone()
    }

    /// Waits until a report newer than `after` (a [`PrinterStatus::reports`] count) was
    /// merged, the session ended, or `timeout` passed; returns the state then.
    pub fn wait_for_update(&self, after: u64, timeout: Duration) -> ListenerState {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.state();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if state.status.reports > after || state.ended.is_some() || left.is_zero() {
                return state.clone();
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// Ends the session (DISCONNECT) and waits for its thread.
    pub fn close(self) {}
}

impl Drop for StatusListener {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The only packets Hotend ever sends on a status session.
enum Outgoing<'a> {
    /// The sign-in.
    Connect {
        client_id: &'a str,
        username: &'a str,
        code: &'a AccessCode,
        keep_alive: Duration,
    },
    /// The subscription to the one report topic.
    Subscribe { topic: &'a str },
    /// Keep-alive.
    PingReq,
    /// The end of the session.
    Disconnect,
}

/// Writes one packet. The single place a status session writes MQTT.
fn send(stream: &mut impl Write, outgoing: Outgoing<'_>) -> std::io::Result<()> {
    let mut packet = match outgoing {
        Outgoing::Connect {
            client_id,
            username,
            code,
            keep_alive,
        } => {
            let mut connect = Connect::new(client_id);
            connect.keep_alive = u16::try_from(keep_alive.as_secs()).unwrap_or(u16::MAX);
            connect.clean_session = true;
            connect.set_login(username, code.expose());
            Packet::Connect(connect)
        }
        Outgoing::Subscribe { topic } => {
            let mut subscribe = Subscribe::new(topic, QoS::AtMostOnce);
            // MQTT forbids packet id 0 on SUBSCRIBE.
            subscribe.pkid = 1;
            Packet::Subscribe(subscribe)
        }
        Outgoing::PingReq => Packet::PingReq,
        Outgoing::Disconnect => Packet::Disconnect,
    };
    let mut out = BytesMut::new();
    let written = packet
        .write(&mut out, MAX_PACKET_BYTES)
        .map_err(|_| std::io::Error::other("a packet could not be encoded"));
    let sent = written.and_then(|_| stream.write_all(&out).and_then(|()| stream.flush()));
    // The sign-in carries the access code: wipe both copies of it.
    if let Packet::Connect(connect) = &mut packet
        && let Some(login) = connect.login.as_mut()
    {
        login.password.zeroize();
    }
    out[..].zeroize();
    sent
}

/// One session, run on its own thread.
struct Session {
    target: ListenTarget,
    config: Arc<rustls::ClientConfig>,
    seen: Arc<Mutex<Seen>>,
    code: AccessCode,
    username: String,
    report_topic: String,
    profile: PrinterProfile,
    keep_alive: Duration,
    shared: Arc<Shared>,
}

impl Session {
    fn run(self) {
        let outcome = self.session();
        self.shared.update(|state| {
            state.connected = false;
            state.ended = Some(match &outcome {
                Ok(()) => "the connection was closed".to_owned(),
                Err(error) => error.to_string(),
            });
            state.failure = outcome.err();
        });
    }

    fn stopping(&self) -> bool {
        self.shared.stop.load(Ordering::SeqCst)
    }

    fn session(&self) -> Result<(), LanError> {
        let address = SocketAddr::from((self.target.ip, self.target.port));
        let socket =
            TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).map_err(|e| unreachable(&e))?;
        socket
            .set_read_timeout(Some(CONNECT_TIMEOUT))
            .and_then(|()| socket.set_write_timeout(Some(CONNECT_TIMEOUT)))
            .map_err(|e| unreachable(&e))?;
        let connection = ClientConnection::new(
            Arc::clone(&self.config),
            ServerName::IpAddress(self.target.ip.into()),
        )
        .map_err(|error| LanError::Tls(error.to_string()))?;
        let mut stream = StreamOwned::new(connection, socket);
        // The handshake completes, and the certificate is accepted, before the sign-in is
        // written: a device whose certificate is refused never sees the access code.
        while stream.conn.is_handshaking() {
            stream
                .conn
                .complete_io(&mut stream.sock)
                .map_err(|error| super::io_error(&error, &self.seen))?;
        }
        stream
            .sock
            .set_read_timeout(Some(POLL))
            .map_err(|e| unreachable(&e))?;
        let client_id = format!(
            "hotend-status-{}-{}",
            std::process::id(),
            CLIENTS.fetch_add(1, Ordering::Relaxed)
        );
        send(
            &mut stream,
            Outgoing::Connect {
                client_id: &client_id,
                username: &self.username,
                code: &self.code,
                keep_alive: self.keep_alive,
            },
        )
        .map_err(|e| unreachable(&e))?;

        let started = Instant::now();
        let ping_every = self.keep_alive / 2;
        let mut last_sent = Instant::now();
        let mut last_heard = Instant::now();
        let mut subscribed = false;
        let mut buffer = BytesMut::new();
        let mut chunk = vec![0u8; 16 * 1024];
        loop {
            if self.stopping() {
                let _ = send(&mut stream, Outgoing::Disconnect);
                stream.conn.send_close_notify();
                let _ = stream.conn.complete_io(&mut stream.sock);
                return Ok(());
            }
            if !subscribed && started.elapsed() > CONNECT_TIMEOUT {
                return Err(LanError::Unreachable("timed out".to_owned()));
            }
            if subscribed && last_heard.elapsed() > self.keep_alive * 2 {
                return Err(LanError::Unreachable(
                    "the printer stopped answering".to_owned(),
                ));
            }
            if subscribed && last_sent.elapsed() >= ping_every {
                send(&mut stream, Outgoing::PingReq).map_err(|e| unreachable(&e))?;
                last_sent = Instant::now();
            }
            let packet = match Packet::read(&mut buffer, MAX_PACKET_BYTES) {
                Ok(packet) => packet,
                Err(mqttbytes::Error::InsufficientBytes(_)) => {
                    match stream.read(&mut chunk) {
                        Ok(0) => {
                            return Err(LanError::Unreachable(
                                "the printer closed the connection".to_owned(),
                            ));
                        }
                        Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                        Err(error)
                            if matches!(
                                error.kind(),
                                ErrorKind::WouldBlock | ErrorKind::TimedOut
                            ) => {}
                        Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
                            return Err(LanError::Unreachable(
                                "the printer closed the connection".to_owned(),
                            ));
                        }
                        Err(error) => return Err(super::io_error(&error, &self.seen)),
                    }
                    continue;
                }
                Err(_) => {
                    return Err(LanError::Unreachable(
                        "the printer sent a message Hotend cannot read".to_owned(),
                    ));
                }
            };
            last_heard = Instant::now();
            match packet {
                Packet::ConnAck(ack) => match ack.code {
                    ConnectReturnCode::Success => {
                        send(
                            &mut stream,
                            Outgoing::Subscribe {
                                topic: &self.report_topic,
                            },
                        )
                        .map_err(|e| unreachable(&e))?;
                        last_sent = Instant::now();
                    }
                    ConnectReturnCode::BadUserNamePassword | ConnectReturnCode::NotAuthorized => {
                        return Err(LanError::AccessCodeRefused);
                    }
                    _ => {
                        return Err(LanError::Unreachable(
                            "the printer refused the connection".to_owned(),
                        ));
                    }
                },
                Packet::SubAck(ack) => {
                    if ack.return_codes.is_empty()
                        || ack
                            .return_codes
                            .iter()
                            .any(|code| matches!(code, SubscribeReasonCode::Failure))
                    {
                        return Err(LanError::SubscriptionRefused);
                    }
                    subscribed = true;
                    self.shared.update(|state| state.connected = true);
                }
                Packet::Publish(publish) if publish.topic == self.report_topic => {
                    // Hostile input: a report that cannot be read is dropped and counted.
                    match report::parse(&publish.payload) {
                        Ok(message) => {
                            if let Some(print) = message.print {
                                self.shared
                                    .update(|state| state.status.apply(&print, &self.profile));
                            }
                        }
                        Err(_) => self.shared.update(|state| {
                            state.dropped_reports = state.dropped_reports.saturating_add(1);
                        }),
                    }
                }
                // Messages on other topics, ping answers and anything else carry nothing.
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_listener_source_builds_no_packet_but_the_four() {
        // Patterns are built so that this test does not match itself.
        let source = include_str!("listen.rs");
        // The code before the tests, without comments (the documentation shows a call
        // that does not compile).
        let code: String = source[..source.find("#[cfg(test)]").unwrap()]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let code = code.as_str();
        for forbidden in [
            concat!("Packet::", "Publish("),
            concat!("Publish", "::new"),
            concat!("Packet::", "PubAck"),
            concat!("Packet::", "PubRec"),
            concat!("Packet::", "PubRel"),
            concat!("Packet::", "PubComp"),
            concat!("Packet::", "Unsubscribe"),
            concat!(".", "publish("),
            concat!("try_", "publish"),
            concat!("rumqttc::", "Client"),
            concat!("Event", "Loop"),
        ] {
            // `Packet::Publish(publish) if ...` is the one read of an incoming report.
            let hits = code.matches(forbidden).count();
            let allowed = usize::from(forbidden == concat!("Packet::", "Publish("));
            assert_eq!(hits, allowed, "{forbidden}");
        }
        // The one incoming match on a report is a read, not a construction.
        assert!(code.contains(concat!("Packet::", "Publish(publish) if publish.topic")));
        // Every packet written goes through `send`, whose input is the closed list.
        assert_eq!(code.matches(concat!(".write(&mut ", "out")).count(), 1);
    }

    #[test]
    fn only_private_ranges_and_loopback_are_allowed() {
        for ip in ["192.168.1.50", "10.0.0.7", "172.16.0.1", "172.31.255.254"] {
            let ip: Ipv4Addr = ip.parse().unwrap();
            assert!(listen_address_allowed(ip), "{ip}");
            assert_eq!(parse_listen_address(&ip.to_string()).unwrap(), ip);
        }
        assert!(listen_address_allowed(Ipv4Addr::LOCALHOST));
        for ip in [
            "8.8.8.8",
            "172.32.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.251",
        ] {
            let ip: Ipv4Addr = ip.parse().unwrap();
            assert!(!listen_address_allowed(ip), "{ip}");
            assert!(
                matches!(parse_listen_address(&ip.to_string()), Err(LanError::NotPrivate(r)) if r == ip)
            );
        }
        // Loopback is for the tests' stand-in, never a printer the person names.
        assert!(matches!(
            parse_listen_address("127.0.0.1"),
            Err(LanError::NotPrivate(_))
        ));
        for name in [
            "printer.local",
            "localhost",
            "example.com",
            "::1",
            "fe80::1",
            "10.0.0.5:8883",
            "10.0.5",
            "10.000.000.005",
            "",
        ] {
            let error = parse_listen_address(name).unwrap_err();
            assert!(matches!(error, LanError::NotAnAddress), "{name}");
            assert!(name.is_empty() || !error.to_string().contains(name));
        }
    }
}
