//! A stand-in printer for the `lan` tests: an MQTT broker and an implicit FTPS server on
//! loopback, inside the test process, built from the crates the transport itself uses
//! (rustls, rumqttc's packet codec). It speaks just enough of both protocols for the
//! transport and records everything it is sent.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::BytesMut;
use rumqttc::mqttbytes;
use rumqttc::{
    ConnAck, ConnectReturnCode, Packet, PubAck, Publish, QoS, SubAck, SubscribeReasonCode,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::{Value, json};

/// The access code the stand-in accepts. Not a secret: it exists only in this test process.
pub const ACCESS_CODE: &str = "t3stc0de";

/// How the stand-in misbehaves, when a test asks it to.
#[derive(Debug, Default, Clone)]
pub struct Quirks {
    /// PASV names another address than the one the client connected to.
    pub pasv_names_other_address: bool,
    /// The server stores only this many bytes of an upload.
    pub store_at_most: Option<usize>,
    /// The server does not know SIZE.
    pub no_size: bool,
    /// Only TLS 1.3 is offered.
    pub tls13_only: bool,
    /// The start command is answered with a report that the printer stays idle.
    pub ignore_start: bool,
    /// The certificate is signed by its own key, not by the stand-in authority: what any
    /// device on the network can make for a serial number it heard announced.
    pub self_signed: bool,
}

#[derive(Debug, Default)]
pub struct Seen {
    /// (user, password) of every MQTT CONNECT.
    pub mqtt_logins: Vec<(String, String)>,
    /// Topics subscribed to.
    pub subscriptions: Vec<String>,
    /// The kind of every MQTT packet received, in order (`Connect`, `Subscribe`, ...).
    pub packets: Vec<&'static str>,
    /// (topic, qos, payload) of every publish received.
    pub requests: Vec<(String, u8, String)>,
    /// Every FTP command line, the password replaced.
    pub ftp_commands: Vec<String>,
    /// Passwords given to FTP.
    pub ftp_passwords: Vec<String>,
    /// Files on the stand-in's storage.
    pub files: BTreeMap<String, Vec<u8>>,
    /// Whether a data connection was encrypted.
    pub data_tls: Vec<bool>,
    /// Payloads to publish on the report topic as soon as possible.
    pub outbox: Vec<Vec<u8>>,
    /// Close every MQTT connection now.
    pub kill_mqtt: bool,
    /// The printer's state as the stand-in reports it.
    pub gcode_state: String,
    pub job_name: String,
}

pub struct StandIn {
    pub serial: String,
    pub mqtt_port: u16,
    pub ftps_port: u16,
    pub certificate: Vec<u8>,
    pub seen: Arc<Mutex<Seen>>,
}

/// The certificate authority that stands in for Bambu Lab's in these tests: its
/// certificate as PEM, and what signs with it.
fn authority() -> &'static (String, rcgen::Issuer<'static, rcgen::KeyPair>) {
    static AUTHORITY: std::sync::OnceLock<(String, rcgen::Issuer<'static, rcgen::KeyPair>)> =
        std::sync::OnceLock::new();
    AUTHORITY.get_or_init(|| {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Stand-in printer CA");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ];
        let pem = params.self_signed(&key).unwrap().pem();
        (pem, rcgen::Issuer::new(params, key))
    })
}

/// The stand-in authority's certificate, for `Issuers::from_pem`.
pub fn authority_pem() -> &'static str {
    &authority().0
}

/// The certificate of an authority that signed none of the stand-in's certificates: what a
/// file of the wrong authorities holds.
pub fn other_authority_pem() -> String {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Another printer CA");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    params.self_signed(&key).unwrap().pem()
}

fn server_config(
    common_name: &str,
    tls13_only: bool,
    self_signed: bool,
) -> (Arc<ServerConfig>, Vec<u8>) {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, common_name);
    let certificate = if self_signed {
        params.self_signed(&key).unwrap()
    } else {
        params.signed_by(&key, &authority().1).unwrap()
    };
    let der: CertificateDer<'static> = certificate.der().clone();
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = ServerConfig::builder_with_provider(provider);
    let builder = if tls13_only {
        builder
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
    } else {
        builder.with_safe_default_protocol_versions().unwrap()
    };
    let config = builder
        .with_no_client_auth()
        .with_single_cert(
            vec![der.clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap();
    (Arc::new(config), der.to_vec())
}

fn status_report(seen: &Seen) -> Vec<u8> {
    json!({"print": {
        "command": "push_status",
        "gcode_state": seen.gcode_state,
        "stg_cur": if seen.gcode_state == "IDLE" { 255 } else { 1 },
        "mc_percent": 0,
        "subtask_name": seen.job_name,
        "print_error": 0,
        "hms": [],
        "sdcard": true,
        "device": {"extruder": {"info": [{"id": 0, "temp": 25}, {"id": 1, "temp": 25}], "state": 0x12}},
        "ams": {"ams": [{
            "id": "0", "info": "103", "humidity": "4", "temp": "24.0", "dry_time": 0,
            "tray": [
                {"id": "0", "tray_type": "PLA", "tray_sub_brands": "PLA Basic", "tray_color": "FFFFFFFF", "remain": 80},
                {"id": "1"}
            ]
        }, {
            // An AMS HT that feeds the main nozzle (kind 4, nozzle 1).
            "id": "128", "info": "104", "humidity": "2", "temp": "30.0", "dry_time": 0,
            "tray": [
                {"id": "0", "tray_type": "PLA", "tray_sub_brands": "PLA Basic", "tray_color": "000000FF", "remain": 60}
            ]
        }]},
        "a_field_from_newer_firmware": {"nested": [1, 2, 3]},
    }})
    .to_string()
    .into_bytes()
}

fn send(stream: &mut impl Write, packet: Packet) -> bool {
    let mut out = BytesMut::new();
    packet.write(&mut out, 4 * 1024 * 1024).is_ok() && stream.write_all(&out).is_ok()
}

/// The name of an MQTT packet's type.
pub fn packet_kind(packet: &Packet) -> &'static str {
    match packet {
        Packet::Connect(_) => "Connect",
        Packet::ConnAck(_) => "ConnAck",
        Packet::Publish(_) => "Publish",
        Packet::PubAck(_) => "PubAck",
        Packet::PubRec(_) => "PubRec",
        Packet::PubRel(_) => "PubRel",
        Packet::PubComp(_) => "PubComp",
        Packet::Subscribe(_) => "Subscribe",
        Packet::SubAck(_) => "SubAck",
        Packet::Unsubscribe(_) => "Unsubscribe",
        Packet::UnsubAck(_) => "UnsubAck",
        Packet::PingReq => "PingReq",
        Packet::PingResp => "PingResp",
        Packet::Disconnect => "Disconnect",
    }
}

fn serve_mqtt(
    socket: TcpStream,
    tls: Arc<ServerConfig>,
    serial: String,
    quirks: Quirks,
    seen: Arc<Mutex<Seen>>,
) {
    socket
        .set_read_timeout(Some(Duration::from_millis(25)))
        .unwrap();
    let Ok(connection) = ServerConnection::new(tls) else {
        return;
    };
    let mut stream = StreamOwned::new(connection, socket);
    let report_topic = format!("device/{serial}/report");
    let request_topic = format!("device/{serial}/request");
    let mut buffer = BytesMut::new();
    let mut chunk = [0u8; 8192];
    let mut subscribed = false;
    loop {
        if seen.lock().unwrap().kill_mqtt {
            return;
        }
        if subscribed {
            let outbox = std::mem::take(&mut seen.lock().unwrap().outbox);
            for payload in outbox {
                if !send(
                    &mut stream,
                    Packet::Publish(Publish::new(&report_topic, QoS::AtMostOnce, payload)),
                ) {
                    return;
                }
            }
        }
        let packet = match Packet::read(&mut buffer, 1024 * 1024) {
            Ok(packet) => packet,
            Err(mqttbytes::Error::InsufficientBytes(_)) => {
                match stream.read(&mut chunk) {
                    Ok(0) => return,
                    Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                    Err(error)
                        if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    Err(_) => return,
                }
                continue;
            }
            Err(_) => return,
        };
        seen.lock().unwrap().packets.push(packet_kind(&packet));
        match packet {
            Packet::Connect(connect) => {
                let login = connect
                    .login
                    .map(|login| (login.username, login.password))
                    .unwrap_or_default();
                let good = login.0 == "bblp" && login.1 == ACCESS_CODE;
                seen.lock().unwrap().mqtt_logins.push(login);
                let code = if good {
                    ConnectReturnCode::Success
                } else {
                    ConnectReturnCode::BadUserNamePassword
                };
                if !send(&mut stream, Packet::ConnAck(ConnAck::new(code, false))) || !good {
                    return;
                }
            }
            Packet::Subscribe(subscribe) => {
                let codes = subscribe
                    .filters
                    .iter()
                    .map(|_| SubscribeReasonCode::Success(QoS::AtMostOnce))
                    .collect();
                seen.lock()
                    .unwrap()
                    .subscriptions
                    .extend(subscribe.filters.iter().map(|f| f.path.clone()));
                subscribed = subscribe.filters.iter().any(|f| f.path == report_topic);
                if !send(
                    &mut stream,
                    Packet::SubAck(SubAck::new(subscribe.pkid, codes)),
                ) {
                    return;
                }
            }
            Packet::Publish(publish) => {
                let payload = String::from_utf8_lossy(&publish.payload).into_owned();
                let command: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
                let mut guard = seen.lock().unwrap();
                guard
                    .requests
                    .push((publish.topic.clone(), publish.qos as u8, payload));
                if publish.topic == request_topic {
                    let name = |block: &str| {
                        command[block]["command"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned()
                    };
                    match (name("print").as_str(), name("pushing").as_str()) {
                        ("project_file", _) if !quirks.ignore_start => {
                            guard.gcode_state = "PREPARE".to_owned();
                            guard.job_name = command["print"]["subtask_name"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned();
                        }
                        ("pause", _) => guard.gcode_state = "PAUSE".to_owned(),
                        ("resume", _) => guard.gcode_state = "RUNNING".to_owned(),
                        ("stop", _) => guard.gcode_state = "FAILED".to_owned(),
                        _ => {}
                    }
                    let report = status_report(&guard);
                    guard.outbox.push(report);
                }
                drop(guard);
                if publish.qos == QoS::AtLeastOnce
                    && !send(&mut stream, Packet::PubAck(PubAck::new(publish.pkid)))
                {
                    return;
                }
            }
            Packet::PingReq => {
                if !send(&mut stream, Packet::PingResp) {
                    return;
                }
            }
            Packet::Disconnect => return,
            _ => {}
        }
    }
}

fn read_line(stream: &mut impl Read) -> Option<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return None,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => line.push(byte[0]),
            Err(_) => return None,
        }
        if line.len() > 1024 {
            return None;
        }
    }
    Some(String::from_utf8_lossy(&line).trim_end().to_owned())
}

fn serve_ftps(socket: TcpStream, tls: Arc<ServerConfig>, quirks: Quirks, seen: Arc<Mutex<Seen>>) {
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let Ok(connection) = ServerConnection::new(Arc::clone(&tls)) else {
        return;
    };
    let mut control = StreamOwned::new(connection, socket);
    let reply = |control: &mut StreamOwned<ServerConnection, TcpStream>, text: &str| {
        control.write_all(format!("{text}\r\n").as_bytes()).is_ok()
    };
    if !reply(&mut control, "220 stand-in printer") {
        return;
    }
    let mut logged_in = false;
    let mut protected = false;
    let mut passive: Option<TcpListener> = None;
    while let Some(line) = read_line(&mut control) {
        let (verb, argument) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        let verb = verb.to_ascii_uppercase();
        if verb == "PASS" {
            let mut guard = seen.lock().unwrap();
            guard.ftp_commands.push("PASS <given>".to_owned());
            guard.ftp_passwords.push(argument.to_owned());
        } else {
            seen.lock().unwrap().ftp_commands.push(line.clone());
        }
        let answer = match verb.as_str() {
            "USER" if argument == "bblp" => "331 password".to_owned(),
            "USER" => "530 no such user".to_owned(),
            "PASS" => {
                logged_in = argument == ACCESS_CODE;
                if logged_in {
                    "230 welcome".to_owned()
                } else {
                    "530 wrong access code".to_owned()
                }
            }
            "QUIT" => {
                reply(&mut control, "221 bye");
                return;
            }
            _ if !logged_in => "530 sign in first".to_owned(),
            "PBSZ" => "200 ok".to_owned(),
            "PROT" => {
                protected = argument == "P";
                "200 ok".to_owned()
            }
            "TYPE" => "200 ok".to_owned(),
            "PASV" => {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = listener.local_addr().unwrap().port();
                passive = Some(listener);
                let host = if quirks.pasv_names_other_address {
                    "10,255,255,1"
                } else {
                    "127,0,0,1"
                };
                format!(
                    "227 Entering Passive Mode ({host},{},{})",
                    port / 256,
                    port % 256
                )
            }
            "STOR" => {
                let Some(listener) = passive.take() else {
                    reply(&mut control, "425 no data connection");
                    continue;
                };
                let Ok((data, _)) = listener.accept() else {
                    return;
                };
                data.set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                if !reply(&mut control, "150 send it") {
                    return;
                }
                let mut bytes = Vec::new();
                if protected {
                    let Ok(connection) = ServerConnection::new(Arc::clone(&tls)) else {
                        return;
                    };
                    let mut data = StreamOwned::new(connection, data);
                    // The client ends with close_notify; a bare close also ends the file.
                    let _ = data.read_to_end(&mut bytes);
                } else {
                    let mut data = data;
                    let _ = data.read_to_end(&mut bytes);
                }
                if let Some(most) = quirks.store_at_most {
                    bytes.truncate(most);
                }
                let mut guard = seen.lock().unwrap();
                guard.data_tls.push(protected);
                guard.files.insert(argument.to_owned(), bytes);
                "226 stored".to_owned()
            }
            "SIZE" if quirks.no_size => "502 not implemented".to_owned(),
            "SIZE" => match seen.lock().unwrap().files.get(argument) {
                Some(bytes) => format!("213 {}", bytes.len()),
                None => "550 no such file".to_owned(),
            },
            "DELE" => {
                seen.lock().unwrap().files.remove(argument);
                "250 deleted".to_owned()
            }
            _ => "502 not implemented".to_owned(),
        };
        if !reply(&mut control, &answer) {
            return;
        }
    }
}

impl StandIn {
    /// A stand-in printer with this serial number.
    pub fn start(serial: &str) -> Self {
        Self::start_with(serial, serial, Quirks::default())
    }

    /// A stand-in whose certificate names `common_name`.
    pub fn start_with(serial: &str, common_name: &str, quirks: Quirks) -> Self {
        let (tls, certificate) = server_config(common_name, quirks.tls13_only, quirks.self_signed);
        let seen = Arc::new(Mutex::new(Seen {
            gcode_state: "IDLE".to_owned(),
            ..Seen::default()
        }));
        let mqtt = TcpListener::bind("127.0.0.1:0").unwrap();
        let ftps = TcpListener::bind("127.0.0.1:0").unwrap();
        let stand_in = Self {
            serial: serial.to_owned(),
            mqtt_port: mqtt.local_addr().unwrap().port(),
            ftps_port: ftps.local_addr().unwrap().port(),
            certificate,
            seen: Arc::clone(&seen),
        };
        {
            let (tls, seen, quirks, serial) = (
                Arc::clone(&tls),
                Arc::clone(&seen),
                quirks.clone(),
                serial.to_owned(),
            );
            std::thread::spawn(move || {
                for socket in mqtt.incoming().flatten() {
                    let (tls, seen, quirks, serial) = (
                        Arc::clone(&tls),
                        Arc::clone(&seen),
                        quirks.clone(),
                        serial.clone(),
                    );
                    std::thread::spawn(move || serve_mqtt(socket, tls, serial, quirks, seen));
                }
            });
        }
        std::thread::spawn(move || {
            for socket in ftps.incoming().flatten() {
                let (tls, seen, quirks) = (Arc::clone(&tls), Arc::clone(&seen), quirks.clone());
                std::thread::spawn(move || serve_ftps(socket, tls, quirks, seen));
            }
        });
        stand_in
    }

    /// Publishes `payload` on the report topic.
    pub fn push(&self, payload: &[u8]) {
        self.seen.lock().unwrap().outbox.push(payload.to_vec());
    }

    /// The kinds of every MQTT packet received so far.
    pub fn packets(&self) -> Vec<&'static str> {
        self.seen.lock().unwrap().packets.clone()
    }

    /// Everything received on the request topic, as text.
    pub fn requests(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .requests
            .iter()
            .map(|(_, _, payload)| payload.clone())
            .collect()
    }
}
