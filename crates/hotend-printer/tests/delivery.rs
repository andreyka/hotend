//! The three delivery paths behind one interface.
#![allow(clippy::unwrap_used)]

mod common;

use common::{check, click, pla_job, sim, write_good};
use hotend_printer::delivery::{DeliveryError, LINK_PREFIX, bambu_connect_link, link_name};
use hotend_printer::{
    BambuConnectHandOff, Capabilities, ConfirmationRequest, Delivered, Delivery, DeliveryKind,
    ExportToFolder, HandOffFolder, HandOffPathError, HostKind, JobError, LinkOpener,
    PrinterDelivery, StandInOpener, TokenError,
};

/// A checked file without the plate preview (the headless Linux slice of 2026-11-21) is
/// not placed for Bambu Connect, which would accept it and import nothing; it can still
/// be exported to a folder.
#[test]
fn a_file_without_the_plate_preview_is_not_handed_to_bambu_connect() {
    let dir = tempfile::tempdir().unwrap();
    let entries: Vec<(String, Vec<u8>)> =
        common::sliced_entries(common::GOOD_GCODE, &common::good_slice_info())
            .into_iter()
            .filter(|(name, _)| name != "Metadata/plate_1.png")
            .collect();
    let path = dir.path().join("headless.gcode.3mf");
    std::fs::write(&path, common::zip_bytes(&common::borrowed(&entries))).unwrap();
    let job = check(&path, "box", &pla_job()).unwrap();
    assert_eq!(
        hotend_printer::bambu_connect_missing(&job),
        ["Metadata/plate_1.png"]
    );
    let inbox = tempfile::tempdir().unwrap();
    let refused = hand_off_to(inbox.path()).deliver(&job).unwrap_err();
    assert!(
        matches!(&refused, DeliveryError::NotImportable(missing) if missing == &["Metadata/plate_1.png"]),
        "{refused}"
    );
    assert!(
        refused
            .to_string()
            .contains("Bambu Connect cannot import this sliced file"),
        "{refused}"
    );
    assert_eq!(std::fs::read_dir(inbox.path()).unwrap().count(), 0);
    let out = tempfile::tempdir().unwrap();
    assert!(ExportToFolder::new(out.path()).deliver(&job).is_ok());
    // The usual fixture has the preview: nothing is missing.
    let good = check(&write_good(dir.path(), "good.gcode.3mf"), "box", &pla_job()).unwrap();
    assert!(hotend_printer::bambu_connect_missing(&good).is_empty());
}

/// A hand-off through `folder` with the local path (Linux).
fn hand_off_to(folder: &std::path::Path) -> BambuConnectHandOff {
    BambuConnectHandOff::new(HandOffFolder::new(folder, HostKind::Linux).unwrap())
}

#[test]
fn every_path_states_what_it_can_and_cannot_do() {
    let mut printer = sim();
    let folder = tempfile::tempdir().unwrap();
    let paths: Vec<Box<dyn Delivery + '_>> = vec![
        Box::new(PrinterDelivery::new(&mut printer)),
        Box::new(hand_off_to(folder.path())),
        Box::new(ExportToFolder::new(folder.path())),
    ];
    let mut capabilities: Vec<Capabilities> =
        paths.iter().map(|path| path.capabilities()).collect();
    assert_eq!(
        capabilities.iter().map(|c| c.kind).collect::<Vec<_>>(),
        [
            DeliveryKind::Simulated,
            DeliveryKind::BambuConnect,
            DeliveryKind::Export
        ]
    );
    for other in &capabilities[1..] {
        assert!(!other.sends_file && !other.starts_print && !other.controls_print);
        assert!(!other.live_status && !other.reports_result);
        assert!(!other.needs.is_empty());
    }
    // The real printer's path, which no printer of a default build has.
    let lan = Capabilities::lan();
    assert_eq!(lan.kind, DeliveryKind::Lan);
    assert!(lan.sends_file && lan.starts_print && lan.controls_print && lan.live_status);
    assert!(
        lan.cost.contains("Bambu Cloud"),
        "the trade-off is stated: {}",
        lan.cost
    );
    assert!(lan.needs.iter().any(|need| need.contains("Developer Mode")));
    capabilities.push(lan);
    for capability in &capabilities {
        assert!(!capability.can.is_empty() && !capability.cannot.is_empty());
        assert!(!capability.cost.is_empty());
        // The interface gets it as plain data.
        let json = serde_json::to_value(capability).unwrap();
        assert!(json["kind"].is_string() && json["can"].is_array());
    }
}

/// The words of a capability set, lower-cased, for looking for a claim in any of them.
fn words(capabilities: &Capabilities) -> String {
    let mut all = vec![capabilities.name, capabilities.cost];
    all.extend(&capabilities.can);
    all.extend(&capabilities.cannot);
    all.extend(&capabilities.needs);
    all.join("\n").to_lowercase()
}

#[test]
fn the_simulated_printer_describes_a_simulation_not_a_real_printer() {
    let mut printer = sim();
    let simulated = PrinterDelivery::new(&mut printer).capabilities();
    assert_eq!(simulated, Capabilities::simulated());
    assert_eq!(simulated.kind, DeliveryKind::Simulated);
    assert_eq!(simulated.name, "Simulated printer");
    assert_eq!(
        serde_json::to_value(&simulated).unwrap()["kind"],
        "simulated"
    );

    // It says that it is simulated and that nothing is made.
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
    assert!(simulated.cost.starts_with("Nothing is printed"));
    assert!(simulated.needs.is_empty(), "a simulation needs no hardware");

    // None of the real printer's sentences, and none of its conditions: no cloud, no USB
    // drive, no Developer Mode, no access code, and no pause that the simulation is not
    // offered with.
    let text = words(&simulated);
    let lan = Capabilities::lan();
    for line in lan.can.iter().chain(&lan.cannot).chain(&lan.needs) {
        assert!(
            !text.contains(&line.to_lowercase()),
            "a real printer's sentence on the simulated one: {line}"
        );
    }
    // Each claim is looked for at the start of a word ("pause" finds "Pauses").
    let spaced: String = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .fold(String::from(" "), |all, word| all + word + " ");
    for claim in [
        "bambu cloud",
        "usb",
        "developer mode",
        "access code",
        "lan ",
        "pause",
        "resume",
        "upload",
    ] {
        assert!(
            !spaced.contains(&format!(" {claim}")),
            "the simulated printer speaks of `{claim}`:\n{text}"
        );
    }

    // List items are written alike: a capital letter first, no full stop last.
    for line in simulated.can.iter().chain(&simulated.cannot) {
        assert!(line.starts_with(char::is_uppercase), "{line}");
        assert!(!line.ends_with('.'), "{line}");
    }
}

#[test]
fn delivery_to_a_printer_uploads_the_checked_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let job = check(
        &write_good(dir.path(), "box.gcode.3mf"),
        "My box",
        &pla_job(),
    )
    .unwrap();
    let mut printer = sim();
    let delivered = PrinterDelivery::new(&mut printer).deliver(&job).unwrap();
    let Delivered::OnPrinter(uploaded) = delivered else {
        panic!("expected an upload");
    };
    // The name on the printer is the label and the first 32 hex digits of the hash.
    let name = format!("My_box-{}.gcode.3mf", &job.sha256()[..32]);
    assert_eq!(uploaded.remote_name(), name);
    assert_eq!(uploaded.sha256(), job.sha256());
    assert_eq!(uploaded.size_bytes(), job.summary().size_bytes);
    assert_eq!(printer.stored_sha256(&name), Some(job.sha256()));
}

#[test]
fn export_writes_an_identical_file_into_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_good(dir.path(), "box.gcode.3mf");
    let job = check(&source, "My box", &pla_job()).unwrap();
    let usb = tempfile::tempdir().unwrap();
    let mut export = ExportToFolder::new(usb.path());
    let Delivered::Exported { path } = export.deliver(&job).unwrap() else {
        panic!("expected an export");
    };
    let name = format!("My_box-{}.gcode.3mf", &job.sha256()[..32]);
    assert_eq!(path, usb.path().canonicalize().unwrap().join(&name));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        std::fs::read(&source).unwrap()
    );
    // Exporting again replaces the file and leaves nothing else behind.
    export.deliver(&job).unwrap();
    let names: Vec<_> = std::fs::read_dir(usb.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, [name]);

    // A folder that is missing, or is a file.
    let missing = usb.path().join("missing");
    assert!(matches!(
        ExportToFolder::new(&missing).deliver(&job),
        Err(DeliveryError::Folder(..))
    ));
    assert!(matches!(
        ExportToFolder::new(&path).deliver(&job),
        Err(DeliveryError::Folder(..))
    ));
}

#[test]
fn a_file_changed_after_the_check_is_not_delivered_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_good(dir.path(), "box.gcode.3mf");
    let job = check(&source, "box", &pla_job()).unwrap();
    let mut bytes = std::fs::read(&source).unwrap();
    bytes.extend_from_slice(b"tampered");
    std::fs::write(&source, bytes).unwrap();

    let out = tempfile::tempdir().unwrap();
    assert!(matches!(
        ExportToFolder::new(out.path()).deliver(&job),
        Err(DeliveryError::Job(JobError::Changed))
    ));
    assert!(matches!(
        hand_off_to(out.path()).deliver(&job),
        Err(DeliveryError::Job(JobError::Changed))
    ));
    let mut printer = sim();
    assert!(PrinterDelivery::new(&mut printer).deliver(&job).is_err());
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
    assert!(printer.stored_files().is_empty());
}

#[test]
fn the_hand_off_places_the_file_and_releases_the_link_only_after_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let job = check(
        &write_good(dir.path(), "box.gcode.3mf"),
        "Vase & lid 50%",
        &pla_job(),
    )
    .unwrap();
    let inbox = tempfile::tempdir().unwrap();
    let Delivered::HandOff(hand_off) = hand_off_to(inbox.path()).deliver(&job).unwrap() else {
        panic!("expected a hand-off");
    };
    assert!(hand_off.path().is_absolute() && hand_off.path().is_file());
    assert_eq!(
        hand_off.path().file_name().unwrap().to_string_lossy(),
        format!("Vase___lid_50_-{}.gcode.3mf", &job.sha256()[..32])
    );

    // A confirmation for starting a print on a printer is not one for a hand-off.
    let printer = sim();
    let wrong = click(ConfirmationRequest::start_print(&job, {
        use hotend_printer::Printer;
        printer.identity()
    }));
    assert!(matches!(
        hand_off.link(&job, wrong),
        Err(DeliveryError::Confirmation(TokenError::NotForThis))
    ));
    // Nor is a confirmation of another file.
    let other = check(
        &common::write_sliced(
            dir.path(),
            "other.gcode.3mf",
            &common::GOOD_GCODE.replace("S220", "S225"),
            &common::good_slice_info(),
        ),
        "other",
        &pla_job(),
    )
    .unwrap();
    let for_other = click(ConfirmationRequest::hand_off(&other));
    assert!(matches!(
        hand_off.link(&job, for_other),
        Err(DeliveryError::Confirmation(TokenError::WrongFile))
    ));

    let request = ConfirmationRequest::hand_off(&job);
    assert_eq!(request.summary(), job.summary());
    assert!(request.printer().is_none());
    let link = hand_off.link(&job, click(request)).unwrap();
    let path = hand_off.path().to_string_lossy().replace('/', "%2F");
    assert_eq!(
        link.as_str(),
        format!("bambu-connect://import-file?path={path}&name=Vase%20_%20lid%2050&version=1.0.0")
    );
    assert_eq!(link.path(), hand_off.path().to_str().unwrap());
    assert_eq!(hand_off.windows_path(), None);
}

#[test]
fn the_link_encodes_like_encode_uri_component() {
    let link = bambu_connect_link(
        "/home/ann marie/Über & co/a+b=c?.gcode.3mf",
        "50% off #1 (final)",
    );
    assert_eq!(
        link,
        "bambu-connect://import-file?path=%2Fhome%2Fann%20marie%2F%C3%9Cber%20%26%20co%2Fa%2Bb%3Dc%3F.gcode.3mf&name=50%25%20off%20%231%20(final)&version=1.0.0"
    );
    // The Windows form: backslashes and the drive's colon are encoded.
    assert_eq!(
        bambu_connect_link(r"D:\Models\a b.gcode.3mf", "a"),
        "bambu-connect://import-file?path=D%3A%5CModels%5Ca%20b.gcode.3mf&name=a&version=1.0.0"
    );
    // Nothing in a path or name can add a parameter or end the link.
    for hostile in ["a&version=9", "a#b", "a b", "a\nb", "a%00", "a?x=1"] {
        let link = bambu_connect_link(hostile, hostile);
        let query = link.strip_prefix("bambu-connect://import-file?").unwrap();
        assert_eq!(query.matches('&').count(), 2, "{link}");
        assert_eq!(query.matches('=').count(), 3, "{link}");
        assert!(!query.contains(['#', ' ', '\n', '?']), "{link}");
    }
}

/// The link names a path, so it is released only while that path holds the confirmed
/// bytes; and a second job with the same label is placed beside the first, not over it.
#[test]
fn the_hand_off_link_is_for_the_file_that_was_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = tempfile::tempdir().unwrap();
    let a = write_good(dir.path(), "a.gcode.3mf");
    let b = common::write_sliced(
        dir.path(),
        "b.gcode.3mf",
        &common::GOOD_GCODE.replace("S220", "S238"),
        &common::good_slice_info(),
    );
    let job_a = check(&a, "bracket-v3", &pla_job()).unwrap();
    let job_b = check(&b, "bracket-v3", &pla_job()).unwrap();
    let place = |job| match hand_off_to(inbox.path()).deliver(job).unwrap() {
        Delivered::HandOff(hand_off) => hand_off,
        other => panic!("{other:?}"),
    };
    let hand_a = place(&job_a);
    let hand_b = place(&job_b);
    assert_ne!(hand_a.path(), hand_b.path());
    assert_eq!(
        std::fs::read(hand_a.path()).unwrap(),
        std::fs::read(&a).unwrap(),
        "placing B left A's file alone"
    );
    // A's confirmation releases A's link, and not B's.
    assert!(matches!(
        hand_b.link(&job_a, click(ConfirmationRequest::hand_off(&job_a))),
        Err(DeliveryError::Confirmation(TokenError::WrongFile))
    ));
    assert!(matches!(
        hand_a.link(&job_b, click(ConfirmationRequest::hand_off(&job_a))),
        Err(DeliveryError::Confirmation(TokenError::WrongFile))
    ));
    // Somebody writes other bytes over the placed file: no link.
    std::fs::copy(&b, hand_a.path()).unwrap();
    assert!(matches!(
        hand_a.link(&job_a, click(ConfirmationRequest::hand_off(&job_a))),
        Err(DeliveryError::Job(JobError::Changed))
    ));
    // Or replaces it by a link to another file.
    std::fs::remove_file(hand_a.path()).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&a, hand_a.path()).unwrap();
        assert!(matches!(
            hand_a.link(&job_a, click(ConfirmationRequest::hand_off(&job_a))),
            Err(DeliveryError::Job(JobError::Changed))
        ));
        std::fs::remove_file(hand_a.path()).unwrap();
    }
    // Placed again, the link is released, and the path it names holds the confirmed bytes.
    let hand_a = place(&job_a);
    let link = hand_a
        .link(&job_a, click(ConfirmationRequest::hand_off(&job_a)))
        .unwrap();
    assert!(link.as_str().contains(&job_a.sha256()[..32]), "{link}");
    assert_eq!(
        std::fs::read(hand_a.path()).unwrap(),
        std::fs::read(&a).unwrap()
    );
}

#[test]
fn the_name_in_a_link_is_a_short_plain_label() {
    assert_eq!(link_name("Planter holder v2"), "Planter holder v2");
    assert_eq!(link_name("a&version=9#x"), "a_version_9_x");
    assert_eq!(link_name("../../etc"), "etc");
    assert_eq!(link_name("\u{202E}<script>"), "script");
    assert_eq!(link_name("&&&"), "Hotend print");
    assert_eq!(link_name(&"x".repeat(200)).len(), 64);
}

/// A drive folder as WSL shows it, `<root>/mnt/d/Models/Hotend`, with the test's own
/// `<root>/mnt` standing in for `/mnt`: the link names the file by its Windows path, and
/// the stand-in opener records that link and opens nothing.
#[test]
fn under_wsl_the_link_names_the_windows_path_of_the_placed_file() {
    let dir = tempfile::tempdir().unwrap();
    let job = check(
        &write_good(dir.path(), "box.gcode.3mf"),
        "planter holder",
        &pla_job(),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let drives = root.path().canonicalize().unwrap().join("mnt");
    let folder = drives.join("d/Models/Hotend");
    std::fs::create_dir_all(&folder).unwrap();
    let chosen = HandOffFolder::with_drives_root(&folder, HostKind::Wsl, &drives).unwrap();
    assert_eq!(chosen.windows_path(), Some(r"D:\Models\Hotend"));
    let Delivered::HandOff(hand_off) = BambuConnectHandOff::new(chosen).deliver(&job).unwrap()
    else {
        panic!("expected a hand-off");
    };
    let name = format!("planter_holder-{}.gcode.3mf", &job.sha256()[..32]);
    assert_eq!(hand_off.path(), folder.join(&name));
    let windows = format!(r"D:\Models\Hotend\{name}");
    assert_eq!(hand_off.windows_path(), Some(windows.as_str()));

    let link = hand_off
        .link(&job, click(ConfirmationRequest::hand_off(&job)))
        .unwrap();
    assert_eq!(
        link.as_str(),
        format!(
            "bambu-connect://import-file?path=D%3A%5CModels%5CHotend%5C{name}&name=planter%20holder&version=1.0.0"
        )
    );
    assert_eq!(link.path(), windows);
    assert!(link.as_str().starts_with(LINK_PREFIX));

    let record = dir.path().join("opened.txt");
    let opener = StandInOpener::recording_to(&record);
    opener.open(&link).unwrap();
    assert_eq!(opener.asked(), [link.as_str().to_owned()]);
    assert_eq!(
        std::fs::read_to_string(&record).unwrap(),
        format!("{}\n", link.as_str())
    );
    assert!(opener.is_stand_in());
    println!("stand-in recorded: {}", link.as_str());
}

/// Under WSL a folder that is not on a Windows drive is refused before anything is placed,
/// and a link inside a drive folder that leads off the drive is too.
#[cfg(unix)]
#[test]
fn under_wsl_nothing_is_placed_outside_a_windows_drive() {
    let root = tempfile::tempdir().unwrap();
    let drives = root.path().canonicalize().unwrap().join("mnt");
    std::fs::create_dir_all(drives.join("d")).unwrap();
    let elsewhere = root.path().join("home");
    std::fs::create_dir_all(&elsewhere).unwrap();
    assert!(matches!(
        HandOffFolder::with_drives_root(&elsewhere, HostKind::Wsl, &drives),
        Err(HandOffPathError::NotOnWindowsDrive(_))
    ));
    // The drive itself is not a folder for the hand-off.
    assert!(matches!(
        HandOffFolder::with_drives_root(&drives.join("d"), HostKind::Wsl, &drives),
        Err(HandOffPathError::NotOnWindowsDrive(_))
    ));
    let sneaky = drives.join("d/Models");
    std::os::unix::fs::symlink(&elsewhere, &sneaky).unwrap();
    assert!(matches!(
        HandOffFolder::with_drives_root(&sneaky, HostKind::Wsl, &drives),
        Err(HandOffPathError::NotOnWindowsDrive(_))
    ));
    // A folder name Windows forbids.
    let bad = drives.join("d/CON");
    std::fs::create_dir_all(&bad).unwrap();
    assert!(matches!(
        HandOffFolder::with_drives_root(&bad, HostKind::Wsl, &drives),
        Err(HandOffPathError::BadName { .. })
    ));
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

/// The link names the placed file only while it is the same file inside the folder: one
/// replaced by a link to a copy elsewhere is refused even when the bytes are the same.
#[cfg(unix)]
#[test]
fn a_placed_file_swapped_for_a_link_releases_no_link() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_good(dir.path(), "box.gcode.3mf");
    let job = check(&source, "box", &pla_job()).unwrap();
    let inbox = tempfile::tempdir().unwrap();
    let Delivered::HandOff(hand_off) = hand_off_to(inbox.path()).deliver(&job).unwrap() else {
        panic!("expected a hand-off");
    };
    let copy = dir.path().join("copy.gcode.3mf");
    std::fs::copy(hand_off.path(), &copy).unwrap();
    std::fs::remove_file(hand_off.path()).unwrap();
    std::os::unix::fs::symlink(&copy, hand_off.path()).unwrap();
    let refused = hand_off
        .link(&job, click(ConfirmationRequest::hand_off(&job)))
        .unwrap_err();
    assert!(
        matches!(
            refused,
            DeliveryError::Job(JobError::Changed) | DeliveryError::Path(_)
        ),
        "{refused:?}"
    );
}

/// A file the check refuses never becomes a job, so there is nothing to place, no link and
/// nothing for an opener: the opener's only input is a link a confirmed hand-off made.
#[test]
fn a_refused_file_never_reaches_an_opener() {
    let dir = tempfile::tempdir().unwrap();
    let bad = common::write_sliced(
        dir.path(),
        "bad.gcode.3mf",
        &common::GOOD_GCODE.replace("S220", "S999"),
        &common::good_slice_info(),
    );
    let opener = StandInOpener::new();
    assert!(check(&bad, "bad", &pla_job()).is_err());
    assert!(opener.asked().is_empty());
}
