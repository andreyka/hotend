//! The read-only status listener against a stand-in printer on loopback, inside this
//! process. Nothing here leaves the machine: the stand-in listens on 127.0.0.1.
//!
//! The central proof is at the transport level: the stand-in broker records the type of
//! every MQTT packet it receives, and a whole session (sign-in, subscription, reports,
//! keep-alive, end) must show only CONNECT, SUBSCRIBE, PINGREQ and DISCONNECT from Hotend.
#![cfg(feature = "lan")]
#![allow(clippy::unwrap_used)]

mod common;
mod lan_standin;

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use common::x2d;
use hotend_printer::ident::Serial;
use hotend_printer::lan::tls::{CertificateProblem, Issuers};
use hotend_printer::lan::{
    LanError, ListenOptions, ListenTarget, StatusListener, Trust, probe_certificate,
};
use hotend_printer::{AccessCode, Phase};
use lan_standin::{ACCESS_CODE, Quirks, StandIn, authority_pem, other_authority_pem};
use sha2::{Digest, Sha256};

const SERIAL: &str = "20P0TEST00000002";

fn serial() -> Serial {
    Serial::new(SERIAL).unwrap()
}

fn code() -> AccessCode {
    AccessCode::new(ACCESS_CODE.to_owned()).unwrap()
}

fn target(stand_in: &StandIn) -> ListenTarget {
    ListenTarget::with_port(Ipv4Addr::LOCALHOST, stand_in.mqtt_port, serial()).unwrap()
}

fn authorities() -> Trust {
    Trust::Authorities(Issuers::from_pem(authority_pem()).unwrap())
}

fn quick() -> ListenOptions {
    ListenOptions {
        keep_alive: Duration::from_secs(1),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Waits until `done` holds, at most five seconds.
fn eventually(mut done: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

#[test]
fn a_whole_session_sends_no_publish_only_connect_subscribe_ping_and_disconnect() {
    let stand_in = StandIn::start(SERIAL);
    let listener =
        StatusListener::connect(target(&stand_in), authorities(), code(), x2d(), quick()).unwrap();
    assert!(listener.state().connected);

    // Partial reports only (no pushall is ever asked for): each is merged into the status.
    stand_in.push(
        br#"{"print": {"command": "push_status", "gcode_state": "RUNNING", "mc_percent": 12}}"#,
    );
    let state = listener.wait_for_update(0, Duration::from_secs(5));
    assert_eq!(state.status.progress_percent, Some(12));
    stand_in.push(br#"{"print": {"command": "push_status", "layer_num": 7, "total_layer_num": 40, "subtask_name": "box-v3"}}"#);
    let state = listener.wait_for_update(1, Duration::from_secs(5));
    assert_eq!(state.status.reports, 2);
    assert_eq!(
        (state.status.layer, state.status.total_layers),
        (Some(7), Some(40))
    );
    assert_eq!(
        state.status.progress_percent,
        Some(12),
        "kept from the first report"
    );
    assert_eq!(state.status.job_name.as_deref(), Some("box-v3"));
    assert_eq!(state.status.phase(), Phase::Printing);

    // Long enough for the keep-alive (a PINGREQ every half second here).
    assert!(eventually(|| stand_in.packets().contains(&"PingReq")));
    drop(listener);
    assert!(eventually(|| stand_in.packets().contains(&"Disconnect")));

    let packets = stand_in.packets();
    for kind in &packets {
        assert!(
            ["Connect", "Subscribe", "PingReq", "Disconnect"].contains(kind),
            "Hotend sent a {kind} packet: {packets:?}"
        );
    }
    assert_eq!(packets.iter().filter(|k| **k == "Publish").count(), 0);
    assert_eq!(packets[0], "Connect");
    assert_eq!(packets[1], "Subscribe");
    assert_eq!(packets.last(), Some(&"Disconnect"));
    let seen = stand_in.seen.lock().unwrap();
    assert!(
        seen.requests.is_empty(),
        "nothing on any topic: {:?}",
        seen.requests
    );
    assert_eq!(seen.subscriptions, [format!("device/{SERIAL}/report")]);
    assert_eq!(seen.mqtt_logins.len(), 1);
    assert_eq!(seen.mqtt_logins[0].0, "bblp");
}

#[test]
fn hostile_oversize_and_unknown_reports_are_dropped_and_the_session_goes_on() {
    let stand_in = StandIn::start(SERIAL);
    let listener =
        StatusListener::connect(target(&stand_in), authorities(), code(), x2d(), quick()).unwrap();
    stand_in.push(b"not json at all");
    stand_in.push(&vec![
        b' ';
        hotend_printer::protocol::report::MAX_REPORT_BYTES + 1
    ]);
    stand_in.push(br#"{"print": {"command": "push_status", "gcode_state": "IDLE", "a_field_from_newer_firmware": {"x": [1]}, "bed_temper": "55.5"}}"#);
    let state = listener.wait_for_update(0, Duration::from_secs(5));
    assert_eq!(state.status.reports, 1);
    assert_eq!(state.dropped_reports, 2);
    assert_eq!(state.status.bed.current, Some(55.5));
    assert!(state.connected);
    drop(listener);
    assert!(!stand_in.packets().contains(&"Publish"));
}

#[test]
fn a_first_look_returns_fingerprint_and_subject_and_sends_nothing() {
    let stand_in = StandIn::start(SERIAL);
    let seen = probe_certificate(&target(&stand_in)).unwrap();
    assert_eq!(seen.sha256, sha256_hex(&stand_in.certificate));
    assert!(seen.subject.contains(SERIAL), "{}", seen.subject);
    assert!(
        seen.issuer.contains("Stand-in printer CA"),
        "{}",
        seen.issuer
    );
    assert_eq!(seen.serial, SERIAL);
    // A handshake only: not even a CONNECT, so no access code.
    std::thread::sleep(Duration::from_millis(200));
    assert!(stand_in.packets().is_empty());

    // A self-signed certificate that names the serial is shown too: the person decides,
    // and the pin then holds it to exactly that certificate.
    let anyone = StandIn::start_with(
        SERIAL,
        SERIAL,
        Quirks {
            self_signed: true,
            ..Quirks::default()
        },
    );
    let shown = probe_certificate(&target(&anyone)).unwrap();
    assert_eq!(shown.sha256, sha256_hex(&anyone.certificate));

    // One that names another serial is refused at once.
    let other = StandIn::start_with(SERIAL, "20P0OTHER0000009", Quirks::default());
    let error = probe_certificate(&target(&other)).unwrap_err();
    assert!(
        matches!(
            error,
            LanError::Certificate(CertificateProblem::WrongSerial { .. })
        ),
        "{error}"
    );
}

#[test]
fn a_pinned_certificate_is_accepted_and_any_other_is_refused_before_the_sign_in() {
    let stand_in = StandIn::start(SERIAL);
    let pin = probe_certificate(&target(&stand_in)).unwrap().sha256;
    let listener = StatusListener::connect(
        target(&stand_in),
        Trust::Pinned(pin.clone()),
        code(),
        x2d(),
        quick(),
    )
    .unwrap();
    drop(listener);

    // Another device at the address, with a certificate for the same serial.
    let impostor = StandIn::start(SERIAL);
    let error = StatusListener::connect(
        target(&impostor),
        Trust::Pinned(pin),
        code(),
        x2d(),
        quick(),
    )
    .unwrap_err();
    assert!(
        matches!(error, LanError::Certificate(CertificateProblem::Changed)),
        "{error}"
    );
    assert!(error.to_string().contains("not the one this printer had"));
    // The access code never reached it: no CONNECT at all.
    std::thread::sleep(Duration::from_millis(200));
    assert!(impostor.packets().is_empty());
    assert!(impostor.seen.lock().unwrap().mqtt_logins.is_empty());

    // A pin that is not a fingerprint is refused without connecting.
    let error = StatusListener::connect(
        target(&impostor),
        Trust::Pinned("not-a-fingerprint".to_owned()),
        code(),
        x2d(),
        quick(),
    )
    .unwrap_err();
    assert!(matches!(error, LanError::Tls(_)), "{error}");
}

#[test]
fn with_the_authorities_file_a_certificate_must_chain_to_it() {
    let stand_in = StandIn::start(SERIAL);
    let wrong = Trust::Authorities(Issuers::from_pem(&other_authority_pem()).unwrap());
    let error =
        StatusListener::connect(target(&stand_in), wrong, code(), x2d(), quick()).unwrap_err();
    assert!(
        matches!(
            error,
            LanError::Certificate(CertificateProblem::UntrustedIssuer)
        ),
        "{error}"
    );
    let self_signed = StandIn::start_with(
        SERIAL,
        SERIAL,
        Quirks {
            self_signed: true,
            ..Quirks::default()
        },
    );
    let error =
        StatusListener::connect(target(&self_signed), authorities(), code(), x2d(), quick())
            .unwrap_err();
    assert!(
        matches!(
            error,
            LanError::Certificate(CertificateProblem::UntrustedIssuer)
        ),
        "{error}"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(stand_in.seen.lock().unwrap().mqtt_logins.is_empty());
    assert!(self_signed.seen.lock().unwrap().mqtt_logins.is_empty());
    // The right authority: accepted.
    let listener =
        StatusListener::connect(target(&stand_in), authorities(), code(), x2d(), quick()).unwrap();
    assert!(listener.state().connected);
}

#[test]
fn public_addresses_are_refused_before_anything_is_opened() {
    for ip in [
        Ipv4Addr::new(8, 8, 8, 8),
        Ipv4Addr::new(1, 1, 1, 1),
        Ipv4Addr::new(169, 254, 1, 1),
        Ipv4Addr::new(100, 64, 0, 1),
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        Ipv4Addr::new(224, 0, 0, 251),
    ] {
        let error = ListenTarget::with_port(ip, 8883, serial()).unwrap_err();
        assert!(
            matches!(error, LanError::NotPrivate(refused) if refused == ip),
            "{ip}"
        );
        assert!(ListenTarget::new(ip, serial(), &x2d()).is_err());
    }
    // A private address is a valid target (nothing is connected to here).
    let home = ListenTarget::new(Ipv4Addr::new(192, 168, 1, 50), serial(), &x2d()).unwrap();
    assert_eq!(home.port(), 8883);
}

#[test]
fn a_refused_access_code_and_a_dropped_connection_are_reported_and_never_retried() {
    let stand_in = StandIn::start(SERIAL);
    let wrong = AccessCode::new("wr0ngc0de".to_owned()).unwrap();
    let error = StatusListener::connect(target(&stand_in), authorities(), wrong, x2d(), quick())
        .unwrap_err();
    assert!(matches!(error, LanError::AccessCodeRefused), "{error}");
    let text = format!("{error} {error:?}");
    assert!(
        !text.contains("wr0ngc0de") && !text.contains(ACCESS_CODE),
        "{text}"
    );

    let listener =
        StatusListener::connect(target(&stand_in), authorities(), code(), x2d(), quick()).unwrap();
    let logins = stand_in.seen.lock().unwrap().mqtt_logins.len();
    stand_in.seen.lock().unwrap().kill_mqtt = true;
    let state = listener.wait_for_update(u64::MAX, Duration::from_secs(5));
    assert!(!state.connected);
    let ended = state.ended.clone().unwrap();
    assert!(!ended.contains(ACCESS_CODE), "{ended}");
    stand_in.seen.lock().unwrap().kill_mqtt = false;
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        stand_in.seen.lock().unwrap().mqtt_logins.len(),
        logins,
        "no silent reconnection"
    );
    // Neither the state nor the listener prints the code.
    let shown = format!("{listener:?} {state:?}");
    assert!(!shown.contains(ACCESS_CODE), "{shown}");
}
