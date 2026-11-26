//! The simulated printer, the start watchdog and job tracking.
#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use common::{
    PETG_TRAY, PLA_TRAY, check, confirmed, pla_job, sim, two_nozzle_job, write_good, write_two, x2d,
};
use hotend_printer::protocol::hms::HmsCode;
use hotend_printer::{
    CheckedJob, Command, FilamentSource, JobEvent, JobOutcome, JobTracker, NotStartedReason, Phase,
    Printer, PrinterError, SimFault, SimulatedPrinter, SpeedProfile, StartError, StartOptions,
    StartQuirk, StartVerdict, StartWatchdog, Tray, UploadedJob, start_print,
};

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

struct Fixture {
    _dir: tempfile::TempDir,
    job: CheckedJob,
    printer: SimulatedPrinter,
    uploaded: UploadedJob,
}

/// The 20-minute PLA job, uploaded to a fresh simulated X2D.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let job = check(&write_good(dir.path(), "box.gcode.3mf"), "box", &pla_job()).unwrap();
    let mut printer = sim();
    let uploaded = printer.upload(&job).unwrap();
    Fixture {
        _dir: dir,
        job,
        printer,
        uploaded,
    }
}

impl Fixture {
    fn start(&mut self) -> Result<StartWatchdog, StartError> {
        self.start_from(&[PLA_TRAY])
    }

    fn start_from(&mut self, sources: &[FilamentSource]) -> Result<StartWatchdog, StartError> {
        let token = confirmed(&self.job, &self.printer);
        start_print(
            &mut self.printer,
            token,
            &self.job,
            &self.uploaded,
            sources,
            StartOptions::default(),
        )
    }

    fn phase(&mut self) -> Phase {
        self.printer.status().unwrap().phase()
    }
}

#[test]
fn a_job_runs_from_idle_through_heating_and_printing_to_finished() {
    let mut f = fixture();
    let idle = f.printer.status().unwrap();
    assert_eq!(idle.phase(), Phase::Idle);
    assert_eq!(idle.storage_present, Some(true));
    let name = f.job.remote_name();
    assert_eq!(name, format!("box-{}.gcode.3mf", &f.job.sha256()[..32]));
    assert_eq!(f.printer.stored_files(), [name.as_str()]);
    assert_eq!(f.printer.stored_sha256(&name), Some(f.job.sha256()));

    let mut watchdog = f.start().unwrap();
    let profile = x2d();
    let mut tracker = JobTracker::new();
    let mut events = Vec::new();
    let mut phases = vec![f.phase()];
    let mut waited = Duration::ZERO;
    let mut verdict = StartVerdict::Waiting;
    // Preparation takes 60 s, heating 120 s and the print the 1200 s the file predicts.
    for _ in 0..150 {
        let status = f.printer.status().unwrap();
        if verdict == StartVerdict::Waiting {
            verdict = watchdog.observe(&status, waited, &profile);
        }
        events.extend(tracker.observe(&status, &profile));
        if phases.last() != Some(&status.phase()) {
            phases.push(status.phase());
        }
        f.printer.advance(secs(10));
        waited += secs(10);
    }
    assert_eq!(
        verdict,
        StartVerdict::Started {
            phase: Phase::Preparing
        }
    );
    assert_eq!(
        phases,
        [
            Phase::Preparing,
            Phase::Heating,
            Phase::Printing,
            Phase::Finished
        ]
    );
    assert_eq!(tracker.outcome(), Some(&JobOutcome::Finished));
    assert_eq!(events.last(), Some(&JobEvent::Ended(JobOutcome::Finished)));
    let percents: Vec<u8> = events
        .iter()
        .filter_map(|event| match event {
            JobEvent::Progress { percent, .. } => Some(*percent),
            _ => None,
        })
        .collect();
    assert!(percents.len() > 50 && percents.is_sorted(), "{percents:?}");
    assert!(events.iter().any(|event| matches!(
        event,
        JobEvent::Phase { phase: Phase::Heating, headline } if headline == "Heating up."
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, JobEvent::Problem(_)))
    );

    let done = f.printer.status().unwrap();
    assert_eq!(done.progress_percent, Some(100));
    assert_eq!(done.job_name.as_deref(), Some("box"));
    // A finished printer takes the next job.
    assert!(f.start().is_ok());
}

#[test]
fn status_mid_print_reports_temperatures_progress_and_layers() {
    let mut f = fixture();
    f.start().unwrap();
    f.printer.advance(secs(60 + 60));
    let heating = f.printer.status().unwrap();
    assert_eq!(heating.phase(), Phase::Heating);
    let main = heating.nozzles[&1];
    assert_eq!(
        main.target,
        Some(220.0),
        "the temperature the sliced file sets"
    );
    assert!(main.current.unwrap() > 25.0 && main.current.unwrap() < 220.0);
    assert_eq!(
        heating.nozzles[&0].target,
        Some(0.0),
        "the auxiliary nozzle stays cold"
    );
    assert_eq!(heating.active_nozzle, Some(1));
    assert_eq!(heating.bed.target, Some(35.0));

    f.printer.advance(secs(60 + 600));
    let printing = f.printer.status().unwrap();
    assert_eq!(printing.phase(), Phase::Printing);
    assert_eq!(printing.progress_percent, Some(50));
    assert_eq!(printing.remaining_minutes, Some(10));
    assert_eq!(printing.total_layers, Some(3));
    assert_eq!(printing.layer, Some(1));
    assert_eq!(
        printing.gcode_file.as_deref(),
        Some(f.job.remote_name().as_str())
    );
}

#[test]
fn pause_resume_stop_speed_and_light_work_and_are_refused_when_they_make_no_sense() {
    let mut f = fixture();
    assert!(matches!(
        f.printer.send(Command::Pause),
        Err(PrinterError::Refused(_))
    ));
    assert!(matches!(
        f.printer.send(Command::Stop),
        Err(PrinterError::Refused(_))
    ));
    f.printer.send(Command::Light(false)).unwrap();
    f.printer
        .send(Command::SetSpeed(SpeedProfile::Silent))
        .unwrap();
    f.printer.send(Command::RequestStatus).unwrap();
    let status = f.printer.status().unwrap();
    assert_eq!(status.chamber_light, Some(false));
    assert_eq!(status.speed_level, Some(1));

    f.start().unwrap();
    f.printer.advance(secs(180 + 300));
    f.printer.send(Command::Pause).unwrap();
    assert_eq!(f.phase(), Phase::Paused);
    let before = f.printer.status().unwrap().progress_percent;
    f.printer.advance(secs(3600));
    assert_eq!(
        f.printer.status().unwrap().progress_percent,
        before,
        "paused time does not print"
    );
    assert!(matches!(
        f.printer.send(Command::Pause),
        Err(PrinterError::Refused(_))
    ));
    f.printer.send(Command::Resume).unwrap();
    assert_eq!(f.phase(), Phase::Printing);
    assert!(matches!(
        f.printer.send(Command::Resume),
        Err(PrinterError::Refused(_))
    ));

    let profile = x2d();
    let mut tracker = JobTracker::new();
    tracker.observe(&f.printer.status().unwrap(), &profile);
    f.printer.send(Command::Stop).unwrap();
    let stopped = f.printer.status().unwrap();
    assert_eq!(stopped.phase(), Phase::Failed);
    assert_eq!(stopped.print_error, Some(0x0300_400C));
    let events = tracker.observe(&stopped, &profile);
    assert_eq!(events.last(), Some(&JobEvent::Ended(JobOutcome::Cancelled)));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, JobEvent::Problem(_)))
    );
    // Everything that was sent is on the allowlist.
    for line in f.printer.wire_log() {
        assert!(!line.contains("gcode_line"), "{line}");
    }
}

#[test]
fn a_failure_carries_the_hms_code_in_plain_words_and_raw() {
    let mut f = fixture();
    f.printer.config_mut().fault = Some(SimFault {
        at_percent: 40,
        hms: HmsCode {
            attr: 0x0300_0100,
            code: 0x0001_0007,
        },
        print_error: 0x0300_8001,
    });
    let mut watchdog = f.start().unwrap();
    let profile = x2d();
    let mut tracker = JobTracker::new();
    let status = f.printer.status().unwrap();
    assert!(matches!(
        watchdog.observe(&status, secs(1), &profile),
        StartVerdict::Started { .. }
    ));
    let mut events = tracker.observe(&status, &profile);
    f.printer.advance(secs(180 + 400));
    assert_eq!(f.phase(), Phase::Printing);
    events.extend(tracker.observe(&f.printer.status().unwrap(), &profile));
    f.printer.advance(secs(200));
    let failed = f.printer.status().unwrap();
    assert_eq!(failed.phase(), Phase::Failed);
    events.extend(tracker.observe(&failed, &profile));

    let Some(JobOutcome::Failed { problems }) = tracker.outcome() else {
        panic!("expected a failure, got {:?}", tracker.outcome());
    };
    let hms = problems
        .iter()
        .find(|problem| problem.raw_code == "HMS_0300_0100_0001_0007")
        .unwrap();
    assert!(hms.blocking);
    assert_eq!(hms.meaning.as_deref(), Some("heatbed_sensor"));
    assert!(
        !hms.message.contains("0300"),
        "plain words, the code beside them: {}",
        hms.message
    );
    assert!(
        hms.help_url
            .as_deref()
            .unwrap()
            .ends_with("0300_0100_0001_0007")
    );
    let error = problems
        .iter()
        .find(|problem| problem.raw_code == "0300_8001")
        .unwrap();
    assert!(error.message.contains("printer's screen"));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, JobEvent::Problem(_)))
            .count(),
        2
    );
    assert!(matches!(
        events.last(),
        Some(JobEvent::Ended(JobOutcome::Failed { .. }))
    ));
    // After the end the tracker says nothing more.
    assert!(tracker.observe(&failed, &profile).is_empty());
}

#[test]
fn a_start_that_is_accepted_while_the_ams_dries_is_reported_as_not_started() {
    let mut f = fixture();
    f.printer.set_drying(0, 240);
    f.printer.config_mut().start_quirk = StartQuirk::AcceptedButStaysIdle;
    let profile = x2d();
    // The command is accepted: `start_print` succeeds and the printer got it.
    let mut watchdog = f.start().unwrap().with_deadline(secs(60));
    assert!(
        f.printer
            .wire_log()
            .last()
            .unwrap()
            .contains("project_file")
    );

    let mut waited = Duration::ZERO;
    let mut verdict = StartVerdict::Waiting;
    while waited < secs(60) {
        assert_eq!(verdict, StartVerdict::Waiting);
        f.printer.advance(secs(5));
        waited += secs(5);
        verdict = watchdog.observe(&f.printer.status().unwrap(), waited, &profile);
    }
    let StartVerdict::NotStarted { reason, message } = &verdict else {
        panic!("expected NotStarted, got {verdict:?}");
    };
    assert_eq!(*reason, NotStartedReason::AmsDrying);
    assert!(message.contains("drying"), "{message}");
    assert_eq!(f.phase(), Phase::Idle);
    assert!(f.printer.status().unwrap().ams_unit(0).unwrap().is_drying());
    // The verdict stands, whatever comes later.
    f.printer.config_mut().start_quirk = StartQuirk::None;
    assert_eq!(
        watchdog.observe(&f.printer.status().unwrap(), secs(600), &profile),
        verdict
    );

    // The same without drying: idle for no reported reason.
    f.printer.set_drying(0, 0);
    f.printer.config_mut().start_quirk = StartQuirk::AcceptedButStaysIdle;
    let mut watchdog = f.start().unwrap();
    assert_eq!(watchdog.deadline(), StartWatchdog::DEFAULT_DEADLINE);
    f.printer.advance(secs(30));
    assert_eq!(
        watchdog.observe(&f.printer.status().unwrap(), secs(30), &profile),
        StartVerdict::Waiting
    );
    f.printer.advance(secs(60));
    assert!(matches!(
        watchdog.observe(&f.printer.status().unwrap(), secs(90), &profile),
        StartVerdict::NotStarted {
            reason: NotStartedReason::StayedIdle,
            ..
        }
    ));
}

#[test]
fn the_watchdog_tells_no_reports_and_another_job_apart() {
    let mut f = fixture();
    let profile = x2d();
    let before = f.printer.status().unwrap();
    // No report arrives after the start.
    let mut silent = StartWatchdog::new("box", &before);
    assert_eq!(
        silent.observe(&before, secs(89), &profile),
        StartVerdict::Waiting
    );
    assert!(matches!(
        silent.observe(&before, secs(90), &profile),
        StartVerdict::NotStarted {
            reason: NotStartedReason::NoReports,
            ..
        }
    ));
    // The printer works, but on a job with another name.
    let mut other = StartWatchdog::new("something else", &before);
    f.start().unwrap();
    let busy = f.printer.status().unwrap();
    assert_eq!(
        other.observe(&busy, secs(10), &profile),
        StartVerdict::Waiting
    );
    assert!(matches!(
        other.observe(&busy, secs(90), &profile),
        StartVerdict::NotStarted {
            reason: NotStartedReason::OtherJob,
            ..
        }
    ));
}

#[test]
fn outside_developer_mode_status_is_reported_and_the_start_is_refused_visibly() {
    let mut f = fixture();
    f.printer.config_mut().developer_mode = false;
    let profile = x2d();
    assert_eq!(f.phase(), Phase::Idle, "status still arrives");
    let mut watchdog = f.start().unwrap();
    let status = f.printer.status().unwrap();
    assert_eq!(status.phase(), Phase::Idle);
    let StartVerdict::Failed { problems } = watchdog.observe(&status, secs(2), &profile) else {
        panic!("expected a refusal");
    };
    assert_eq!(problems[0].meaning.as_deref(), Some("not_authorised"));
    assert!(
        problems[0].message.contains("Developer Mode"),
        "{}",
        problems[0].message
    );
    assert_eq!(problems[0].raw_code, "0502_4007");
}

#[test]
fn an_old_error_on_the_screen_is_not_blamed_on_the_new_job() {
    let mut f = fixture();
    f.start().unwrap();
    f.printer.advance(secs(200));
    f.printer.send(Command::Stop).unwrap();
    assert_eq!(f.phase(), Phase::Failed);
    // The next start is accepted and ignored; the cancelled job's error is still shown.
    f.printer.config_mut().start_quirk = StartQuirk::AcceptedButStaysIdle;
    let profile = x2d();
    let mut watchdog = f.start().unwrap();
    f.printer.advance(secs(10));
    assert_eq!(
        watchdog.observe(&f.printer.status().unwrap(), secs(10), &profile),
        StartVerdict::Waiting
    );
    f.printer.advance(secs(80));
    assert!(matches!(
        watchdog.observe(&f.printer.status().unwrap(), secs(90), &profile),
        StartVerdict::NotStarted {
            reason: NotStartedReason::StayedIdle,
            ..
        }
    ));
}

#[test]
fn uploads_need_a_usb_drive_and_an_unchanged_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_good(dir.path(), "box.gcode.3mf");
    let job = check(&path, "box", &pla_job()).unwrap();
    let mut printer = sim();
    printer.config_mut().storage_present = false;
    let error = printer.upload(&job).unwrap_err();
    assert!(error.to_string().contains("USB drive"), "{error}");
    assert_eq!(printer.status().unwrap().storage_present, Some(false));
    printer.config_mut().storage_present = true;
    std::fs::write(&path, b"something else").unwrap();
    assert!(matches!(printer.upload(&job), Err(PrinterError::Job(_))));
    assert!(printer.stored_files().is_empty());
}

#[test]
fn what_is_loaded_must_match_what_was_sliced() {
    let mut f = fixture();
    let mismatch = |result: Result<StartWatchdog, StartError>, needle: &str| match result {
        Err(StartError::Mismatch(why)) => assert!(why.contains(needle), "{why}"),
        other => panic!("expected a mismatch about {needle:?}, got {other:?}"),
    };
    // PLA from the PETG spool, from an empty tray, from an AMS that is not there, from
    // the AMS on the other nozzle, from nowhere.
    mismatch(f.start_from(&[PETG_TRAY]), "feeds nozzle id 0");
    f.printer.config_mut().ams[0].trays[1].material = None;
    mismatch(
        f.start_from(&[FilamentSource::Ams { unit: 0, tray: 1 }]),
        "empty",
    );
    mismatch(
        f.start_from(&[FilamentSource::Ams { unit: 2, tray: 0 }]),
        "no such AMS",
    );
    f.printer.config_mut().ams[0].trays[2].material = Some("PETG".to_owned());
    mismatch(
        f.start_from(&[FilamentSource::Ams { unit: 0, tray: 2 }]),
        "holds PETG",
    );
    mismatch(f.start_from(&[FilamentSource::Unused]), "no spool");
    mismatch(f.start_from(&[]), "0 source");
    mismatch(
        f.start_from(&[FilamentSource::External(254)]),
        "external spool holder",
    );
    mismatch(
        f.start_from(&[FilamentSource::External(3)]),
        "not an external",
    );
    // External spool holders feed one nozzle each: 254 the main one, 255 the auxiliary one.
    let spool = |id: u8| Tray {
        id,
        material: Some("PLA".to_owned()),
        ..Tray::default()
    };
    f.printer.config_mut().external = vec![spool(254), spool(255)];
    mismatch(
        f.start_from(&[FilamentSource::External(255)]),
        "feeds nozzle id 0",
    );
    assert!(f.printer.wire_log().is_empty(), "nothing was sent");
    assert!(f.start_from(&[FilamentSource::External(254)]).is_ok());
    f.printer.send(Command::Stop).unwrap();
    // Another PLA spool of the same AMS is fine.
    assert!(
        f.start_from(&[FilamentSource::Ams { unit: 0, tray: 3 }])
            .is_ok()
    );
}

#[test]
fn a_two_nozzle_job_is_routed_by_device_extruder_id() {
    let dir = tempfile::tempdir().unwrap();
    let job = check(
        &write_two(dir.path(), "two.gcode.3mf"),
        "two",
        &two_nozzle_job(),
    )
    .unwrap();
    let mut printer = sim();
    let status = printer.status().unwrap();
    assert_eq!(
        status.ams_unit(0).unwrap().nozzle,
        Some(1),
        "the AMS feeds the main nozzle"
    );
    let ht = status.ams_unit(128).unwrap();
    assert!(ht.high_temperature);
    assert_eq!(ht.nozzle, Some(0), "the AMS HT feeds the auxiliary nozzle");
    assert_eq!(ht.tray(0).unwrap().material.as_deref(), Some("PETG"));
    assert_eq!(
        status
            .ams_unit(0)
            .unwrap()
            .tray(2)
            .unwrap()
            .remaining_percent,
        Some(30)
    );

    let uploaded = printer.upload(&job).unwrap();
    let token = confirmed(&job, &printer);
    let swapped = start_print(
        &mut printer,
        token,
        &job,
        &uploaded,
        &[PETG_TRAY, PLA_TRAY],
        StartOptions::default(),
    );
    assert!(
        matches!(swapped, Err(StartError::Mismatch(_))),
        "{swapped:?}"
    );
    let token = confirmed(&job, &printer);
    start_print(
        &mut printer,
        token,
        &job,
        &uploaded,
        &[PLA_TRAY, PETG_TRAY],
        StartOptions::default(),
    )
    .unwrap();
    let sent: serde_json::Value = serde_json::from_str(printer.wire_log().last().unwrap()).unwrap();
    assert_eq!(sent["print"]["ams_mapping"], serde_json::json!([0, 16]));
    assert_eq!(sent["print"]["use_ams"], true);
}

#[test]
fn time_can_run_faster_than_real_time() {
    let mut f = fixture();
    f.printer.config_mut().time_scale = 20_000.0;
    f.start().unwrap();
    // 1380 simulated seconds pass in 69 real milliseconds.
    let mut phase = f.phase();
    for _ in 0..200 {
        if phase == Phase::Finished {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
        phase = f.phase();
    }
    assert_eq!(phase, Phase::Finished);
}
