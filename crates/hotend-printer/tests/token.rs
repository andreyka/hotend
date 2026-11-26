//! The start token: a print starts only with a human confirmation of this file on this
//! printer, once, in time.
#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use common::{
    GOOD_GCODE, PLA_TRAY, check, click, confirmed, dialog, good_slice_info, pla_job, sim,
    slice_info, write_good, write_sliced, x2d,
};
use hotend_printer::ident::Serial;
use hotend_printer::{
    ConfirmationAuthority, ConfirmationRequest, Phase, Printer, SimConfig, SimulatedPrinter,
    StartError, StartOptions, StartToken, TokenError, start_print,
};

fn start(
    printer: &mut SimulatedPrinter,
    token: StartToken,
    job: &hotend_printer::CheckedJob,
    uploaded: &hotend_printer::UploadedJob,
) -> Result<hotend_printer::StartWatchdog, StartError> {
    start_print(
        printer,
        token,
        job,
        uploaded,
        &[PLA_TRAY],
        StartOptions::default(),
    )
}

/// No start command reached the printer.
fn nothing_started(printer: &mut SimulatedPrinter) {
    assert!(
        printer
            .wire_log()
            .iter()
            .all(|line| !line.contains("project_file")),
        "{:?}",
        printer.wire_log()
    );
    assert_eq!(printer.status().unwrap().phase(), Phase::Idle);
}

#[test]
fn a_confirmed_job_starts() {
    let dir = tempfile::tempdir().unwrap();
    let job = check(&write_good(dir.path(), "box.gcode.3mf"), "box", &pla_job()).unwrap();
    let mut printer = sim();
    let uploaded = printer.upload(&job).unwrap();
    let token = confirmed(&job, &printer);
    assert_eq!(token.sha256(), job.sha256());
    assert_eq!(token.printer(), &printer.identity().serial);
    assert!(token.expires_in() <= StartToken::DEFAULT_TTL);
    start(&mut printer, token, &job, &uploaded).unwrap();
    assert_eq!(printer.status().unwrap().phase(), Phase::Preparing);
    let sent = printer.wire_log().last().unwrap();
    assert!(sent.contains("\"command\":\"project_file\""), "{sent}");
    assert!(sent.contains("Metadata/plate_1.gcode"), "{sent}");
}

#[test]
fn the_dialog_shows_what_the_file_says() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_sliced(
        dir.path(),
        "hot.gcode.3mf",
        &GOOD_GCODE.replace("S220", "S228"),
        &slice_info(1500, &[("PLA", "#FF0000", 0.04)], "1"),
    );
    // The label is the only free text a caller controls; it is not part of the summary.
    let job = check(
        &path,
        "harmless 2 minute test at 180 C with 1 g of PLA",
        &pla_job(),
    )
    .unwrap();
    let printer = sim();
    let request = ConfirmationRequest::start_print(&job, printer.identity());
    let summary = request.summary();
    assert_eq!(summary, job.summary());
    assert_eq!(summary.max_nozzle_c, Some(228.0));
    assert_eq!(summary.max_bed_c, Some(35.0));
    assert_eq!(summary.estimated_seconds, Some(1500));
    assert_eq!(summary.plate, 1);
    assert_eq!(summary.filaments[0].grams, Some(0.04));
    assert_eq!(summary.filaments[0].material.as_deref(), Some("PLA"));
    assert_eq!(summary.filaments[0].nozzle, Some(1));
    assert_eq!(summary.sha256, job.sha256());
    assert_eq!(request.printer(), Some(printer.identity()));
    let shown = serde_json::to_string(summary).unwrap();
    assert!(
        !shown.contains("harmless") && !shown.contains("180"),
        "{shown}"
    );
    let text = request.review_text();
    assert!(
        !text.contains("harmless") && !text.contains("180"),
        "{text}"
    );
    assert!(text.contains("up to 228 C"));
}

/// The confirmation is of the text that was displayed: the dialog hands in what it drew,
/// and anything but the request's own text confirms nothing.
#[test]
fn a_confirmation_is_of_the_text_that_was_shown() {
    let dir = tempfile::tempdir().unwrap();
    let job = check(&write_good(dir.path(), "box.gcode.3mf"), "box", &pla_job()).unwrap();
    let printer = sim();
    let request = || ConfirmationRequest::start_print(&job, printer.identity());
    let text = request().review_text();
    for shown in [
        String::new(),
        "Print a small box. It takes two minutes.".to_owned(),
        text.replace("up to 220 C", "up to 190 C"),
        text.lines().take(3).collect::<Vec<_>>().join("\n"),
        format!("{text}\n"),
        serde_json::to_string(request().summary()).unwrap(),
    ] {
        let error = dialog().confirm(request(), &shown).unwrap_err();
        assert!(matches!(error, TokenError::NotWhatWasShown), "{error}");
    }
    let confirmation = dialog().confirm(request(), &text).unwrap();
    // What was confirmed can be written down: the file, the text, the plate, the name
    // on the printer and the printer.
    let record = confirmation.record();
    assert_eq!(record.sha256, job.sha256());
    assert_eq!(record.review_sha256, request().review_sha256());
    assert_eq!(record.plate, 1);
    assert_eq!(record.remote_name, job.remote_name());
    assert_eq!(record.printer.as_ref(), Some(&printer.identity().serial));
    assert!(StartToken::mint(confirmation, StartToken::DEFAULT_TTL).is_ok());
}

/// A token is for one job: the file, the plate, the declaration it was held to, the text
/// that was shown and the name on the printer. Another job of the very same file does
/// not start with it.
#[test]
fn a_token_for_another_job_of_the_same_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_good(dir.path(), "box.gcode.3mf");
    let confirmed_job = check(&path, "box", &pla_job()).unwrap();
    // The same bytes under another label: another name on the printer.
    let relabelled = check(&path, "other label", &pla_job()).unwrap();
    // The same bytes held to another declaration (an alias of the same family).
    let mut alias = pla_job();
    alias.filaments[0].material = "PLA Basic".to_owned();
    let redeclared = check(&path, "box", &alias).unwrap();
    assert_eq!(confirmed_job.sha256(), relabelled.sha256());
    assert_eq!(confirmed_job.sha256(), redeclared.sha256());
    assert_eq!(confirmed_job.remote_name(), redeclared.remote_name());

    for other in [&relabelled, &redeclared] {
        let mut printer = sim();
        let uploaded = printer.upload(other).unwrap();
        let token = confirmed(&confirmed_job, &printer);
        let error = start(&mut printer, token, other, &uploaded).unwrap_err();
        assert!(
            matches!(error, StartError::Token(TokenError::WrongJob)),
            "{error}"
        );
        nothing_started(&mut printer);
    }
    // The upload of one job is not the upload of another, even of the same bytes.
    let mut printer = sim();
    let uploaded_other = printer.upload(&relabelled).unwrap();
    let token = confirmed(&confirmed_job, &printer);
    let error = start(&mut printer, token, &confirmed_job, &uploaded_other).unwrap_err();
    assert!(matches!(error, StartError::NotUploaded), "{error}");
    nothing_started(&mut printer);
}

/// Two different files never share a name on the printer, so a later upload of another
/// job with the same label cannot take the place of the file that was confirmed.
#[test]
fn a_second_job_with_the_same_label_does_not_replace_the_confirmed_file() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_good(dir.path(), "a.gcode.3mf");
    let b = write_sliced(
        dir.path(),
        "b.gcode.3mf",
        &GOOD_GCODE.replace("S220", "S238").replace("S35", "S44"),
        &good_slice_info(),
    );
    let job_a = check(&a, "bracket-v3", &pla_job()).unwrap();
    let job_b = check(&b, "bracket-v3", &pla_job()).unwrap();
    assert_ne!(job_a.sha256(), job_b.sha256());
    assert_ne!(job_a.remote_name(), job_b.remote_name());
    assert!(job_a.remote_name().starts_with("bracket-v3-"));

    let mut printer = sim();
    let uploaded_a = printer.upload(&job_a).unwrap();
    let uploaded_b = printer.upload(&job_b).unwrap();
    // Both are on the printer, each under its own name with its own content.
    assert_eq!(
        printer.stored_sha256(&job_a.remote_name()),
        Some(job_a.sha256())
    );
    assert_eq!(
        printer.stored_sha256(&job_b.remote_name()),
        Some(job_b.sha256())
    );
    // A's confirmation does not start B, with either upload.
    let token = confirmed(&job_a, &printer);
    let error = start(&mut printer, token, &job_b, &uploaded_b).unwrap_err();
    assert!(
        matches!(error, StartError::Token(TokenError::WrongFile)),
        "{error}"
    );
    let token = confirmed(&job_a, &printer);
    let error = start(&mut printer, token, &job_a, &uploaded_b).unwrap_err();
    assert!(matches!(error, StartError::NotUploaded), "{error}");
    nothing_started(&mut printer);
    // And A's confirmation starts A: the command names A's file, which holds A.
    let token = confirmed(&job_a, &printer);
    start(&mut printer, token, &job_a, &uploaded_a).unwrap();
    let sent = printer.wire_log().last().unwrap().clone();
    assert!(sent.contains(&job_a.remote_name()), "{sent}");
    assert!(!sent.contains(&job_b.remote_name()), "{sent}");
    printer.advance(Duration::from_secs(600));
    let status = printer.status().unwrap();
    assert!(
        status
            .nozzles
            .values()
            .all(|nozzle| nozzle.target.unwrap_or(0.0) <= 220.0),
        "the printer heats to what was confirmed: {status:?}"
    );
}

#[test]
fn a_token_for_another_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let confirmed_job = check(&write_good(dir.path(), "a.gcode.3mf"), "a", &pla_job()).unwrap();
    let other = write_sliced(
        dir.path(),
        "b.gcode.3mf",
        &GOOD_GCODE.replace("S220", "S229"),
        &good_slice_info(),
    );
    let other_job = check(&other, "b", &pla_job()).unwrap();
    let mut printer = sim();
    let uploaded = printer.upload(&other_job).unwrap();
    let token = confirmed(&confirmed_job, &printer);
    let error = start(&mut printer, token, &other_job, &uploaded).unwrap_err();
    assert!(
        matches!(error, StartError::Token(TokenError::WrongFile)),
        "{error}"
    );
    nothing_started(&mut printer);
}

#[test]
fn a_token_for_another_printer_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let job = check(&write_good(dir.path(), "box.gcode.3mf"), "box", &pla_job()).unwrap();
    let mut first = sim();
    let mut config = SimConfig::x2d(&x2d());
    config.identity.serial = Serial::new("20P0SIM00000002").unwrap();
    let mut second = SimulatedPrinter::new(x2d(), config);
    let uploaded = second.upload(&job).unwrap();
    let token = confirmed(&job, &first);
    let error = start(&mut second, token, &job, &uploaded).unwrap_err();
    assert!(
        matches!(error, StartError::Token(TokenError::WrongPrinter)),
        "{error}"
    );
    nothing_started(&mut second);
    nothing_started(&mut first);
}

#[test]
fn an_expired_token_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let job = check(&write_good(dir.path(), "box.gcode.3mf"), "box", &pla_job()).unwrap();
    let mut printer = sim();
    let uploaded = printer.upload(&job).unwrap();
    let request = ConfirmationRequest::start_print(&job, printer.identity());
    let token = StartToken::mint(click(request), Duration::from_millis(20)).unwrap();
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(token.expires_in(), Duration::ZERO);
    let error = start(&mut printer, token, &job, &uploaded).unwrap_err();
    assert!(
        matches!(error, StartError::Token(TokenError::Expired)),
        "{error}"
    );
    nothing_started(&mut printer);

    // A lifetime cannot be stretched past the maximum.
    let request = ConfirmationRequest::start_print(&job, printer.identity());
    let token = StartToken::mint(click(request), Duration::from_secs(86_400)).unwrap();
    assert!(token.expires_in() <= StartToken::MAX_TTL);
}

#[test]
fn a_file_swapped_after_the_confirmation_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_good(dir.path(), "box.gcode.3mf");
    let job = check(&path, "box", &pla_job()).unwrap();
    let mut printer = sim();
    let uploaded = printer.upload(&job).unwrap();
    let token = confirmed(&job, &printer);
    let other = write_sliced(
        dir.path(),
        "other.gcode.3mf",
        &GOOD_GCODE.replace("S220", "S229"),
        &good_slice_info(),
    );
    std::fs::copy(&other, &path).unwrap();
    let error = start(&mut printer, token, &job, &uploaded).unwrap_err();
    assert!(
        matches!(error, StartError::Token(TokenError::FileChanged)),
        "{error}"
    );
    nothing_started(&mut printer);

    // A file that is gone cannot be checked, so nothing starts either.
    let token = confirmed(&job, &printer);
    std::fs::remove_file(&path).unwrap();
    let error = start(&mut printer, token, &job, &uploaded).unwrap_err();
    assert!(
        matches!(error, StartError::Token(TokenError::Unreadable(_))),
        "{error}"
    );
    nothing_started(&mut printer);
}

#[test]
fn a_token_is_spent_by_its_one_use() {
    // Using a token twice does not compile (see the `compile_fail` examples in
    // `hotend_printer::token`); what can be shown at run time is that a second start
    // needs a second confirmation, and that the first token is gone even when the start
    // it was spent on failed.
    let dir = tempfile::tempdir().unwrap();
    let job = check(&write_good(dir.path(), "box.gcode.3mf"), "box", &pla_job()).unwrap();
    let mut printer = sim();
    let uploaded = printer.upload(&job).unwrap();
    let token = confirmed(&job, &printer);
    start(&mut printer, token, &job, &uploaded).unwrap();
    let again = confirmed(&job, &printer);
    let error = start(&mut printer, again, &job, &uploaded).unwrap_err();
    assert!(
        matches!(error, StartError::Busy(Phase::Preparing)),
        "{error}"
    );
    let starts = printer
        .wire_log()
        .iter()
        .filter(|line| line.contains("project_file"))
        .count();
    assert_eq!(starts, 1);
}

#[test]
fn only_the_one_authority_can_confirm_and_a_confirmation_has_one_purpose() {
    let _ = dialog();
    assert!(
        ConfirmationAuthority::take().is_none(),
        "the authority exists once per process"
    );

    let dir = tempfile::tempdir().unwrap();
    let job = check(&write_good(dir.path(), "box.gcode.3mf"), "box", &pla_job()).unwrap();
    // A confirmation of a hand-off cannot be turned into a start token.
    let hand_off = click(ConfirmationRequest::hand_off(&job));
    assert!(matches!(
        StartToken::mint(hand_off, StartToken::DEFAULT_TTL),
        Err(TokenError::NotForAPrint)
    ));
}

#[test]
fn a_start_needs_the_upload_to_this_printer() {
    let dir = tempfile::tempdir().unwrap();
    let job = check(&write_good(dir.path(), "box.gcode.3mf"), "box", &pla_job()).unwrap();
    let mut printer = sim();
    let mut config = SimConfig::x2d(&x2d());
    config.identity.serial = Serial::new("20P0SIM00000002").unwrap();
    let mut elsewhere = SimulatedPrinter::new(x2d(), config);
    let uploaded_elsewhere = elsewhere.upload(&job).unwrap();
    let token = confirmed(&job, &printer);
    let error = start(&mut printer, token, &job, &uploaded_elsewhere).unwrap_err();
    assert!(matches!(error, StartError::NotUploaded), "{error}");
    nothing_started(&mut printer);
}

#[test]
fn a_later_upload_under_the_same_label_does_not_take_the_confirmed_files_place() {
    let dir = tempfile::tempdir().unwrap();
    // Two slices of one version: the same label, other bytes (B heats the bed to 40 C,
    // A to 35 C; both within the Cool Plate's limit).
    let job_a = check(&write_good(dir.path(), "a.gcode.3mf"), "box-v2", &pla_job()).unwrap();
    let hotter = write_sliced(
        dir.path(),
        "b.gcode.3mf",
        &GOOD_GCODE.replace("S35", "S40").replace("S220", "S240"),
        &good_slice_info(),
    );
    let job_b = check(&hotter, "box-v2", &pla_job()).unwrap();
    assert_eq!(job_a.name(), job_b.name());
    assert_ne!(job_a.remote_name(), job_b.remote_name());

    let mut printer = sim();
    let token_a = confirmed(&job_a, &printer);
    let uploaded_a = printer.upload(&job_a).unwrap();
    let _uploaded_b = printer.upload(&job_b).unwrap();
    // Both files are on the storage, each under its own name.
    assert_eq!(
        printer.stored_sha256(uploaded_a.remote_name()),
        Some(job_a.sha256())
    );
    assert_eq!(
        printer.stored_sha256(&job_b.remote_name()),
        Some(job_b.sha256())
    );
    start(&mut printer, token_a, &job_a, &uploaded_a).unwrap();
    let sent: serde_json::Value = serde_json::from_str(printer.wire_log().last().unwrap()).unwrap();
    assert_eq!(
        sent["print"]["url"],
        format!("ftp:///box-v2-{}.gcode.3mf", &job_a.sha256()[..32])
    );
    // What runs is what was confirmed: A's bed temperature, not B's.
    printer.advance(Duration::from_secs(30));
    let status = printer.status().unwrap();
    assert_eq!(
        status.gcode_file.as_deref(),
        Some(job_a.remote_name().as_str())
    );
    assert_eq!(status.bed.target, Some(35.0));
}

/// An editable project is handed over on a confirmation of its own: its text names the
/// project's hash and says that the project is outside the check, the confirmation is bound
/// to that hash and to the test slice, and it is good for nothing else.
#[test]
fn a_project_confirmation_covers_the_project_hash_and_nothing_else() {
    use hotend_printer::{BambuConnectHandOff, Delivered, Delivery, HandOffFolder, HostKind};
    let dir = tempfile::tempdir().unwrap();
    let job = check(
        &write_good(dir.path(), "test.gcode.3mf"),
        "holder-v3",
        &pla_job(),
    )
    .unwrap();
    let project = "AB".repeat(32);
    let lower = project.to_ascii_lowercase();

    let to = "Saved in the folder /inbox; nothing is opened.";
    assert!(ConfirmationRequest::project(&job, "not a hash", to).is_none());
    assert!(ConfirmationRequest::project(&job, &"a".repeat(63), to).is_none());
    // The destination is one line that says something.
    assert!(ConfirmationRequest::project(&job, &project, "").is_none());
    assert!(ConfirmationRequest::project(&job, &project, "  ").is_none());
    assert!(ConfirmationRequest::project(&job, &project, "a\nb").is_none());
    // ... and plain: no other white space than the space, and nothing invisible that could
    // break the line or reorder what the person reads.
    for hidden in [
        '\t',
        '\r',
        '\u{0B}',
        '\u{0C}',
        '\u{85}',
        '\u{A0}',
        '\u{2028}',
        '\u{2029}',
        '\u{3000}',
        '\u{200B}',
        '\u{200E}',
        '\u{200F}',
        '\u{202A}',
        '\u{202E}',
        '\u{2066}',
        '\u{2069}',
        '\u{2060}',
        '\u{FEFF}',
        '\u{00AD}',
        '\u{061C}',
        '\u{E0041}',
    ] {
        let destination = format!("Saved in the folder /in{hidden}box; nothing is opened.");
        assert!(
            ConfirmationRequest::project(&job, &project, &destination).is_none(),
            "U+{:04X} was accepted",
            u32::from(hidden)
        );
        assert!(!hotend_printer::is_plain_line(&destination));
    }
    assert!(hotend_printer::is_plain_line(to));
    assert!(hotend_printer::is_plain_line(
        "Saved in the folder D:\\Models\\Gärten (2); nothing is opened."
    ));
    let request = ConfirmationRequest::project(&job, &project, to).unwrap();
    assert_eq!(request.project_sha256(), Some(lower.as_str()));
    assert!(request.printer().is_none());
    let text = request.review_text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        format!("Editable Bambu Studio project. SHA-256 of the project file: {lower}.")
    );
    assert_eq!(lines[1], format!("Where it goes: {to}"));
    assert_eq!(lines[2], hotend_printer::PROJECT_OUTSIDE_CHECK);
    assert_eq!(
        hotend_printer::PROJECT_OUTSIDE_CHECK,
        "A project opened this way is sliced again by Bambu Studio, so it is outside Hotend's check."
    );
    assert!(lines[3].contains("as a test"), "{text}");
    assert!(text.ends_with(&job.summary().review_text()), "{text}");
    // The text never says that the project was checked.
    assert!(
        !lines[..4].join(" ").contains("project was checked"),
        "{text}"
    );

    // The dialog must have shown this text, not the test slice's text alone.
    let request = ConfirmationRequest::project(&job, &project, to).unwrap();
    assert!(matches!(
        dialog().confirm(request, &job.summary().review_text()),
        Err(TokenError::NotWhatWasShown)
    ));
    // Nor the text of the same project going somewhere else.
    let elsewhere = ConfirmationRequest::project(&job, &project, "Saved in /other.").unwrap();
    assert!(matches!(
        dialog().confirm(
            ConfirmationRequest::project(&job, &project, to).unwrap(),
            &elsewhere.review_text()
        ),
        Err(TokenError::NotWhatWasShown)
    ));

    // Redeemed for this project, this destination and this test slice only.
    let for_project = || click(ConfirmationRequest::project(&job, &project, to).unwrap());
    assert!(matches!(
        for_project().redeem_for_project(&job, &"cd".repeat(32), to),
        Err(TokenError::WrongFile)
    ));
    assert!(matches!(
        for_project().redeem_for_project(&job, "short", to),
        Err(TokenError::WrongFile)
    ));
    assert!(matches!(
        for_project().redeem_for_project(&job, &project, "Saved in /other."),
        Err(TokenError::WrongJob)
    ));
    let other = check(
        &write_sliced(
            dir.path(),
            "other.gcode.3mf",
            &GOOD_GCODE.replace("S220", "S225"),
            &good_slice_info(),
        ),
        "holder-v3",
        &pla_job(),
    )
    .unwrap();
    assert!(matches!(
        for_project().redeem_for_project(&other, &project, to),
        Err(TokenError::WrongFile)
    ));
    for_project().redeem_for_project(&job, &lower, to).unwrap();

    // A project confirmation starts nothing and releases no Bambu Connect link.
    assert!(matches!(
        StartToken::mint(for_project(), StartToken::DEFAULT_TTL),
        Err(TokenError::NotForAPrint)
    ));
    let inbox = tempfile::tempdir().unwrap();
    let Delivered::HandOff(placed) =
        BambuConnectHandOff::new(HandOffFolder::new(inbox.path(), HostKind::Linux).unwrap())
            .deliver(&job)
            .unwrap()
    else {
        panic!("expected a hand-off");
    };
    assert!(placed.link(&job, for_project()).is_err());
    // Nor does any other confirmation hand over a project.
    let printer = sim();
    let start = click(ConfirmationRequest::start_print(&job, printer.identity()));
    assert!(matches!(
        start.redeem_for_project(&job, &project, to),
        Err(TokenError::NotForThis)
    ));
    let hand_off = click(ConfirmationRequest::hand_off(&job));
    assert!(matches!(
        hand_off.redeem_for_project(&job, &project, to),
        Err(TokenError::NotForThis)
    ));

    // A test slice whose file changed after the confirmation is refused.
    let confirmed = for_project();
    std::fs::write(job.path(), b"changed").unwrap();
    assert!(confirmed.redeem_for_project(&job, &project, to).is_err());
}
