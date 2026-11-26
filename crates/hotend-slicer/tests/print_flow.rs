//! From a built model to a finished job on the simulated printer, through the core's
//! facade with the stand-in slicer: prepare, slice the plate, lint, propose, review,
//! confirm, deliver, watch. Nothing here talks to a real printer or to the network.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use common::*;
use hotend_core::{
    ConfirmationAuthority, Core, CoreConfig, CoreError, CoreEvent, DeliveryChoice,
    MemorySecretStore, PrintEvent, PrintJob, PrintReview, PrinterOptions, SecretBackend, Settings,
};
use hotend_slicer::Programs;

const MODEL: &str = include_str!("../../../python/tests/models/cable_box.py");

/// The process's only confirmation authority, shared by the tests of this file.
fn authority() -> &'static ConfirmationAuthority {
    static AUTHORITY: OnceLock<ConfirmationAuthority> = OnceLock::new();
    AUTHORITY.get_or_init(|| ConfirmationAuthority::take().expect("taken once"))
}

struct Fixture {
    _tmp: tempfile::TempDir,
    core: Core,
    project: String,
}

/// The plate the person of these tests has on the printer, as the settings name it.
const PLATE: &str = "textured-pei";

/// What a dialog shows for a review: lines of its own around the review text of the
/// checked file, which it shows as it is.
fn dialog(review: &PrintReview) -> String {
    format!(
        "Start this print?\n\n{}\n\nPress Start to print.",
        review.review_text
    )
}

/// A core with the stand-in slicer and a built cable box, sliced for [`PLATE`].
///
/// Without a usable sandbox or without Hotend's Python environment these tests prove
/// nothing, so that fails the test. Only with `HOTEND_TEST_ALLOW_SKIP=1` it is skipped
/// instead (`None` after printing why).
async fn sliced(test: &str, printer: PrinterOptions) -> Option<Fixture> {
    sliced_for(test, printer, Some(PLATE)).await
}

/// As [`sliced`], for the plate the settings name, or for none.
async fn sliced_for(test: &str, printer: PrinterOptions, plate: Option<&str>) -> Option<Fixture> {
    let (tmp, core) = opened(test, printer).await?;
    core.settings_set(Settings {
        plate_type: plate.map(str::to_owned),
        // The stand-in slicer draws no plate preview; a Bambu Connect review needs one.
        previews: hotend_core::PreviewsMode::HotendMade,
        ..Settings::default()
    })
    .await
    .unwrap();
    let project = core.project_create("Cable box").await.unwrap();
    let built = core
        .model_build(&project.id, MODEL, Default::default(), "")
        .await
        .unwrap();
    assert!(built.version.is_some(), "{:?}", built.result.error);
    let outcome = core
        .slice_run(&project.id, "demo-0.20mm", None)
        .await
        .unwrap();
    // The slice is recorded with the plate the settings name and with the nozzle sizes of
    // the printer profile; the stand-in sliced for that plate, or for its own when the
    // settings name none.
    assert_eq!(outcome.record.job["plate_type"], serde_json::json!(plate));
    assert_eq!(
        outcome.record.job["nozzles"],
        serde_json::json!([{"id": 1, "diameter_mm": 0.4}, {"id": 0, "diameter_mm": 0.4}])
    );
    let bed = outcome.summary.bed.as_ref().unwrap();
    assert_eq!(bed.id, plate.unwrap_or("textured-pei"));
    // The slicer read the prepared plate, with the lid turned over.
    assert!(outcome.preparation.dir.join("plates/1/plate.3mf").is_file());
    assert!(
        outcome
            .output_path
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("input/plate.3mf")
            .is_file()
    );
    let lid = &outcome.preparation.orientations[1];
    assert_eq!(
        (lid.part.as_str(), lid.turned.as_str()),
        ("lid", "upside down")
    );
    Some(Fixture {
        _tmp: tmp,
        core,
        project: project.id,
    })
}

/// A core with the stand-in slicer and nothing in it.
async fn opened(test: &str, printer: PrinterOptions) -> Option<(tempfile::TempDir, Core)> {
    if !sandbox_ready(test) {
        return None;
    }
    let runtime = match hotend_core::PythonRuntime::discover().await {
        Ok(runtime) => runtime,
        Err(error) => {
            let problem = format!("no Python environment ({error})");
            if std::env::var_os("HOTEND_TEST_ALLOW_SKIP").is_some_and(|v| v == "1") {
                println!("SKIPPED {test}: {problem}");
                return None;
            }
            panic!("{test} cannot run: {problem} (set HOTEND_TEST_ALLOW_SKIP=1 to skip instead)");
        }
    };
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::open(CoreConfig {
        data_dir: Some(tmp.path().join("data")),
        secrets: SecretBackend::Custom(Arc::new(MemorySecretStore::default())),
        runtime: Some(runtime),
        printer,
        ..CoreConfig::default()
    })
    .await
    .unwrap();
    hotend_slicer::install(&core, Programs::StandIn).unwrap();
    Some((tmp, core))
}

fn fast() -> PrinterOptions {
    PrinterOptions {
        time_scale: 20_000.0,
        poll_interval: Duration::from_millis(10),
        ..PrinterOptions::default()
    }
}

/// The print events of one job until it reaches a state that is not `starting` or
/// `printing`.
async fn follow(
    events: &mut tokio::sync::broadcast::Receiver<CoreEvent>,
    job_id: &str,
) -> (Vec<PrintEvent>, PrintJob) {
    let mut seen = Vec::new();
    let watch = async {
        loop {
            let Ok(CoreEvent::Print {
                job_id: id,
                simulated,
                event,
                ..
            }) = events.recv().await
            else {
                continue;
            };
            assert!(simulated);
            if id != job_id {
                continue;
            }
            seen.push(event.clone());
            if let PrintEvent::Status { job } = event
                && !["starting", "printing"].contains(&job.status.as_str())
            {
                return *job;
            }
        }
    };
    let last = tokio::time::timeout(Duration::from_secs(60), watch)
        .await
        .expect("the job ended");
    (seen, last)
}

#[tokio::test]
async fn a_confirmed_job_runs_on_the_simulated_printer_to_the_end() {
    let test = "a_confirmed_job_runs_on_the_simulated_printer_to_the_end";
    let Some(fixture) = sliced(test, fast()).await else {
        return;
    };
    let core = &fixture.core;

    let printers = core.printers_list().await.unwrap();
    assert_eq!(printers.len(), 1);
    assert!(printers[0].printer.simulated);
    assert_eq!(printers[0].state.phase, "idle");
    // Only the ways this build can deliver are listed: direct control of a real printer is
    // not built and has no entry.
    let deliveries = &printers[0].deliveries;
    let enabled: Vec<(&str, bool)> = deliveries
        .iter()
        .map(|d| (d.kind.as_str(), d.enabled))
        .collect();
    assert_eq!(
        enabled,
        [
            ("simulated_printer", true),
            ("bambu_connect", true),
            ("export_to_folder", true)
        ]
    );
    assert!(deliveries.iter().all(|d| d.disabled_reason.is_none()));
    // The simulated printer is described as a simulation, in its own sentences: nothing
    // about a real printer's cloud, USB drive or pause, and no sentence shared with
    // another way.
    let simulated = &deliveries[0];
    assert_eq!(simulated.name, "Simulated printer");
    assert!(
        simulated.can.iter().any(|line| line.contains("simulated")),
        "{:?}",
        simulated.can
    );
    assert!(
        simulated
            .cannot
            .iter()
            .any(|line| line.contains("nothing is heated, moved or printed")),
        "{:?}",
        simulated.cannot
    );
    assert!(simulated.needs.is_empty(), "{:?}", simulated.needs);
    let said = [&simulated.can, &simulated.cannot, &simulated.needs]
        .into_iter()
        .flatten()
        .map(|line| line.to_lowercase())
        .collect::<Vec<_>>()
        .join("\n");
    for real in ["bambu cloud", "usb", "pause", "developer mode", "upload"] {
        assert!(
            !said.contains(real),
            "`{real}` on the simulated printer: {said}"
        );
    }
    for other in &deliveries[1..] {
        assert_ne!(other.can, simulated.can, "{}", other.kind);
        assert_ne!(other.cannot, simulated.cannot, "{}", other.kind);
    }

    let job = core
        .print_propose(&fixture.project, "first print")
        .await
        .unwrap();
    // The stand-in slicer's file passes the linter, and the summary is read from the file.
    let review = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap();
    assert!(review.simulated);
    assert_eq!(review.summary.sha256, job.slice_sha256);
    assert_eq!(review.summary.plate, 1);
    assert_eq!(review.summary.filaments.len(), 1);
    assert_eq!(review.summary.filaments[0].nozzle_id, Some(1));
    // The stand-in's file passes on what it is: a file for this printer, this plate and
    // this nozzle size that prints nothing, and says so.
    let codes: Vec<&str> = review.warnings.iter().map(|w| w.code.as_str()).collect();
    // The nozzle size is assumed (there is no setting for the fitted nozzle yet), and the
    // review says so. The slice carries Hotend-made plate previews (the fixture's setting),
    // and that notice comes first.
    assert_eq!(
        codes,
        ["hotend_made_previews", "no_moves", "nozzle_size_assumed"]
    );
    assert!(
        review.warnings[0]
            .message
            .starts_with("plate previews made by Hotend")
    );
    assert!(review.warnings[2].message.contains("0.4 mm nozzle"));
    assert_eq!(
        (review.summary.estimated_seconds, review.summary.layers),
        (Some(0), None)
    );
    assert_eq!(
        (
            review.summary.filaments[0].grams,
            review.summary.filaments[0].metres
        ),
        (Some(0.0), Some(0.0))
    );
    // The review text is the printer layer's, written from the checked file: it names
    // the declared plate and nozzle size, says that nothing is printed, and ends with
    // the file's hash.
    let text = &review.review_text;
    for line in [
        "checked for the Bambu Lab X2D.",
        "The file says it was sliced for: Bambu Lab X2D.",
        "Layers: the commands lay no plastic.",
        "Filament 1: PLA on the main nozzle (id 1), 0.4 mm nozzle. The commands push 0 m of it.",
        "Build plate: Textured PEI Plate, as chosen for this job; the file was sliced for it.",
        "Printed area: the file prints nothing.",
    ] {
        assert!(text.contains(line), "`{line}` is missing in:\n{text}");
    }
    assert!(
        text.ends_with(&format!("SHA-256 of the file: {}.", job.slice_sha256)),
        "{text}"
    );
    // The PLA spool of the AMS that feeds the main nozzle was picked.
    assert_eq!(review.spools.len(), 1);
    assert_eq!(
        (review.spools[0].nozzle_id, review.spools[0].ams_unit),
        (1, 0)
    );
    assert_eq!(core.print_status(&job.id).await.unwrap().status, "pending");
    assert!(core.simulated_printer_wire_log().is_empty());

    // A confirmation is given for the text the person was shown. Anything that does not
    // contain the review text word for word is refused, and the review is spent: the job
    // must be put before the person again. The hash alone was enough once; it is not now.
    let mut one_word_changed = dialog(&review);
    one_word_changed = one_word_changed.replacen("Textured PEI Plate", "Cool Plate", 1);
    let all_but_the_last_line = review
        .review_text
        .rsplit_once('\n')
        .map(|(head, _)| head.to_owned())
        .unwrap();
    for not_shown in [
        "0".repeat(64),
        job.slice_sha256.clone(),
        String::new(),
        one_word_changed,
        all_but_the_last_line,
        // Every line is there, but not in one piece.
        review.review_text.replace('\n', "\n  "),
    ] {
        let wrong = core
            .print_confirm(authority(), &job.id, &not_shown)
            .await
            .unwrap_err();
        assert!(
            matches!(&wrong, CoreError::Print(e) if e.code() == "summary_mismatch"),
            "{wrong} for {not_shown:?}"
        );
        let spent = core
            .print_confirm(authority(), &job.id, &dialog(&review))
            .await
            .unwrap_err();
        assert!(matches!(&spent, CoreError::Print(e) if e.code() == "not_reviewed"));
        assert_eq!(core.print_status(&job.id).await.unwrap().status, "pending");
        assert!(core.simulated_printer_wire_log().is_empty());
        core.print_review(&job.id, DeliveryChoice::SimulatedPrinter)
            .await
            .unwrap();
    }

    let review = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap();
    let mut events = core.events();
    let started = core
        .print_confirm(authority(), &job.id, &dialog(&review))
        .await
        .unwrap();
    assert_eq!(started.status, "starting");
    assert_eq!(started.delivery.as_deref(), Some("simulated_printer"));
    assert!(started.detail.simulated);
    let (seen, last) = follow(&mut events, &job.id).await;
    assert_eq!(last.status, "finished", "{last:?}");
    let states: Vec<&str> = seen
        .iter()
        .filter_map(|event| match event {
            PrintEvent::Status { job } => Some(job.status.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(states, ["starting", "printing", "finished"]);
    assert!(seen.iter().any(|e| matches!(e, PrintEvent::Phase { .. })));
    assert!(
        seen.iter()
            .any(|e| matches!(e, PrintEvent::Progress { .. }))
    );
    assert_eq!(core.print_status(&job.id).await.unwrap(), last);
    // The simulated printer was sent exactly one start, after the confirmation.
    let wire = core.simulated_printer_wire_log();
    assert_eq!(
        wire.iter()
            .filter(|line| line.contains("project_file"))
            .count(),
        1,
        "{wire:?}"
    );
    // A finished job cannot be confirmed again, and the audit log has the whole story.
    assert!(
        core.print_review(&job.id, DeliveryChoice::SimulatedPrinter)
            .await
            .is_err()
    );
    let audit = core.audit_log(Some(&fixture.project), 100).await.unwrap();
    let printed: Vec<&str> = audit
        .iter()
        .rev()
        .filter(|entry| entry.kind == "print")
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(printed, ["starting", "printing", "finished"]);
}

#[tokio::test]
async fn a_printer_that_accepts_the_start_and_stays_idle_is_reported_as_not_started() {
    let test = "a_printer_that_accepts_the_start_and_stays_idle_is_reported_as_not_started";
    let options = PrinterOptions {
        stays_idle_after_start: true,
        ..fast()
    };
    let Some(fixture) = sliced(test, options).await else {
        return;
    };
    let core = &fixture.core;
    let job = core.print_propose(&fixture.project, "").await.unwrap();
    let review = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap();
    let mut events = core.events();
    core.print_confirm(authority(), &job.id, &review.review_text)
        .await
        .unwrap();
    let (_, last) = follow(&mut events, &job.id).await;
    assert_eq!(last.status, "not_started");
    let message = last.detail.message.unwrap();
    assert!(
        message.contains("idle") || message.contains("did not"),
        "{message}"
    );
}

#[tokio::test]
async fn a_job_left_running_by_a_core_that_closed_is_reported_as_failed() {
    let test = "a_job_left_running_by_a_core_that_closed_is_reported_as_failed";
    // Real time: the print cannot finish during the test.
    let slow = PrinterOptions {
        time_scale: 1.0,
        poll_interval: Duration::from_millis(10),
        ..PrinterOptions::default()
    };
    let Some(fixture) = sliced(test, slow).await else {
        return;
    };
    let Fixture {
        _tmp: tmp,
        core,
        project,
    } = fixture;
    let job = core.print_propose(&project, "").await.unwrap();
    let review = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap();
    let started = core
        .print_confirm(authority(), &job.id, &review.review_text)
        .await
        .unwrap();
    assert_eq!(started.status, "starting");
    let open = async || {
        Core::open(CoreConfig {
            data_dir: Some(tmp.path().join("data")),
            secrets: SecretBackend::Custom(Arc::new(MemorySecretStore::default())),
            ..CoreConfig::default()
        })
        .await
        .unwrap()
    };

    // Another core next to the one that runs the job leaves the job alone.
    let beside = open().await;
    let seen = beside.print_status(&job.id).await.unwrap();
    assert!(
        ["starting", "printing"].contains(&seen.status.as_str()),
        "{seen:?}"
    );
    drop(beside);

    // The core that ran it is gone, and with it the simulated printer.
    drop(core);
    let alone = open().await;
    let ended = alone.print_status(&job.id).await.unwrap();
    assert_eq!(ended.status, "failed");
    assert_eq!(ended.delivery.as_deref(), Some("simulated_printer"));
    let message = ended.detail.message.unwrap();
    assert!(message.contains("Hotend closed"), "{message}");
    let audit = alone.audit_log(Some(&project), 100).await.unwrap();
    let last = audit.iter().find(|entry| entry.kind == "print").unwrap();
    assert_eq!(last.name, "failed");
    assert_eq!(last.detail["job"], job.id.as_str());
}

#[tokio::test]
async fn a_job_can_be_handed_to_bambu_connect_or_exported_instead() {
    let test = "a_job_can_be_handed_to_bambu_connect_or_exported_instead";
    let Some(fixture) = sliced(test, fast()).await else {
        return;
    };
    let core = &fixture.core;

    let job = core
        .print_propose(&fixture.project, "hand off")
        .await
        .unwrap();
    let review = core
        .print_review(&job.id, DeliveryChoice::BambuConnect)
        .await
        .unwrap();
    assert!(!review.simulated && review.printer.is_none() && review.spools.is_empty());
    let handed = core
        .print_confirm(authority(), &job.id, &review.review_text)
        .await
        .unwrap();
    assert_eq!(handed.status, "handed_off");
    let link = handed.detail.link.unwrap();
    assert!(link.starts_with("bambu-connect://import-file?"), "{link}");
    let placed = handed.detail.path.unwrap();
    assert!(placed.starts_with(core.data_dir().join("handoff")) && placed.is_file());

    let out = tempfile::tempdir().unwrap();
    let job = core
        .print_propose(&fixture.project, "export")
        .await
        .unwrap();
    let delivery = DeliveryChoice::ExportToFolder {
        folder: out.path().to_path_buf(),
    };
    let review = core.print_review(&job.id, delivery).await.unwrap();
    let exported = core
        .print_confirm(authority(), &job.id, &review.review_text)
        .await
        .unwrap();
    assert_eq!(exported.status, "exported");
    let written = exported.detail.path.unwrap();
    assert!(written.starts_with(out.path().canonicalize().unwrap()));
    assert_eq!(sha256(&written), job.slice_sha256);
    // Neither path touched the simulated printer.
    assert!(core.simulated_printer_wire_log().is_empty());

    // A folder that does not exist fails the delivery and leaves the job pending.
    let job = core
        .print_propose(&fixture.project, "nowhere")
        .await
        .unwrap();
    let delivery = DeliveryChoice::ExportToFolder {
        folder: out.path().join("missing"),
    };
    let review = core.print_review(&job.id, delivery).await.unwrap();
    assert!(
        core.print_confirm(authority(), &job.id, &review.review_text)
            .await
            .is_err()
    );
    assert_eq!(core.print_status(&job.id).await.unwrap().status, "pending");
}

#[tokio::test]
async fn a_print_is_checked_for_the_plate_the_person_chose_and_for_no_other() {
    let test = "a_print_is_checked_for_the_plate_the_person_chose_and_for_no_other";
    // Sliced without a chosen plate: the stand-in slices for its own plate, which nobody
    // chose, so the slice cannot become a print.
    let Some(fixture) = sliced_for(test, fast(), None).await else {
        return;
    };
    let core = &fixture.core;
    let job = core.print_propose(&fixture.project, "").await.unwrap();
    let refused = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, CoreError::Print(e) if e.code() == "bed_type_not_chosen"),
        "{refused}"
    );
    assert_eq!(core.print_status(&job.id).await.unwrap().status, "pending");

    // Choosing a plate afterwards does not make the old slice one for that plate.
    let choose = async |plate: &str| {
        core.settings_set(Settings {
            plate_type: Some(plate.to_owned()),
            ..Settings::default()
        })
        .await
        .unwrap();
    };
    choose("cool").await;
    let refused = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, CoreError::Print(e) if e.code() == "bed_type_not_chosen"),
        "{refused}"
    );

    // Sliced again for the Cool Plate: the file says Cool Plate, the job declares the
    // Cool Plate, and the review says so.
    core.slice_run(&fixture.project, "demo-0.20mm", None)
        .await
        .unwrap();
    let job = core.print_propose(&fixture.project, "").await.unwrap();
    let review = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap();
    assert!(
        review
            .review_text
            .contains("Build plate: Cool Plate, as chosen for this job"),
        "{}",
        review.review_text
    );

    // The person puts another plate on the printer and says so: the file sliced for the
    // Cool Plate is not delivered for it, by any way out.
    choose("textured-pei").await;
    for delivery in [
        DeliveryChoice::SimulatedPrinter,
        DeliveryChoice::BambuConnect,
    ] {
        let refused = core.print_review(&job.id, delivery).await.unwrap_err();
        assert!(
            matches!(&refused, CoreError::Print(e) if e.code() == "plate_changed"),
            "{refused}"
        );
        let text = refused.to_string();
        assert!(
            text.contains("Cool Plate") && text.contains("`textured-pei`"),
            "{text}"
        );
    }
    // The review made before the change is not a way around it: the refused review
    // replaced it.
    let stale = core
        .print_confirm(authority(), &job.id, &review.review_text)
        .await
        .unwrap_err();
    assert!(
        matches!(&stale, CoreError::Print(e) if e.code() == "not_reviewed"),
        "{stale}"
    );
    // Nor is a review that is still held when the plate changes: reviewed for the Cool
    // Plate, then the settings change, then the confirmation arrives.
    choose("cool").await;
    let review = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap();
    choose("textured-pei").await;
    let stale = core
        .print_confirm(authority(), &job.id, &review.review_text)
        .await
        .unwrap_err();
    assert!(
        matches!(&stale, CoreError::Print(e) if e.code() == "plate_changed"),
        "{stale}"
    );
    assert_eq!(core.print_status(&job.id).await.unwrap().status, "pending");

    // A plate the slicer knows and the G-code check has no facts about can be sliced
    // for, and is not delivered.
    choose("high-temp").await;
    core.slice_run(&fixture.project, "demo-0.20mm", None)
        .await
        .unwrap();
    let job = core.print_propose(&fixture.project, "").await.unwrap();
    let refused = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, CoreError::Print(e) if e.code() == "unsupported"),
        "{refused}"
    );
    assert!(core.simulated_printer_wire_log().is_empty());
}

/// How many start commands the simulated printer was sent.
fn starts(core: &Core) -> usize {
    core.simulated_printer_wire_log()
        .iter()
        .filter(|line| line.contains("project_file"))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_that_races_a_confirmation_never_leaves_a_started_job_unwatched() {
    let test = "a_cancel_that_races_a_confirmation_never_leaves_a_started_job_unwatched";
    let Some(fixture) = sliced(test, fast()).await else {
        return;
    };
    let core = &fixture.core;
    let (mut withdrawn, mut started) = (0, 0);
    for round in 0..24u64 {
        let job = core.print_propose(&fixture.project, "race").await.unwrap();
        let review = core
            .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
            .await
            .unwrap();
        let before = starts(core);
        let confirm = {
            let (core, id, sha) = (core.clone(), job.id.clone(), review.summary.sha256.clone());
            tokio::spawn(async move { core.print_confirm(authority(), &id, &sha).await })
        };
        let cancel = {
            let (core, id) = (core.clone(), job.id.clone());
            tokio::spawn(async move {
                // Another point of the confirmation each round.
                tokio::time::sleep(Duration::from_micros(round * 150)).await;
                core.print_cancel(&id).await
            })
        };
        let confirmed = confirm.await.unwrap();
        let cancelled = cancel.await.unwrap();
        let sent = starts(core) - before;
        assert!(sent <= 1, "round {round}");
        assert_eq!(
            confirmed.is_ok(),
            sent == 1,
            "round {round}: the confirmation answered {confirmed:?}, starts sent: {sent}"
        );

        // Whatever the order, the job comes to rest, and what it says is what happened.
        let mut last = core.print_status(&job.id).await.unwrap();
        for _ in 0..600 {
            if !["starting", "printing"].contains(&last.status.as_str()) {
                break;
            }
            // A stop that reached the printer before the start is repeated, as a person
            // would press Cancel again.
            let _ = core.print_cancel(&job.id).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
            last = core.print_status(&job.id).await.unwrap();
        }
        let message = last.detail.message.clone().unwrap_or_default();
        if sent == 0 {
            // Nothing was started: the cancel came first and withdrew the job.
            assert!(cancelled.is_ok(), "round {round}: {cancelled:?}");
            assert_eq!(last.status, "cancelled", "round {round}: {last:?}");
            assert_eq!(message, "withdrawn before it was confirmed");
            assert!(!last.detail.simulated && last.delivery.is_none());
            withdrawn += 1;
        } else {
            // The printer was started: the job was followed to its end on the printer and
            // is recorded as a job that was sent there (a stop before the first layer
            // reads `failed` or `not_started`), never as withdrawn before it began.
            assert!(
                ["cancelled", "finished", "failed", "not_started"].contains(&last.status.as_str()),
                "round {round}: {last:?}"
            );
            assert_ne!(message, "withdrawn before it was confirmed", "{last:?}");
            assert!(last.detail.simulated, "round {round}: {last:?}");
            assert_eq!(last.delivery.as_deref(), Some("simulated_printer"));
            started += 1;
        }
        // The printer is free again for the next round.
        for _ in 0..600 {
            let phase = core.printers_list().await.unwrap()[0].state.phase.clone();
            if ["idle", "finished", "failed"].contains(&phase.as_str()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    eprintln!("{test}: {withdrawn} round(s) withdrawn first, {started} started first");
    assert_eq!(withdrawn + started, 24);
}

#[tokio::test]
async fn a_job_proposed_before_the_version_was_laid_out_again_is_not_reviewed() {
    let test = "a_job_proposed_before_the_version_was_laid_out_again_is_not_reviewed";
    let Some(fixture) = sliced(test, fast()).await else {
        return;
    };
    let core = &fixture.core;
    let job = core.print_propose(&fixture.project, "old").await.unwrap();
    // The job can be reviewed as long as the layout is the one that was sliced; the review
    // names the plate of the layout and its pieces.
    let review = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap();
    assert_eq!(review.plate, 1);
    assert_eq!(review.pieces, ["box", "lid"]);

    let options = hotend_core::PrepareOptions {
        copies: [("lid".to_owned(), 2)].into(),
        ..Default::default()
    };
    core.prepare_run(&fixture.project, None, options)
        .await
        .unwrap();
    assert!(
        core.slice_get(&fixture.project, None)
            .await
            .unwrap()
            .is_none()
    );
    let error = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap_err();
    let CoreError::Print(inner) = &error else {
        panic!("{error:?}");
    };
    assert_eq!(inner.code(), "slice_changed", "{error}");
    assert!(error.to_string().contains("laid out"), "{error}");
    // A review that was on the screen while the version was laid out again cannot be
    // confirmed either.
    let sliced = core
        .slice_run(&fixture.project, "demo-0.20mm", None)
        .await
        .unwrap();
    let job = core.print_propose(&fixture.project, "new").await.unwrap();
    let review = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap();
    assert_eq!(review.summary.sha256, sliced.summary.sha256);
    core.prepare_run(&fixture.project, None, Default::default())
        .await
        .unwrap();
    let error = core
        .print_confirm(authority(), &job.id, &review.summary.sha256)
        .await
        .unwrap_err();
    let CoreError::Print(inner) = &error else {
        panic!("{error:?}");
    };
    assert_eq!(inner.code(), "slice_changed", "{error}");
    assert_eq!(core.print_status(&job.id).await.unwrap().status, "pending");
    assert!(core.simulated_printer_wire_log().is_empty());
}

#[tokio::test]
async fn the_review_names_the_plate_of_the_layout_not_the_plate_inside_the_file() {
    let test = "the_review_names_the_plate_of_the_layout_not_the_plate_inside_the_file";
    let Some(fixture) = sliced(test, fast()).await else {
        return;
    };
    let core = &fixture.core;
    let options = hotend_core::PrepareOptions {
        copies: [("box".to_owned(), 12), ("lid".to_owned(), 12)].into(),
        ..Default::default()
    };
    let layout = core
        .prepare_run(&fixture.project, None, options)
        .await
        .unwrap();
    assert!(layout.plates.len() >= 2, "{} plate(s)", layout.plates.len());
    let sliced = core
        .slice_run(&fixture.project, "demo-0.20mm", Some(2))
        .await
        .unwrap();
    assert_eq!(sliced.record.job["plate"], 2);
    let job = core
        .print_propose(&fixture.project, "plate 2")
        .await
        .unwrap();
    let review = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap();
    assert_eq!(review.plate, 2);
    let on_plate: Vec<&str> = layout.plates[1]
        .items
        .iter()
        .map(|item| item.name.as_str())
        .collect();
    assert_eq!(review.pieces, on_plate);
    // The sliced file holds one plate, its first.
    assert_eq!(review.summary.plate, 1);
}

#[tokio::test]
async fn a_layout_in_which_nothing_fits_is_stored_and_says_why() {
    let test = "a_layout_in_which_nothing_fits_is_stored_and_says_why";
    let Some((_tmp, core)) = opened(test, fast()).await else {
        return;
    };
    let shown =
        |error: CoreError| serde_json::to_value(hotend_core::ApiError::from(&error)).unwrap();
    let slab = |side: f64| {
        format!(
            "from build123d import *\n\nPARAMS = {{}}\n\n\ndef build(p, ctx):\n    \
             bottom = (Align.CENTER, Align.CENTER, Align.MIN)\n    \
             return {{\"slab\": Box({side:.1}, {side:.1}, 5.0, align=bottom)}}\n"
        )
    };

    // 254 x 254 mm: inside the main nozzle's 256 mm reach as drawn, outside it with the
    // 2 mm margin a plate keeps. The version exists and fails its build-volume check.
    let project = core.project_create("Slab").await.unwrap();
    let built = core
        .model_build(&project.id, &slab(254.0), Default::default(), "")
        .await
        .unwrap();
    let version = built
        .version
        .unwrap_or_else(|| panic!("{:?}", built.result.error))
        .number;
    let layout = core
        .prepare_run(&project.id, None, Default::default())
        .await
        .unwrap();
    assert_eq!(layout.status, hotend_core::EngineStatus::CheckError);
    assert!(layout.plates.is_empty());
    assert_eq!(layout.unplaced.len(), 1);
    assert!(
        layout.unplaced[0].reason.contains("larger than"),
        "{:?}",
        layout.unplaced
    );
    // It is stored: the window shows it after a reload.
    let stored = core
        .preparation_get(&project.id, version)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.unplaced, layout.unplaced);
    let refused = shown(
        core.slice_run(&project.id, "demo-0.20mm", None)
            .await
            .unwrap_err(),
    );
    assert_eq!(refused["kind"], "slicer", "{refused}");

    // A version that passed its checks on the printer it was built for, sliced after the
    // settings moved to a printer with a smaller bed: no plate, and the answer says which
    // piece and what to do about it.
    let project = core.project_create("Smaller bed").await.unwrap();
    let built = core
        .model_build(&project.id, &slab(240.0), Default::default(), "")
        .await
        .unwrap();
    assert_eq!(
        built.result.status,
        hotend_core::EngineStatus::Ok,
        "{:?}",
        built.result.checks
    );
    let mut settings = core.settings_get().await.unwrap();
    settings.printer_id = "prusa-mk4s".to_owned();
    core.settings_set(settings).await.unwrap();
    let refused = shown(
        core.slice_run(&project.id, "demo-0.20mm", None)
            .await
            .unwrap_err(),
    );
    assert_eq!(refused["kind"], "slicer", "{refused}");
    assert_eq!(refused["code"], "no_plate", "{refused}");
    let message = refused["message"].as_str().unwrap();
    assert!(
        message.contains("nothing could be placed") && message.contains("`slab`"),
        "{message}"
    );
    assert!(
        refused["next_step"].as_str().unwrap().contains("smaller"),
        "{refused}"
    );
}

#[tokio::test]
async fn a_version_built_for_another_material_says_so_when_it_is_prepared() {
    let test = "a_version_built_for_another_material_says_so_when_it_is_prepared";
    let Some(fixture) = sliced(test, fast()).await else {
        return;
    };
    let core = &fixture.core;
    // Prepared and sliced for what it was built for: nothing to say.
    let first = core
        .slice_get(&fixture.project, None)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !first
            .preparation
            .warnings
            .iter()
            .any(|w| w.contains("built and checked")),
        "{:?}",
        first.preparation.warnings
    );

    let mut settings = core.settings_get().await.unwrap();
    settings.material.id = "tpu-95a".to_owned();
    settings.material.family = "TPU".to_owned();
    core.settings_set(settings).await.unwrap();
    let again = core
        .slice_run(&fixture.project, "demo-0.20mm", None)
        .await
        .unwrap();
    assert_eq!(again.preparation.material_id, "tpu-95a");
    let warning = again
        .preparation
        .warnings
        .iter()
        .find(|w| w.contains("built and checked"))
        .unwrap_or_else(|| panic!("{:?}", again.preparation.warnings));
    assert!(
        warning.contains("`pla-generic`")
            && warning.contains("`tpu-95a`")
            && warning.contains("build it again"),
        "{warning}"
    );
    // The person who confirms the print sees it too: the review, which
    // the window, the CLI and the native confirmation show, carries it as a warning.
    let job = core.print_propose(&fixture.project, "").await.unwrap();
    let review = core
        .print_review(&job.id, DeliveryChoice::BambuConnect)
        .await
        .unwrap();
    let seen = review
        .warnings
        .iter()
        .find(|finding| finding.code == "built_for_other")
        .unwrap_or_else(|| panic!("{:?}", review.warnings));
    assert!(
        seen.message.contains("`pla-generic`") && seen.message.contains("`tpu-95a`"),
        "{}",
        seen.message
    );
    core.print_cancel(&job.id).await.unwrap();

    // Built again for the new material, the warning is gone.
    core.model_build(&fixture.project, MODEL, Default::default(), "for TPU")
        .await
        .unwrap()
        .version
        .expect("the box builds for TPU");
    let rebuilt = core
        .prepare_run(&fixture.project, None, Default::default())
        .await
        .unwrap();
    assert!(
        !rebuilt
            .warnings
            .iter()
            .any(|w| w.contains("built and checked")),
        "{:?}",
        rebuilt.warnings
    );
}

#[tokio::test]
async fn a_wrong_value_is_answered_as_one_not_as_a_broken_machine() {
    let test = "a_wrong_value_is_answered_as_one_not_as_a_broken_machine";
    let Some(fixture) = sliced(test, fast()).await else {
        return;
    };
    let core = &fixture.core;
    let shown =
        |error: CoreError| serde_json::to_value(hotend_core::ApiError::from(&error)).unwrap();

    // A part the version does not have.
    let options = hotend_core::PrepareOptions {
        copies: [("handle".to_owned(), 2)].into(),
        ..Default::default()
    };
    let error = shown(
        core.prepare_run(&fixture.project, None, options)
            .await
            .unwrap_err(),
    );
    assert_eq!(error["kind"], "invalid_input", "{error}");
    assert!(
        error["message"].as_str().unwrap().contains("box, lid"),
        "{error}"
    );

    // A nozzle the printer does not have.
    let reading = hotend_core::GaugeReading {
        feature: hotend_core::GaugeFeature::Hole,
        drawn: 6.0,
        measured: 5.9,
    };
    let error = shown(
        core.calibration_record(vec![reading], Some(7))
            .await
            .unwrap_err(),
    );
    assert_eq!(error["kind"], "invalid_input", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("no nozzle with the id 7"),
        "{error}"
    );

    // What the engine itself refuses for its content is a value to correct too.
    let far = hotend_core::GaugeReading {
        feature: hotend_core::GaugeFeature::Peg,
        drawn: 6.0,
        measured: 9.0,
    };
    let error = shown(core.calibration_record(vec![far], None).await.unwrap_err());
    assert_eq!(error["kind"], "engine", "{error}");
    assert_eq!(error["code"], "bad_request", "{error}");
    assert_ne!(
        error["next_step"].as_str().unwrap_or(""),
        "Run the doctor report; it says what is missing on this machine and how to fix it.",
        "{error}"
    );
}

/// The editable Bambu Studio project is made by the real Bambu Studio only: with the
/// stand-in the facade refuses with a clear message and makes nothing, and nothing about a
/// project can be reviewed or handed over without one.
#[tokio::test]
async fn the_stand_in_makes_no_editable_project_and_nothing_can_be_handed_over() {
    let test = "the_stand_in_makes_no_editable_project_and_nothing_can_be_handed_over";
    let Some(fixture) = sliced(test, fast()).await else {
        return;
    };
    let core = &fixture.core;
    let refused = core
        .print_project_make(&fixture.project, None)
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, CoreError::Slice(e) if e.code() == "stand_in_slicer"),
        "{refused}"
    );
    let text = refused.to_string();
    assert!(
        text.contains("real Bambu Studio") && text.contains("stand-in"),
        "{text}"
    );
    assert!(
        core.print_projects(&fixture.project)
            .await
            .unwrap()
            .is_empty()
    );
    let version = core.project_get(&fixture.project).await.unwrap();
    let number = version.project.current_version.unwrap();
    let projects = core
        .data_dir()
        .join("projects")
        .join(&fixture.project)
        .join("versions")
        .join(number.to_string())
        .join("slicer-project");
    assert!(
        !projects.exists() || std::fs::read_dir(&projects).unwrap().next().is_none(),
        "nothing was made in {}",
        projects.display()
    );

    // Without a made project there is nothing to review or hand over.
    let missing = core
        .print_project_review(
            "no-such-project",
            hotend_core::ProjectDestination::BambuStudio,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&missing, CoreError::Print(e) if e.code() == "not_found"),
        "{missing}"
    );
    let unreviewed = core
        .print_project_deliver(authority(), "no-such-project", "anything")
        .await
        .unwrap_err();
    assert!(
        matches!(&unreviewed, CoreError::Print(e) if e.code() == "not_reviewed"),
        "{unreviewed}"
    );
    assert!(core.simulated_printer_wire_log().is_empty());
}

/// A proven baseline for the printer, plate and material of the settings, as a holder
/// print that held is recorded, with the brim width `brim_mm`.
fn print_baseline(settings: &Settings, brim_mm: i64) -> hotend_core::ProvenBaseline {
    hotend_core::ProvenBaseline {
        printer_id: settings.printer_id.clone(),
        plate_type: settings.plate_type.clone().unwrap(),
        material_id: settings.material.id.clone(),
        printed_on: "2026-11-17".to_owned(),
        source: "holder print, reported good".to_owned(),
        values: hotend_core::BaselineFields {
            level: 1,
            brim_kind: "outer".to_owned(),
            brim_width_mm: brim_mm,
            first_layer_wall_speed_mm_s: 20,
            first_layer_infill_speed_mm_s: 30,
            first_layer_bed_c: 65,
            layers_bed_c: 60,
            part_fan_off_layers: 3,
            aux_fan_first_layers_pct: 0,
            aux_fan_max_pct: 30,
            wall_loops: 3,
        },
        recorded_at_ms: 0,
    }
}

fn code_of(error: &CoreError) -> &'static str {
    match error {
        CoreError::Print(inner) => inner.code(),
        other => panic!("not a print error: {other:?}"),
    }
}

/// A print job is reviewed and confirmed only while the plate's print plan is the one it
/// was sliced with: a proven baseline recorded or changed after the job was proposed (or
/// while its review is on the screen) refuses the review and the confirmation. A slice
/// made with a proven baseline is current: the plan is computed with the baseline
/// everywhere, `plan_is_current` included.
#[tokio::test]
async fn a_job_whose_print_plan_changed_is_not_reviewed_or_confirmed() {
    let test = "a_job_whose_print_plan_changed_is_not_reviewed_or_confirmed";
    let Some(fixture) = sliced(test, fast()).await else {
        return;
    };
    let core = &fixture.core;
    let project = fixture.project.as_str();
    let first = core.slice_get(project, None).await.unwrap().unwrap();
    let version = first.record.version;
    let plan = first.record.job["plan_sha256"]
        .as_str()
        .expect("the slice was made with Hotend's print plan")
        .to_owned();
    assert!(
        core.gateway()
            .plan_is_current(project, version, 1, &plan)
            .unwrap()
    );
    let job = core.print_propose(project, "before").await.unwrap();
    let review = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap();

    // A proven baseline is recorded while the review is on the screen: the plan changes,
    // and the reviewed file is not what Hotend would print now.
    let settings = core.settings_get().await.unwrap();
    core.baseline_set(print_baseline(&settings, 5))
        .await
        .unwrap();
    assert!(
        !core
            .gateway()
            .plan_is_current(project, version, 1, &plan)
            .unwrap()
    );
    let refused = core
        .print_confirm(authority(), &job.id, &dialog(&review))
        .await
        .unwrap_err();
    assert_eq!(code_of(&refused), "plan_changed", "{refused}");
    assert!(
        refused
            .to_string()
            .contains("print settings of this plate changed"),
        "{refused}"
    );
    assert_eq!(core.print_status(&job.id).await.unwrap().status, "pending");
    // The refused confirmation used the review up, and the job is not reviewed again.
    let again = core
        .print_confirm(authority(), &job.id, &dialog(&review))
        .await
        .unwrap_err();
    assert_eq!(code_of(&again), "not_reviewed", "{again}");
    let refused = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap_err();
    assert_eq!(code_of(&refused), "plan_changed", "{refused}");
    let refused = core
        .print_review(&job.id, DeliveryChoice::BambuConnect)
        .await
        .unwrap_err();
    assert_eq!(code_of(&refused), "plan_changed", "{refused}");

    // Sliced again with the baseline: that plan is current, and its job is reviewed.
    let second = core.slice_run(project, "demo-0.20mm", None).await.unwrap();
    let with_baseline = second.record.job["plan_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(with_baseline, plan);
    assert!(
        core.gateway()
            .plan_is_current(project, version, 1, &with_baseline)
            .unwrap()
    );
    let job = core
        .print_propose(project, "with the baseline")
        .await
        .unwrap();
    let review = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap();
    // The baseline changes between the review and the confirmation: refused.
    core.baseline_set(print_baseline(&settings, 6))
        .await
        .unwrap();
    let refused = core
        .print_confirm(authority(), &job.id, &dialog(&review))
        .await
        .unwrap_err();
    assert_eq!(code_of(&refused), "plan_changed", "{refused}");
    assert_eq!(core.print_status(&job.id).await.unwrap().status, "pending");
    assert!(core.simulated_printer_wire_log().is_empty());
}

/// An editable project is reviewed and handed over only while its plan is the plate's plan
/// with the proven baseline: one made with a baseline is reviewed, and a baseline changed
/// between the review and the hand-over refuses it (`project_stale`), with nothing written.
/// The stand-in makes no project, so the project is recorded here as `print_project_make`
/// records one, with the plan's own sliced file as its test slice.
#[tokio::test]
async fn an_editable_project_whose_plan_changed_is_not_handed_over() {
    let test = "an_editable_project_whose_plan_changed_is_not_handed_over";
    let Some(fixture) = sliced(test, fast()).await else {
        return;
    };
    let core = &fixture.core;
    let project = fixture.project.as_str();
    let settings = core.settings_get().await.unwrap();
    core.baseline_set(print_baseline(&settings, 5))
        .await
        .unwrap();
    let sliced = core.slice_run(project, "demo-0.20mm", None).await.unwrap();
    let plan = sliced.record.job["plan_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let version = sliced.record.version;

    let version_dir = sliced.preparation.dir.parent().unwrap().to_path_buf();
    let folder = format!("1-{}", &plan[..16]);
    let project_dir = version_dir.join("slicer-project").join(&folder);
    std::fs::create_dir_all(&project_dir).unwrap();
    let project_bytes = b"PK\x03\x04 an editable project".to_vec();
    std::fs::write(project_dir.join("project.3mf"), &project_bytes).unwrap();
    std::fs::copy(&sliced.output_path, project_dir.join("test.gcode.3mf")).unwrap();
    let mut test_slice = sliced.summary.clone();
    test_slice.output = "test.gcode.3mf".to_owned();
    let project_sha256 = hotend_core::util::sha256_hex(&project_bytes);
    let export = hotend_core::ProjectExportSummary {
        project: "project.3mf".to_owned(),
        sha256: project_sha256.clone(),
        keys: hotend_core::ProjectKeys::default(),
        slice: test_slice,
        proof: "passed".to_owned(),
        stand_in: false,
    };
    let id = hotend_core::util::new_id();
    let data = hotend_core::paths::DataDir::resolve(Some(core.data_dir())).unwrap();
    let store = hotend_core::store::Store::open(&data.db_path()).unwrap();
    store
        .put_project_export(&hotend_core::store::ProjectExportRecord {
            id: id.clone(),
            project_id: project.to_owned(),
            version,
            plate: 1,
            plan_sha256: plan.clone(),
            profile_hash: sliced.record.profile_hash.clone(),
            slice_sha256: sliced.summary.sha256.clone(),
            folder,
            sha256: project_sha256,
            summary: serde_json::json!(export),
            delivery: None,
            detail: serde_json::json!({}),
            created_at_ms: 1,
            updated_at_ms: 1,
        })
        .unwrap();

    // The project's plan, made with the baseline, is current: it is reviewed.
    let out = tempfile::tempdir().unwrap();
    let destination = hotend_core::ProjectDestination::Folder {
        folder: out.path().to_path_buf(),
    };
    let review = core
        .print_project_review(&id, destination.clone())
        .await
        .unwrap();
    assert!(
        review
            .review_text
            .contains("Where it goes: saved in the folder")
    );

    // The baseline changes before the hand-over: refused, nothing written.
    core.baseline_set(print_baseline(&settings, 6))
        .await
        .unwrap();
    let shown = format!("Save this project?\n\n{}\n", review.review_text);
    let refused = core
        .print_project_deliver(authority(), &id, &shown)
        .await
        .unwrap_err();
    assert_eq!(code_of(&refused), "project_stale", "{refused}");
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
    let refused = core
        .print_project_review(&id, destination)
        .await
        .unwrap_err();
    assert_eq!(code_of(&refused), "project_stale", "{refused}");
    // The review was used up: with the baseline the project was made with restored, its
    // plan is the plate's plan again, and the old confirmation is still refused, as not
    // reviewed.
    core.baseline_set(print_baseline(&settings, 5))
        .await
        .unwrap();
    assert!(
        core.gateway()
            .plan_is_current(project, version, 1, &plan)
            .unwrap(),
        "restoring the baseline restores the plan"
    );
    let refused = core
        .print_project_deliver(authority(), &id, &shown)
        .await
        .unwrap_err();
    assert_eq!(code_of(&refused), "not_reviewed", "{refused}");
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
}
