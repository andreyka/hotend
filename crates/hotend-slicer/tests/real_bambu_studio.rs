//! Tests against a real Bambu Studio, when one is installed where the sandbox can run it
//! (Linux, macOS). They settle what the recorded output and the stand-in cannot: that the
//! program starts inside the sandbox, that the profiles of its installation are found and
//! flattened, that it accepts Hotend's command line (the plate type and the filament map
//! above all) and that Hotend reads what it writes.
//!
//! Bambu Studio is optional and Hotend never installs it, so these tests are not part of
//! `cargo test`: they are marked `ignore`. Where a Bambu Studio is installed, run them with
//!
//! ```text
//! cargo test -p hotend-slicer --test real_bambu_studio -- --ignored --nocapture --test-threads 1
//! ```
//!
//! Name the program with `HOTEND_BAMBU_STUDIO` when it is not on the `PATH`, and its
//! profile folder with `HOTEND_BAMBU_PROFILES` when it is not `resources/profiles` next to
//! or one folder above the program. Run like that, a test that finds no Bambu Studio fails:
//! a pass always means the real program ran. Each test prints what it saw; keep the output.
//!
//! They passed with Bambu Studio 02.09.01.70 on Linux. A failure is a finding about the real
//! program, to be read with the README. A slice that fails with `profile_unusable` names the
//! installed profile and the reason (not a preset of its role, or a G-code or script setting
//! where only the machine profile may have one): that is a finding about the installed
//! profiles.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{Read, Write};

use common::*;
use hotend_slicer::{
    Installation, SYSTEM_PROFILES_ENV, SliceJob, Slicer, SlicerConfig, SlicerKind,
};

/// A slicer configured from the environment, as the application's backend does it.
fn slicer() -> Slicer {
    let mut config = SlicerConfig::new().unwrap();
    config.system_profiles = std::env::var_os(SYSTEM_PROFILES_ENV)
        .filter(|value| !value.is_empty())
        .map(Into::into);
    Slicer::new(config).unwrap()
}

/// The real Bambu Studio on this machine. Without one the test fails; only with
/// `HOTEND_TEST_ALLOW_SKIP=1` it is skipped instead (`None` after printing why).
fn real_bambu_studio(test: &str, slicer: &Slicer) -> Option<Installation> {
    if !sandbox_ready(test) {
        return None;
    }
    let found = slicer
        .detect_kind(SlicerKind::BambuStudio)
        .unwrap()
        .into_iter()
        .find(|i| !i.fake);
    if found.is_some() {
        return found;
    }
    let problem = "no Bambu Studio was found (set HOTEND_BAMBU_STUDIO to the program)";
    if std::env::var_os("HOTEND_TEST_ALLOW_SKIP").is_some_and(|v| v == "1") {
        println!("SKIPPED {test}: {problem}");
        return None;
    }
    panic!("{test} cannot run: {problem} (set HOTEND_TEST_ALLOW_SKIP=1 to skip instead)");
}

/// The box of the fixtures (20 x 30 x 10 mm) moved to X 100 to 120 and Y 100 to 130, well
/// inside the bed: the same 3MF with every vertex shifted. The measured run kept a plain
/// 3MF where its vertices lie.
fn shifted_box_job(dir: &JobDir) -> SliceJob {
    const SHIFT: f64 = 100.0;
    let source = std::fs::File::open(fixture("box_20x30x10.3mf")).unwrap();
    let mut archive = zip::ZipArchive::new(source).unwrap();
    let target = std::fs::File::create(dir.path.join("input/placed.3mf")).unwrap();
    let mut writer = zip::ZipWriter::new(target);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().unwrap().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if name.ends_with(".model") {
            let text = String::from_utf8(bytes).unwrap();
            let mut moved = String::new();
            for (number, piece) in text.split("<vertex x=\"").enumerate() {
                if number == 0 {
                    moved.push_str(piece);
                    continue;
                }
                let (x, rest) = piece.split_once("\" y=\"").unwrap();
                let (y, rest) = rest.split_once('"').unwrap();
                let x: f64 = x.parse().unwrap();
                let y: f64 = y.parse().unwrap();
                moved.push_str(&format!(
                    "<vertex x=\"{}\" y=\"{}\"{rest}",
                    x + SHIFT,
                    y + SHIFT
                ));
            }
            bytes = moved.into_bytes();
        }
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    writer.finish().unwrap();

    let mut job = x2d_job(dir, "placed.3mf", vec![filament(1, "pla-basic", "PLA", 1)]);
    job.arrange = false;
    job.extents[0].min = [SHIFT, SHIFT, 0.0];
    job.extents[0].max = [SHIFT + 20.0, SHIFT + 30.0, 10.0];
    job.extents[0].placed = true;
    job
}

/// Prints everything a run left behind that a person needs to judge it.
fn show(dir: &JobDir) {
    for name in ["profiles/flatten-report.json", "output/result.json"] {
        match std::fs::read_to_string(dir.path.join(name)) {
            Ok(text) => println!("{name}: {text}"),
            Err(error) => println!("{name}: not there ({error})"),
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir.path.join("output")) {
        let names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
        println!("output folder: {names:?}");
    }
}

#[test]
#[ignore = "needs an installed Bambu Studio; run with `-- --ignored`"]
fn the_installed_bambu_studio_is_found_with_its_profiles() {
    let test = "the_installed_bambu_studio_is_found_with_its_profiles";
    let slicer = slicer();
    let Some(installation) = real_bambu_studio(test, &slicer) else {
        return;
    };
    // What `--help` gave inside the sandbox: a version, or a note saying why there is none.
    println!("found {installation:#?}");
    assert!(
        !installation.verdict.is_refused(),
        "the program cannot be used: {:?}",
        installation.verdict
    );
    let explicit = std::env::var_os(SYSTEM_PROFILES_ENV).map(std::path::PathBuf::from);
    let folder = hotend_slicer::system::profile_folder(&installation, explicit.as_deref())
        .expect("no `resources/profiles` next to the program; set HOTEND_BAMBU_PROFILES");
    println!("profile folder: {}", folder.display());
    for role in ["machine", "process", "filament"] {
        assert!(folder.join("BBL").join(role).is_dir(), "{role}");
    }
}

#[test]
#[ignore = "needs an installed Bambu Studio; run with `-- --ignored`"]
fn the_real_bambu_studio_slices_a_placed_box_for_the_measured_plate() {
    let test = "the_real_bambu_studio_slices_a_placed_box_for_the_measured_plate";
    let slicer = slicer();
    let Some(installation) = real_bambu_studio(test, &slicer) else {
        return;
    };
    let dir = JobDir::new(&[]);
    let mut job = shifted_box_job(&dir);
    // The plate of the measured run. The command line's own default, so this run differs
    // from the measured one only by Hotend's options.
    job.bed_type = Some("cool".into());
    let outcome = slicer.slice_with(&installation, &job);
    show(&dir);
    let result = outcome.unwrap();
    println!("{}", serde_json::to_string_pretty(&result).unwrap());

    // The file states the version even when the program printed none.
    assert_ne!(result.slicer.version, "unknown");
    assert_eq!(result.layers, Some(50), "10 mm at 0.2 mm layers");
    assert!(result.time_s.is_some_and(|seconds| seconds > 60.0));
    // A weight above zero: the filament settings were complete, so the flattening worked.
    assert!(result.filament[0].grams.is_some_and(|grams| grams > 0.5));
    assert_eq!(result.filament[0].nozzle_id, 1);
    let bed = result.bed.as_ref().unwrap();
    assert_eq!(bed.plate_type, "Cool Plate");
    assert_eq!(bed.temperature_c, Some(35.0), "measured for PLA Basic");
    // The box was printed where the file has it (the slice is refused otherwise), and the
    // extent was read from the moves of the G-code. The measured settings allow a brim of
    // up to 5 mm around a part (`brim_type = auto_brim`); whether this box gets one is
    // printed here.
    let extent = result.extent.as_ref().unwrap();
    println!("printed extent: {extent:?}");
    assert_eq!(extent.source, "gcode");
    assert!(extent.x[0] >= 94.0 && extent.x[1] <= 126.0, "{extent:?}");
    assert!(extent.y[0] >= 94.0 && extent.y[1] <= 136.0, "{extent:?}");
    // What Hotend set on top of the installation's profiles is named, and nothing had to
    // be left out of them.
    let overrides = result
        .warnings
        .iter()
        .find(|w| w.code == "profile_overrides")
        .expect("the overrides are reported");
    assert!(
        overrides.message.contains("curr_bed_type = Cool Plate"),
        "{}",
        overrides.message
    );
    for warning in &result.warnings {
        println!("warning {}: {}", warning.code, warning.message);
    }
    // The manual filament map was accepted and is what the file reports.
    let codes = codes(&result.warnings);
    assert!(!codes.contains(&"filament_map_unconfirmed"), "{codes:?}");
    assert!(!codes.contains(&"summary_disagrees"), "{codes:?}");
}

#[test]
#[ignore = "needs an installed Bambu Studio; run with `-- --ignored`"]
fn the_real_bambu_studio_slices_for_the_plate_hotend_asks_for() {
    let test = "the_real_bambu_studio_slices_for_the_plate_hotend_asks_for";
    let slicer = slicer();
    let Some(installation) = real_bambu_studio(test, &slicer) else {
        return;
    };
    // Every plate type of the table that has not been sliced with yet. A plate the program
    // does not accept under that name fails here with `bed_type_not_applied` (it sliced for
    // another plate) or `slicer_failed` (it refused the value).
    for (id, name) in [
        ("textured-pei", "Textured PEI Plate"),
        ("high-temp", "High Temp Plate"),
        ("engineering", "Engineering Plate"),
    ] {
        let dir = JobDir::new(&[]);
        let mut job = shifted_box_job(&dir);
        job.bed_type = Some(id.into());
        let outcome = slicer.slice_with(&installation, &job);
        if outcome.is_err() {
            show(&dir);
        }
        let result = outcome.unwrap_or_else(|error| panic!("{id}: {} ({error})", error.code()));
        let bed = result.bed.unwrap();
        println!(
            "{id}: sliced for `{}` at {:?} C",
            bed.plate_type, bed.temperature_c
        );
        assert_eq!(bed.plate_type, name);
        assert!(bed.temperature_c.is_some(), "{id}");
    }
}

#[test]
#[ignore = "needs an installed Bambu Studio; run with `-- --ignored`"]
fn the_real_bambu_studio_reports_a_model_it_cannot_slice() {
    let test = "the_real_bambu_studio_reports_a_model_it_cannot_slice";
    let slicer = slicer();
    let Some(installation) = real_bambu_studio(test, &slicer) else {
        return;
    };
    let dir = JobDir::new(&[]);
    // A 3MF archive with the parts of a model and nothing else, so that Hotend hands it
    // on, whose model is not a model.
    let broken = zip_of(&[
        (
            "[Content_Types].xml",
            b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><Types/>".as_slice(),
        ),
        ("3D/3dmodel.model", b"this is not a model".as_slice()),
    ]);
    std::fs::write(dir.path.join("input/broken.3mf"), broken).unwrap();
    let mut job = x2d_job(&dir, "broken.3mf", vec![filament(1, "pla-basic", "PLA", 1)]);
    job.bed_type = Some("cool".into());
    let error = slicer.slice_with(&installation, &job).unwrap_err();
    show(&dir);
    // Which of the two it is says how the real program fails: with a result file that
    // carries its return code and words (`slicer_failed`), or without one
    // (`result_missing`). Either way nothing is taken for a sliced file.
    println!("a broken model gives `{}`: {error}", error.code());
    assert!(
        ["slicer_failed", "result_missing"].contains(&error.code()),
        "{}",
        error.code()
    );
}

/// Copies a file a run made into the folder `HOTEND_KEEP_REAL_OUTPUT` names, when it is set,
/// so that a person can inspect it. Outputs stay outside the repository.
fn keep(file: &std::path::Path, name: &str) {
    if let Some(folder) = std::env::var_os("HOTEND_KEEP_REAL_OUTPUT").filter(|v| !v.is_empty()) {
        let target = std::path::Path::new(&folder).join(name);
        std::fs::copy(file, &target).unwrap();
        println!("kept {}", target.display());
    }
}

/// The placed box with the holder recipe: Generic PLA, the Textured PEI plate, the fitted
/// 0.4 mm nozzle and the print settings of a plan that gives level 1 with a 5 mm brim.
fn recipe_job(dir: &JobDir) -> SliceJob {
    let mut job = shifted_box_job(dir);
    job.filaments = vec![filament(1, "pla-generic", "PLA", 1)];
    job.bed_type = Some("textured-pei".into());
    job.print_settings = Some(recipe_settings());
    job.nozzle_diameter_mm = Some(0.4);
    job
}

#[test]
#[ignore = "needs an installed Bambu Studio; run with `-- --ignored`"]
fn the_real_bambu_studio_slices_with_the_holder_recipe_and_the_proof_passes() {
    let test = "the_real_bambu_studio_slices_with_the_holder_recipe_and_the_proof_passes";
    let slicer = slicer();
    let Some(installation) = real_bambu_studio(test, &slicer) else {
        return;
    };
    let dir = JobDir::new(&[]);
    let job = recipe_job(&dir);
    let outcome = slicer.slice_with(&installation, &job);
    show(&dir);
    let result = outcome.unwrap_or_else(|error| panic!("{}: {error}", error.code()));
    println!(
        "recipe slice: {:?} layers, {:?} g, {:?} s",
        result.layers, result.filament[0].grams, result.time_s
    );
    keep(&dir.path.join(&result.output), "recipe-slice.gcode.3mf");
    for warning in &result.warnings {
        println!("warning {}: {}", warning.code, warning.message);
    }
    let codes = codes(&result.warnings);
    // The proof ran inside the slice and passed; otherwise the slice is an error.
    assert!(codes.contains(&"print_settings_proven"), "{codes:?}");
    assert!(!codes.contains(&"print_settings_missing"), "{codes:?}");
    let bed = result.bed.as_ref().unwrap();
    assert_eq!(bed.plate_type, "Textured PEI Plate");
    assert_eq!(bed.temperature_c, Some(65.0));
    // What Hotend put on top is listed: the plate, the brim, the speeds, the bed and fans.
    let overrides = result
        .warnings
        .iter()
        .find(|w| w.code == "profile_overrides")
        .unwrap();
    for part in [
        "curr_bed_type = Textured PEI Plate",
        "brim_type = outer_only",
        "initial_layer_speed = 20",
        "textured_plate_temp_initial_layer = 65",
        "wall_loops = 3",
        "enable_prime_tower = 0",
    ] {
        assert!(
            overrides.message.contains(part),
            "{part}: {}",
            overrides.message
        );
    }

    // A plan with a value never measured with this slicer is refused before the program runs:
    // a very slender piece gets gentle motion, and its acceleration limit has never been
    // sliced with. (Until 18 November 2026 the calm plan was refused here for `no_brim`; that
    // value is measured now, see the next test.)
    let mut slender = recipe_job(&dir);
    slender.print_settings = Some(slender_settings());
    let error = slicer.slice_with(&installation, &slender).unwrap_err();
    println!("slender plan: {}: {error}", error.code());
    assert_eq!(error.code(), "setting_unmeasured");
}

/// The G-code text of plate 1 inside a sliced 3MF.
fn plate_gcode(file: &std::path::Path) -> String {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(file).unwrap()).unwrap();
    let mut entry = archive.by_name("Metadata/plate_1.gcode").unwrap();
    let mut text = String::new();
    entry.read_to_string(&mut text).unwrap();
    text
}

/// What a sliced file itself says about its brim: the value of `brim_type` in its settings
/// block, and how many brim features its first layer and the whole file have.
fn brim_in_file(gcode: &str) -> (Option<String>, usize, usize) {
    let echoed = gcode.lines().find_map(|line| {
        line.strip_prefix("; brim_type = ")
            .map(|value| value.trim().to_owned())
    });
    let layers: Vec<&str> = gcode.split("; CHANGE_LAYER").collect();
    let first_layer = layers.get(1).copied().unwrap_or("");
    let brim = "; FEATURE: Brim";
    (
        echoed,
        first_layer.matches(brim).count(),
        gcode.matches(brim).count(),
    )
}

#[test]
#[ignore = "needs an installed Bambu Studio; run with `-- --ignored`"]
fn the_real_bambu_studio_slices_without_a_brim_when_the_plan_has_none() {
    // The measurement behind `verified_texts = [..., "no_brim"]` of `brim_type` in the keymap
    // (18 November 2026, Linux build 02.09.01.70): the calm plan (level 0, no brim) on the
    // placed box with Generic PLA on the Textured PEI plate. It passes only when
    // the file's settings say `no_brim`, its first layer (and every other) has no brim
    // feature, and the proof of the plan passes.
    let test = "the_real_bambu_studio_slices_without_a_brim_when_the_plan_has_none";
    let slicer = slicer();
    let Some(installation) = real_bambu_studio(test, &slicer) else {
        return;
    };
    let dir = JobDir::new(&[]);
    let mut job = recipe_job(&dir);
    let settings = stock_settings();
    assert_eq!(
        settings.get(hotend_settings::model::Field::BrimKind),
        hotend_settings::model::Value::Token("none")
    );
    job.print_settings = Some(settings.clone());
    let outcome = slicer.slice_with(&installation, &job);
    show(&dir);
    let result = outcome.unwrap_or_else(|error| panic!("{}: {error}", error.code()));
    let output = dir.path.join(&result.output);
    keep(&output, "no-brim-slice.gcode.3mf");
    println!(
        "no-brim slice: {:?} layers, {:?} g, {:?} s",
        result.layers, result.filament[0].grams, result.time_s
    );
    let codes = codes(&result.warnings);
    assert!(codes.contains(&"print_settings_proven"), "{codes:?}");
    let overrides = result
        .warnings
        .iter()
        .find(|w| w.code == "profile_overrides")
        .unwrap();
    println!("overrides: {}", overrides.message);
    assert!(
        overrides.message.contains("brim_type = no_brim"),
        "{}",
        overrides.message
    );
    // The slicer's own output, read here without Hotend's proof.
    let (echoed, first_layer, whole) = brim_in_file(&plate_gcode(&output));
    println!(
        "file: brim_type = {echoed:?}, brim features on layer 1: {first_layer}, in the file: {whole}"
    );
    assert_eq!(echoed.as_deref(), Some("no_brim"));
    assert_eq!(first_layer, 0);
    assert_eq!(whole, 0);
    // And Hotend's proof of the plan on the same file.
    let expected = hotend_slicer::proof::Expected::for_settings(&settings, None).unwrap();
    let report = hotend_slicer::proof::check_sliced_file(&output, 1, &expected).unwrap();
    assert!(report.passed(), "{}", report.sentence());
    let brim = report.facts.iter().find(|fact| fact.id == "brim").unwrap();
    assert_eq!(brim.measured, Some(0.0));

    // The same box with the holder recipe: the file says `outer_only` and its first layer
    // has the brim, so the reading above is not blind to a brim.
    let dir = JobDir::new(&[]);
    let job = recipe_job(&dir);
    let result = slicer.slice_with(&installation, &job).unwrap();
    let (echoed, first_layer, _) = brim_in_file(&plate_gcode(&dir.path.join(&result.output)));
    println!("recipe file: brim_type = {echoed:?}, brim features on layer 1: {first_layer}");
    assert_eq!(echoed.as_deref(), Some("outer_only"));
    assert!(first_layer > 0);
}

#[test]
#[ignore = "needs an installed Bambu Studio; run with `-- --ignored`"]
fn the_editable_project_of_a_small_plate_slices_alone_and_passes_the_proof() {
    let test = "the_editable_project_of_a_small_plate_slices_alone_and_passes_the_proof";
    let slicer = slicer();
    let Some(installation) = real_bambu_studio(test, &slicer) else {
        return;
    };
    let dir = JobDir::new(&[]);
    let job = recipe_job(&dir);
    let outcome = slicer.export_project_with(&installation, &job);
    show(&dir);
    for name in ["proof/output/result.json"] {
        if let Ok(text) = std::fs::read_to_string(dir.path.join(name)) {
            println!("{name}: {text}");
        }
    }
    let export = outcome.unwrap_or_else(|error| panic!("{}: {error}", error.code()));
    keep(&export.project, "recipe-project.3mf");
    keep(
        &dir.path.join("proof").join(&export.result.output),
        "recipe-project-resliced.gcode.3mf",
    );
    println!(
        "project: {} ({}), run C: {:?} layers, {:?} g, {:?} s",
        export.project.display(),
        export.sha256,
        export.result.layers,
        export.result.filament[0].grams,
        export.result.time_s
    );
    println!("changed keys: {:?}", export.keys.texts());
    assert!(export.proof.passed());
    // The lists the window writes for the holder slice, from the keys Hotend changed.
    let [process, filament, printer] = export.keys.texts();
    assert_eq!(
        process,
        "brim_object_gap;brim_type;curr_bed_type;enable_prime_tower;\
         initial_layer_infill_speed;initial_layer_speed;wall_loops"
    );
    assert_eq!(
        filament,
        "additional_cooling_fan_speed;close_fan_the_first_x_layers;first_x_layer_fan_speed;\
         first_x_layer_optional_fan_speed;optional_cooling_fan_speed;textured_plate_temp;\
         textured_plate_temp_initial_layer"
    );
    assert_eq!(printer, "");
    // The project on disk is the one that was hashed and proven.
    let bytes = std::fs::read(&export.project).unwrap();
    use sha2::Digest;
    let hex: String = sha2::Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(hex, export.sha256);
}

/// The feed rates (mm/s) of the moves that lay a line (X or Y with E) of `feature` on the
/// first layer of a G-code.
fn first_layer_feeds(gcode: &str, feature: &str) -> Vec<f64> {
    let first = gcode.split("; CHANGE_LAYER").nth(1).unwrap_or("");
    let (mut current, mut feed, mut feeds) = (None::<&str>, None::<f64>, Vec::new());
    for line in first.lines() {
        if let Some(name) = line.strip_prefix("; FEATURE: ") {
            current = Some(name.trim());
            continue;
        }
        if !(line.starts_with("G1 ") || line.starts_with("G2 ") || line.starts_with("G3 ")) {
            continue;
        }
        let words: Vec<&str> = line
            .split(';')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        if let Some(f) = words.iter().find_map(|word| word.strip_prefix('F')) {
            feed = f.parse::<f64>().ok().map(|f| f / 60.0);
        }
        let extrudes = words.iter().any(|word| {
            word.strip_prefix('E')
                .and_then(|e| e.parse::<f64>().ok())
                .is_some_and(|e| e > 0.0)
        });
        // A move that only primes the nozzle (E without X or Y) lays no line.
        let moves = words
            .iter()
            .any(|word| word.starts_with('X') || word.starts_with('Y'));
        if extrudes && moves && current == Some(feature) {
            feeds.push(feed.unwrap_or(f64::NAN));
        }
    }
    feeds
}

/// The bundled example's plate (the cable box of `hotend demo`, laid out by Hotend's
/// engine), where it lies on the bed.
fn cable_box_job(dir: &JobDir, settings: hotend_settings::PrintSettings) -> SliceJob {
    std::fs::copy(
        fixture("cable_box_plate.3mf"),
        dir.path.join("input/cable_box_plate.3mf"),
    )
    .unwrap();
    let mut job = x2d_job(
        dir,
        "cable_box_plate.3mf",
        vec![filament(1, "pla-generic", "PLA", 1)],
    );
    job.arrange = false;
    job.extents = Vec::new();
    job.bed_type = Some("textured-pei".into());
    job.print_settings = Some(settings);
    job.nozzle_diameter_mm = Some(0.4);
    job
}

#[test]
#[ignore = "needs an installed Bambu Studio; run with `-- --ignored`"]
fn the_real_bambu_studio_fills_first_layer_gaps_at_the_wall_speed() {
    // The measurement behind the `Gap infill` feature of `profiles/bambu-gcode-facts.toml`
    // (18 November 2026, Linux build 02.09.01.70): the bundled example's first layer has gap
    // fill. With two plans whose wall and infill speeds differ, every gap fill move of the
    // first layer runs at the wall speed, never above it, and the proof passes.
    let test = "the_real_bambu_studio_fills_first_layer_gaps_at_the_wall_speed";
    let slicer = slicer();
    let Some(installation) = real_bambu_studio(test, &slicer) else {
        return;
    };
    for (name, settings, wall) in [
        ("calm", calm_thin_walled_settings(), 30.0),
        ("recipe", recipe_settings(), 20.0),
    ] {
        let dir = JobDir::new(&[]);
        let job = cable_box_job(&dir, settings.clone());
        let outcome = slicer.slice_with(&installation, &job);
        if outcome.is_err() {
            show(&dir);
        }
        let result = outcome.unwrap_or_else(|error| panic!("{name}: {}: {error}", error.code()));
        let output = dir.path.join(&result.output);
        keep(&output, &format!("cable-box-{name}.gcode.3mf"));
        let gcode = plate_gcode(&output);
        let gaps = first_layer_feeds(&gcode, "Gap infill");
        let walls = first_layer_feeds(&gcode, "Outer wall");
        let fill = first_layer_feeds(&gcode, "Bottom surface");
        let top = |feeds: &[f64]| feeds.iter().copied().fold(0.0_f64, f64::max);
        println!(
            "{name}: {:?} layers, {:?} g, {:?} s; layer 1 gap fill: {} moves up to {} mm/s, outer wall up to {} mm/s, bottom up to {} mm/s",
            result.layers,
            result.filament[0].grams,
            result.time_s,
            gaps.len(),
            top(&gaps),
            top(&walls),
            top(&fill)
        );
        assert!(!gaps.is_empty(), "{name}: the first layer has no gap fill");
        assert!(
            gaps.iter().all(|feed| (*feed - wall).abs() < 1e-6),
            "{name}: {gaps:?}"
        );
        assert!(
            top(&fill) > wall,
            "{name}: the infill speed must differ from the wall speed"
        );
        let codes = codes(&result.warnings);
        assert!(
            codes.contains(&"print_settings_proven"),
            "{name}: {codes:?}"
        );
    }
}
