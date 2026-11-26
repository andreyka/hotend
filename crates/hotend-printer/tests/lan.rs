//! The LAN transport against a stand-in printer on loopback, inside this process. Nothing
//! here leaves the machine: [`Endpoint`] refuses every other address.
#![cfg(feature = "lan")]
#![allow(clippy::unwrap_used)]

mod common;
mod lan_standin;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::{PLA_TRAY, check, confirmed, pla_job, write_good, x2d};
use hotend_printer::ident::Serial;
use hotend_printer::lan::tls::{CertificateProblem, Issuers};
use hotend_printer::lan::{Endpoint, LanError, LanPrinter, Pairing, pair_with_issuers};
use hotend_printer::{
    AccessCode, Capabilities, Command, Delivered, Delivery, Phase, Printer, PrinterDelivery,
    PrinterError, StartOptions, StartVerdict, start_print,
};
use lan_standin::{ACCESS_CODE, Quirks, StandIn, authority_pem};
use sha2::{Digest, Sha256};

const SERIAL: &str = "20P0TEST00000001";

fn serial() -> Serial {
    Serial::new(SERIAL).unwrap()
}

fn code() -> AccessCode {
    AccessCode::new(ACCESS_CODE.to_owned()).unwrap()
}

fn endpoint(stand_in: &StandIn) -> Endpoint {
    Endpoint::with_ports(Ipv4Addr::LOCALHOST, stand_in.mqtt_port, stand_in.ftps_port).unwrap()
}

/// Pairs as the product does, with the stand-in authority in the place of Bambu Lab's.
fn pair(endpoint: Endpoint, serial: &Serial) -> Result<Pairing, LanError> {
    let issuers = Issuers::from_pem(authority_pem()).unwrap();
    pair_with_issuers(endpoint, serial, &issuers)
}

fn connect(stand_in: &StandIn) -> LanPrinter {
    let pairing = pair(endpoint(stand_in), &serial()).unwrap();
    LanPrinter::connect(endpoint(stand_in), &pairing, code(), x2d(), "Test printer").unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn only_loopback_addresses_can_be_connected_to() {
    let profile = x2d();
    for ip in [
        Ipv4Addr::new(192, 168, 1, 50),
        Ipv4Addr::new(10, 0, 0, 7),
        Ipv4Addr::new(172, 16, 0, 1),
        Ipv4Addr::new(169, 254, 1, 1),
        Ipv4Addr::new(8, 8, 8, 8),
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
    ] {
        let error = Endpoint::new(ip, &profile).unwrap_err();
        assert!(
            matches!(error, LanError::NotAllowed(refused) if refused == ip),
            "{ip}"
        );
        assert!(Endpoint::with_ports(ip, 1, 2).is_err());
    }
    let local = Endpoint::new(Ipv4Addr::LOCALHOST, &profile).unwrap();
    assert_eq!((local.mqtt_port(), local.ftps_port()), (8883, 990));
    assert!(Endpoint::new(Ipv4Addr::new(127, 9, 9, 9), &profile).is_ok());
}

#[test]
fn pairing_pins_the_certificate_of_the_confirmed_serial() {
    let stand_in = StandIn::start(SERIAL);
    let pairing = pair(endpoint(&stand_in), &serial()).unwrap();
    assert_eq!(pairing.serial(), &serial());
    assert_eq!(
        pairing.certificate_sha256(),
        hex(&Sha256::digest(&stand_in.certificate))
    );
    // A pairing is plain data the application can store.
    let stored = serde_json::to_string(&pairing).unwrap();
    assert_eq!(serde_json::from_str::<Pairing>(&stored).unwrap(), pairing);
    assert!(!stored.contains(ACCESS_CODE));
    // Pairing sends nothing: no sign-in reached the broker.
    assert!(stand_in.seen.lock().unwrap().mqtt_logins.is_empty());

    // The user confirmed another serial than the device presents.
    let other = Serial::new("20P0TEST00000002").unwrap();
    let error = pair(endpoint(&stand_in), &other).unwrap_err();
    assert!(
        matches!(&error, LanError::Certificate(CertificateProblem::WrongSerial { found }) if found == SERIAL),
        "{error}"
    );
    // Nothing listens.
    let dead = Endpoint::with_ports(Ipv4Addr::LOCALHOST, 1, 1).unwrap();
    assert!(matches!(
        pair(dead, &serial()),
        Err(LanError::Unreachable(_))
    ));
}

#[test]
fn a_paired_printer_reports_status_through_the_hostile_input_parser() {
    let stand_in = StandIn::start(SERIAL);
    let mut printer = connect(&stand_in);
    assert_eq!(printer.identity().serial, serial());
    assert_eq!(printer.identity().model, "bambu-x2d");
    let status = printer.status().unwrap();
    assert_eq!(status.phase(), Phase::Idle);
    let ams = status.ams_unit(0).unwrap();
    assert_eq!(
        ams.nozzle,
        Some(1),
        "AMS 0 feeds the main nozzle (device id 1)"
    );
    assert_eq!(ams.tray(0).unwrap().material.as_deref(), Some("PLA"));
    assert!(!ams.tray(1).unwrap().is_loaded());
    assert_eq!(status.active_nozzle, Some(1));

    {
        let seen = stand_in.seen.lock().unwrap();
        assert_eq!(
            seen.mqtt_logins,
            [("bblp".to_owned(), ACCESS_CODE.to_owned())]
        );
        assert_eq!(seen.subscriptions, [format!("device/{SERIAL}/report")]);
        assert_eq!(seen.requests[0].0, format!("device/{SERIAL}/request"));
        assert!(seen.requests[0].2.contains("pushall"));
    }

    // Garbage, a non-object and a report with wrong types change nothing and break nothing.
    let before = status.reports;
    stand_in.push(b"\xff\xfe not json");
    stand_in.push(b"[1, 2, 3]");
    stand_in.push(br#"{"print": {"gcode_state": 7, "ams": "none", "mc_percent": "many"}}"#);
    stand_in.push(br#"{"print": {"mc_percent": 42, "gcode_state": "RUNNING", "stg_cur": 0}}"#);
    let mut latest = printer.status().unwrap();
    for _ in 0..100 {
        if latest.progress_percent == Some(42) {
            break;
        }
        latest = printer
            .wait_for_report(latest.reports, Duration::from_millis(100))
            .unwrap();
    }
    assert!(latest.reports > before);
    assert_eq!(latest.progress_percent, Some(42));
    assert_eq!(latest.phase(), Phase::Printing);
    assert_eq!(
        latest
            .ams_unit(0)
            .unwrap()
            .tray(0)
            .unwrap()
            .material
            .as_deref(),
        Some("PLA")
    );

    // The access code is in no debug output.
    assert!(!format!("{printer:?}").contains(ACCESS_CODE));
}

#[test]
fn a_wrong_access_code_is_refused_with_a_clear_error() {
    let stand_in = StandIn::start(SERIAL);
    let pairing = pair(endpoint(&stand_in), &serial()).unwrap();
    let wrong = AccessCode::new("wr0ngc0de".to_owned()).unwrap();
    let error = LanPrinter::connect(endpoint(&stand_in), &pairing, wrong, x2d(), "x").unwrap_err();
    assert!(matches!(error, LanError::AccessCodeRefused), "{error}");
    assert!(!error.to_string().contains("wr0ngc0de"));
    assert!(!format!("{error:?}").contains("wr0ngc0de"));
}

#[test]
fn another_device_with_the_same_serial_gets_nothing_after_pairing() {
    let real = StandIn::start(SERIAL);
    let pairing = pair(endpoint(&real), &serial()).unwrap();
    // An impostor presents a certificate with the right serial but another key.
    let impostor = StandIn::start(SERIAL);
    let error = LanPrinter::connect(endpoint(&impostor), &pairing, code(), x2d(), "x").unwrap_err();
    assert!(
        matches!(error, LanError::Certificate(CertificateProblem::Changed)),
        "{error}"
    );
    // The handshake failed before the sign-in: the impostor never saw the access code.
    assert!(impostor.seen.lock().unwrap().mqtt_logins.is_empty());

    // A device naming another serial, after pairing.
    let other = StandIn::start_with(SERIAL, "20P0TEST00000009", Quirks::default());
    let error = LanPrinter::connect(endpoint(&other), &pairing, code(), x2d(), "x").unwrap_err();
    assert!(
        matches!(
            error,
            LanError::Certificate(CertificateProblem::WrongSerial { .. })
        ),
        "{error}"
    );
    assert!(other.seen.lock().unwrap().mqtt_logins.is_empty());

    // The upload channel is pinned too: FTPS of the impostor, MQTT of the real printer.
    let dir = tempfile::tempdir().unwrap();
    let job = check(&write_good(dir.path(), "box.gcode.3mf"), "box", &pla_job()).unwrap();
    let mixed =
        Endpoint::with_ports(Ipv4Addr::LOCALHOST, real.mqtt_port, impostor.ftps_port).unwrap();
    let mut printer = LanPrinter::connect(mixed, &pairing, code(), x2d(), "x").unwrap();
    assert!(printer.upload(&job).is_err());
    let seen = impostor.seen.lock().unwrap();
    assert!(
        seen.ftp_passwords.is_empty() && seen.files.is_empty(),
        "{:?}",
        seen.ftp_commands
    );
}

#[test]
fn a_certificate_that_only_names_the_serial_is_not_paired() {
    // A serial number is announced on the network. Any device can answer on the printer's
    // port with a certificate it made for that serial; pinning it would hand it the access
    // code on the next connection.
    let impostor = StandIn::start_with(
        SERIAL,
        SERIAL,
        Quirks {
            self_signed: true,
            ..Quirks::default()
        },
    );
    let error = pair(endpoint(&impostor), &serial()).unwrap_err();
    assert!(
        matches!(
            error,
            LanError::Certificate(CertificateProblem::UntrustedIssuer)
        ),
        "{error}"
    );
    // Against an authority that did not sign the device's certificate the chain check
    // refuses it.
    let stand_in = StandIn::start(SERIAL);
    let other = Issuers::from_pem(&lan_standin::other_authority_pem()).unwrap();
    let error = pair_with_issuers(endpoint(&stand_in), &serial(), &other).unwrap_err();
    assert!(
        matches!(
            error,
            LanError::Certificate(CertificateProblem::UntrustedIssuer)
        ),
        "{error}"
    );
    assert!(impostor.seen.lock().unwrap().mqtt_logins.is_empty());
    assert!(stand_in.seen.lock().unwrap().mqtt_logins.is_empty());
}

#[test]
fn pairing_needs_a_file_of_bambu_labs_authorities() {
    // Hotend ships no certificate authority. Without a file, or with a file of any other
    // authority (here the stand-in's own, which did sign the device's certificate), nothing
    // is connected to: a file of the wrong authorities would let any device it signed be
    // paired and then be sent the access code.
    let stand_in = StandIn::start(SERIAL);
    let dir = tempfile::tempdir().unwrap();
    let foreign = dir.path().join("stand-in.pem");
    std::fs::write(&foreign, authority_pem()).unwrap();
    let mixed = dir.path().join("mixed.pem");
    std::fs::write(&mixed, format!("{}\n{}", authority_pem(), authority_pem())).unwrap();
    for refused in [
        dir.path().join("none.pem"),
        dir.path().to_path_buf(),
        foreign,
        mixed,
    ] {
        let error =
            hotend_printer::lan::pair(endpoint(&stand_in), &serial(), &refused).unwrap_err();
        assert!(matches!(error, LanError::NoIssuers(_)), "{error}");
        assert!(error.to_string().contains("Nothing was sent"), "{error}");
    }
    assert!(Issuers::bambu_lab_from_file(&dir.path().join("stand-in.pem")).is_none());
    assert!(Issuers::from_file(&dir.path().join("stand-in.pem")).is_some());
    let seen = stand_in.seen.lock().unwrap();
    assert!(seen.mqtt_logins.is_empty());
}

#[test]
fn only_tls_1_2_is_spoken() {
    let stand_in = StandIn::start_with(
        SERIAL,
        SERIAL,
        Quirks {
            tls13_only: true,
            ..Quirks::default()
        },
    );
    let error = pair(endpoint(&stand_in), &serial()).unwrap_err();
    assert!(matches!(error, LanError::Tls(_)), "{error}");
}

#[test]
fn a_job_is_uploaded_started_after_confirmation_and_seen_to_begin() {
    let stand_in = StandIn::start(SERIAL);
    let mut printer = connect(&stand_in);
    let dir = tempfile::tempdir().unwrap();
    let path = write_good(dir.path(), "box.gcode.3mf");
    let job = check(&path, "Little box", &pla_job()).unwrap();

    // A real printer's delivery states the real printer's abilities, limits and cost.
    assert_eq!(
        PrinterDelivery::new(&mut printer).capabilities(),
        Capabilities::lan()
    );

    let Delivered::OnPrinter(uploaded) = PrinterDelivery::new(&mut printer).deliver(&job).unwrap()
    else {
        panic!("expected an upload");
    };
    // The name on the printer carries the hash of the file, so no other file can be
    // stored under it.
    let name = format!("Little_box-{}.gcode.3mf", &job.sha256()[..32]);
    assert_eq!(uploaded.remote_name(), name);
    {
        let seen = stand_in.seen.lock().unwrap();
        assert_eq!(seen.files[&name], std::fs::read(&path).unwrap());
        assert_eq!(seen.data_tls, [true], "the data connection is encrypted");
        assert_eq!(seen.ftp_passwords, [ACCESS_CODE]);
        let commands: Vec<&str> = seen.ftp_commands.iter().map(String::as_str).collect();
        assert_eq!(
            commands,
            [
                "USER bblp",
                "PASS <given>",
                "PBSZ 0",
                "PROT P",
                "TYPE I",
                "PASV",
                format!("STOR {name}").as_str(),
                format!("SIZE {name}").as_str(),
                "QUIT"
            ]
        );
    }

    let token = confirmed(&job, &printer);
    let before = printer.status().unwrap().reports;
    let mut watchdog = start_print(
        &mut printer,
        token,
        &job,
        &uploaded,
        &[PLA_TRAY],
        StartOptions::default(),
    )
    .unwrap();
    let status = printer
        .wait_for_report(before, Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        watchdog.observe(&status, Duration::from_secs(1), &x2d()),
        StartVerdict::Started {
            phase: Phase::Preparing
        }
    );
    assert_eq!(status.job_name.as_deref(), Some("Little box"));

    let start: serde_json::Value = serde_json::from_str(
        stand_in
            .requests()
            .iter()
            .find(|request| request.contains("project_file"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(start["print"]["url"], format!("ftp:///{name}"));
    assert_eq!(start["print"]["param"], "Metadata/plate_1.gcode");
    assert_eq!(start["print"]["ams_mapping"], serde_json::json!([0]));

    // Pause is acknowledged (QoS 1) and shows in the next report.
    let before = printer.status().unwrap().reports;
    printer.send(Command::Pause).unwrap();
    let status = printer
        .wait_for_report(before, Duration::from_secs(5))
        .unwrap();
    assert_eq!(status.phase(), Phase::Paused);
    let seen = stand_in.seen.lock().unwrap();
    let pause = seen
        .requests
        .iter()
        .find(|(_, _, payload)| payload.contains("\"pause\""))
        .unwrap();
    assert_eq!(pause.1, 1);
    for (topic, _, payload) in &seen.requests {
        assert_eq!(topic, &format!("device/{SERIAL}/request"));
        assert!(!payload.contains("gcode_line"), "{payload}");
    }
}

/// How an AMS HT is named in the flat `ams_mapping` is not proven on a printer. A start
/// that would need it is refused before anything is sent, and says what to do instead.
#[test]
fn a_start_from_an_ams_ht_is_refused_and_nothing_is_sent() {
    let stand_in = StandIn::start(SERIAL);
    let mut printer = connect(&stand_in);
    let dir = tempfile::tempdir().unwrap();
    let path = write_good(dir.path(), "box.gcode.3mf");
    let job = check(&path, "Little box", &pla_job()).unwrap();
    let Delivered::OnPrinter(uploaded) = PrinterDelivery::new(&mut printer).deliver(&job).unwrap()
    else {
        panic!("expected an upload");
    };
    let token = confirmed(&job, &printer);
    let ht = hotend_printer::FilamentSource::Ams { unit: 128, tray: 0 };
    let Err(refused) = start_print(
        &mut printer,
        token,
        &job,
        &uploaded,
        &[ht],
        StartOptions::default(),
    ) else {
        panic!("a start from an AMS HT was accepted");
    };
    assert!(
        matches!(
            &refused,
            hotend_printer::StartError::Printer(PrinterError::NotSupported(why))
                if why.contains("AMS HT") && why.contains("Bambu Connect")
        ),
        "{refused:?}"
    );
    // Nothing was sent: no start command reached the printer, and it stays idle.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !stand_in
            .requests()
            .iter()
            .any(|request| request.contains("project_file")),
        "{:?}",
        stand_in.requests()
    );
    assert_eq!(printer.status().unwrap().phase(), Phase::Idle);
}

#[test]
fn an_accepted_start_that_changes_nothing_is_reported_by_the_watchdog() {
    let stand_in = StandIn::start_with(
        SERIAL,
        SERIAL,
        Quirks {
            ignore_start: true,
            ..Quirks::default()
        },
    );
    let mut printer = connect(&stand_in);
    let dir = tempfile::tempdir().unwrap();
    let job = check(&write_good(dir.path(), "box.gcode.3mf"), "box", &pla_job()).unwrap();
    let uploaded = printer.upload(&job).unwrap();
    let token = confirmed(&job, &printer);
    let before = printer.status().unwrap().reports;
    let mut watchdog = start_print(
        &mut printer,
        token,
        &job,
        &uploaded,
        &[PLA_TRAY],
        StartOptions::default(),
    )
    .unwrap();
    let status = printer
        .wait_for_report(before, Duration::from_secs(5))
        .unwrap();
    assert_eq!(status.phase(), Phase::Idle);
    assert!(matches!(
        watchdog.observe(&status, Duration::from_secs(90), &x2d()),
        StartVerdict::NotStarted { .. }
    ));
}

#[test]
fn the_data_connection_goes_to_the_printer_whatever_the_server_names() {
    let quirks = Quirks {
        pasv_names_other_address: true,
        no_size: true,
        ..Quirks::default()
    };
    let stand_in = StandIn::start_with(SERIAL, SERIAL, quirks);
    let mut printer = connect(&stand_in);
    let dir = tempfile::tempdir().unwrap();
    let path = write_good(dir.path(), "box.gcode.3mf");
    let job = check(&path, "box", &pla_job()).unwrap();
    // The server names 10.255.255.1; the upload still arrives on loopback, and a server
    // without SIZE is accepted on the hash of what was sent.
    printer.upload(&job).unwrap();
    assert_eq!(
        stand_in.seen.lock().unwrap().files[&job.remote_name()],
        std::fs::read(&path).unwrap()
    );
}

#[test]
fn an_upload_that_is_not_stored_whole_fails_and_is_removed() {
    let quirks = Quirks {
        store_at_most: Some(100),
        ..Quirks::default()
    };
    let stand_in = StandIn::start_with(SERIAL, SERIAL, quirks);
    let mut printer = connect(&stand_in);
    let dir = tempfile::tempdir().unwrap();
    let path = write_good(dir.path(), "box.gcode.3mf");
    let job = check(&path, "box", &pla_job()).unwrap();
    let error = printer.upload(&job).unwrap_err();
    assert!(matches!(error, PrinterError::Upload(_)), "{error}");
    assert!(
        stand_in.seen.lock().unwrap().files.is_empty(),
        "the partial file was deleted"
    );

    // A file changed after the check is not sent at all.
    std::fs::write(&path, b"changed").unwrap();
    assert!(matches!(printer.upload(&job), Err(PrinterError::Job(_))));
}

#[test]
fn a_dropped_connection_is_reported_and_never_silently_reopened() {
    let stand_in = StandIn::start(SERIAL);
    let mut printer = connect(&stand_in);
    stand_in.seen.lock().unwrap().kill_mqtt = true;
    let mut result = printer.status();
    for _ in 0..100 {
        if result.is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        result = printer.status();
    }
    assert!(
        matches!(result, Err(PrinterError::Unreachable(_))),
        "{result:?}"
    );
    assert!(matches!(
        printer.send(Command::Stop),
        Err(PrinterError::Unreachable(_))
    ));
    // The broker is back; the old session stays closed and nothing queued arrives.
    stand_in.seen.lock().unwrap().kill_mqtt = false;
    std::thread::sleep(Duration::from_millis(400));
    let seen = stand_in.seen.lock().unwrap();
    assert_eq!(seen.mqtt_logins.len(), 1);
    assert!(
        seen.requests
            .iter()
            .all(|(_, _, payload)| !payload.contains("stop"))
    );
}
