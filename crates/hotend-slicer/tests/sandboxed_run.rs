//! How a slicer process is run and what happens when it misbehaves. Shell scripts stand in
//! for the slicer so each failure can be produced on purpose.

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use hotend_slicer::{Slicer, SlicerConfig, SlicerKind, Verdict};

fn slicer(wall_seconds: u64) -> Slicer {
    let mut config = SlicerConfig::new().unwrap();
    config.limits.wall_seconds = wall_seconds;
    Slicer::new(config).unwrap()
}

#[test]
fn the_slicer_is_confined_to_the_job_folder_with_its_home_inside() {
    if !sandbox_ready("the_slicer_is_confined_to_the_job_folder_with_its_home_inside") {
        return;
    }
    let dir = JobDir::new(&["box_20x30x10.stl"]);
    let secret = dir.outside().join("secret.txt");
    std::fs::write(&secret, "printer access code").unwrap();
    let escape = dir.outside().join("escaped.txt");
    let body = format!(
        r#"{{
  echo "home=$HOME"
  echo "cwd=$(pwd)"
  echo "api=${{ANTHROPIC_API_KEY:-unset}}"
  if cat "{secret}" >/dev/null 2>&1; then echo "read=yes"; else echo "read=no"; fi
  if echo x > "{escape}" 2>/dev/null; then echo "write=yes"; else echo "write=no"; fi
  if echo x > "$HOME/inside.txt" 2>/dev/null; then echo "home_write=yes"; else echo "home_write=no"; fi
}} > report.txt
echo "args=$*" >> report.txt
exit 3"#,
        secret = secret.display(),
        escape = escape.display()
    );
    let script = Script::new(SlicerKind::PrusaSlicer, &body);
    // SAFETY: no other thread of this test binary reads this variable; it only shows that
    // the parent's secrets do not reach the slicer.
    unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-should-never-be-seen") };
    let error = slicer(60)
        .slice_with(&script.installation, &prusa_job(&dir, "box_20x30x10.stl"))
        .unwrap_err();
    assert_eq!(error.code(), "slicer_failed");
    assert!(error.to_string().contains("exit code 3"), "{error}");

    let report = std::fs::read_to_string(dir.path.join("report.txt")).unwrap();
    let job = dir.path.display().to_string();
    assert!(
        report.contains(&format!("home={job}/.sandbox/home\n")),
        "{report}"
    );
    assert!(report.contains(&format!("cwd={job}\n")), "{report}");
    assert!(report.contains("api=unset\n"), "{report}");
    assert!(report.contains("read=no\n"), "{report}");
    assert!(report.contains("write=no\n"), "{report}");
    assert!(report.contains("home_write=yes\n"), "{report}");
    assert!(!escape.exists());
    // The command line the slicer received is the documented PrusaSlicer one.
    assert!(
        report.contains(&format!(
            "args=--export-gcode --datadir {job}/slicer-config --loglevel 2 --load profiles/machine.ini \
             --load profiles/process.ini --load profiles/filament_1.ini --output {job}/output/plate_1.gcode \
             input/box_20x30x10.stl\n"
        )),
        "{report}"
    );
}

#[test]
fn a_slicer_that_hangs_is_killed_at_the_wall_clock_limit() {
    if !sandbox_ready("a_slicer_that_hangs_is_killed_at_the_wall_clock_limit") {
        return;
    }
    let dir = JobDir::new(&["box_20x30x10.stl"]);
    let script = Script::new(SlicerKind::PrusaSlicer, "sleep 60");
    let started = std::time::Instant::now();
    let error = slicer(2)
        .slice_with(&script.installation, &prusa_job(&dir, "box_20x30x10.stl"))
        .unwrap_err();
    assert_eq!(error.code(), "slicer_timeout");
    assert!(error.to_string().contains("2 s"), "{error}");
    assert!(
        started.elapsed().as_secs() < 30,
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn failures_and_missing_output_are_typed_errors() {
    if !sandbox_ready("failures_and_missing_output_are_typed_errors") {
        return;
    }
    let dir = JobDir::new(&["box_20x30x10.stl"]);
    let job = prusa_job(&dir, "box_20x30x10.stl");

    // The slicer's own words are passed on flattened, labelled and cut to their end.
    let noisy = Script::new(
        SlicerKind::PrusaSlicer,
        "i=0; while [ $i -lt 3000 ]; do echo 'filler filler filler filler'; i=$((i+1)); done\n\
         printf 'error:\\tcould not\\033[31m slice\\n' >&2; exit 7",
    );
    let error = slicer(60)
        .slice_with(&noisy.installation, &job)
        .unwrap_err();
    assert_eq!(error.code(), "slicer_failed");
    let text = error.to_string();
    assert!(
        text.contains("exit code 7") && text.contains("slicer output (untrusted)"),
        "{text}"
    );
    assert!(text.contains("error: could not [31m slice"), "{text}");
    assert!(text.len() < 1500, "{}", text.len());

    let silent = Script::new(SlicerKind::PrusaSlicer, "exit 0");
    let error = slicer(60)
        .slice_with(&silent.installation, &job)
        .unwrap_err();
    assert_eq!(error.code(), "unusable_output");

    let empty = Script::new(SlicerKind::PrusaSlicer, ": > output/plate_1.gcode");
    let error = slicer(60)
        .slice_with(&empty.installation, &job)
        .unwrap_err();
    assert_eq!(error.code(), "unusable_output");

    // G-code without a summary: a result with nulls and warnings, not guesses.
    let bare = Script::new(
        SlicerKind::PrusaSlicer,
        "printf 'G28\\nG1 X1\\n' > output/plate_1.gcode",
    );
    let result = slicer(60).slice_with(&bare.installation, &job).unwrap();
    assert_eq!((result.time_s, result.layers), (None, None));
    assert_eq!(
        (result.filament[0].grams, result.filament[0].metres),
        (None, None)
    );
    assert_eq!(result.filament[0].nozzle_id, 0);
    assert_eq!(
        result.sha256,
        sha256(&dir.path.join("output/plate_1.gcode"))
    );
    let codes = codes(&result.warnings);
    for code in ["summary_missing", "layers_unknown", "filament_use_unknown"] {
        assert!(codes.contains(&code), "{codes:?}");
    }
    assert_eq!(result.slicer.version, "unknown");
    assert!(
        serde_json::to_string(&result)
            .unwrap()
            .contains(r#""time_s":null"#)
    );

    // A refused installation never runs.
    let mut refused = Script::new(SlicerKind::PrusaSlicer, "echo ran > ran.txt");
    refused.installation.verdict = Verdict::Refuse("too old".into());
    let error = slicer(60)
        .slice_with(&refused.installation, &job)
        .unwrap_err();
    assert_eq!(error.code(), "slicer_version_refused");
    assert!(!dir.path.join("ran.txt").exists());
}

/// The stand-in slicer, copied next to a script so that the script can change what it is
/// told before passing it on: a program that does not do what its command line says.
fn disobedient(rewrite: &str) -> Script {
    let mut script = Script::new(
        SlicerKind::BambuStudio,
        &format!(
            "previous=\nfor a in \"$@\"; do\n  {rewrite}\n  previous=\"$a\"\n  set -- \"$@\" \"$a\"\n  shift\ndone\n\
             exec \"$(dirname \"$0\")/stand-in\" \"$@\""
        ),
    );
    let folder = script.installation.path.parent().unwrap().to_path_buf();
    std::fs::copy(fake_slicer(), folder.join("stand-in")).unwrap();
    // What comes back is the stand-in's file, which states no program version and has no
    // printing moves: it is judged as the stand-in's.
    script.installation.fake = true;
    script
}

#[test]
fn a_silent_bambu_studio_is_judged_by_its_result_file_and_its_sliced_file() {
    if !sandbox_ready("a_silent_bambu_studio_is_judged_by_its_result_file_and_its_sliced_file") {
        return;
    }
    // Scripts stand in for a real Bambu Studio whose installation holds the profile tree.
    let tree = SystemTree::new();
    let slicer = tree.slicer();
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );

    // The program says it worked, and what it wrote is not a sliced file.
    let garbage = Script::new(
        SlicerKind::BambuStudio,
        &format!(
            "printf 'this is not a zip archive at all, just text' > output/plate_1.gcode.3mf\n\
             cat > output/result.json <<'END'\n{RESULT_OK}\nEND"
        ),
    );
    let error = slicer.slice_with(&garbage.installation, &job).unwrap_err();
    assert_eq!(error.code(), "hostile_output");

    // It says it worked and wrote no sliced file at all.
    let no_file = Script::new(
        SlicerKind::BambuStudio,
        &format!("cat > output/result.json <<'END'\n{RESULT_OK}\nEND"),
    );
    let error = slicer.slice_with(&no_file.installation, &job).unwrap_err();
    assert_eq!(error.code(), "unusable_output");

    // The real program prints nothing and exits with 0 whatever happened. Without its
    // result file nothing says that it sliced, even when a sliced file from an earlier run
    // lies in the job folder.
    let stale = dir.path.join("plate_1.gcode.3mf");
    std::fs::copy(fixture("box_20x30x10.3mf"), &stale).unwrap();
    let silent = Script::new(SlicerKind::BambuStudio, "exit 0");
    let error = slicer.slice_with(&silent.installation, &job).unwrap_err();
    assert_eq!(error.code(), "result_missing");
    assert!(
        error.to_string().contains("output/result.json")
            && error.to_string().contains("printed nothing"),
        "{error}"
    );
    assert!(!stale.exists());

    // A failure is what the result file says, in the slicer's own words, with exit code 0
    // or without.
    for exit in [0, 5] {
        let failing = Script::new(
            SlicerKind::BambuStudio,
            &format!(
                "printf '{{\"error_string\": \"The input\\\\tfile is \\\\u001b[31mbroken.\", \
                 \"return_code\": -17, \"plate_index\": 0}}' > output/result.json; exit {exit}"
            ),
        );
        let error = slicer.slice_with(&failing.installation, &job).unwrap_err();
        assert_eq!(error.code(), "slicer_failed");
        let text = error.to_string();
        assert!(
            text.contains("-17") && text.contains("The input file is [31mbroken."),
            "{text}"
        );
    }

    // A result file that is not what it should be is hostile output, not a success.
    let liar = Script::new(
        SlicerKind::BambuStudio,
        "printf '[1, 2, 3]' > output/result.json",
    );
    let error = slicer.slice_with(&liar.installation, &job).unwrap_err();
    assert_eq!(error.code(), "hostile_output");

    // The command line is the measured one: every path absolute, `--slice 0`, the filament
    // map stated, `--arrange` stated. The plate is in the process settings (`curr_bed_type`,
    // measured), not on the command line. The working directory is the
    // job folder, so a file the program drops there (as `--info` was seen to do) stays
    // inside the job.
    let echo = Script::new(
        SlicerKind::BambuStudio,
        "echo \"$*\" > args.txt; pwd > cwd.txt; echo '{}' > result.json; exit 1",
    );
    let error = slicer.slice_with(&echo.installation, &job).unwrap_err();
    assert_eq!(error.code(), "slicer_failed");
    assert!(error.to_string().contains("exit code 1"), "{error}");
    let args = std::fs::read_to_string(dir.path.join("args.txt")).unwrap();
    let job_path = dir.path.display();
    assert_eq!(
        args.trim_end(),
        format!(
            "--debug 3 --slice 0 --load-settings {job_path}/profiles/machine.json;{job_path}/profiles/process.json \
             --load-filaments {job_path}/profiles/filament_1.json \
             --filament-map-mode=Manual --filament-map=1 --arrange 1 --outputdir {job_path}/output \
             --export-3mf plate_1.gcode.3mf {job_path}/input/box_20x30x10.3mf"
        )
    );
    assert_eq!(
        std::fs::read_to_string(dir.path.join("cwd.txt"))
            .unwrap()
            .trim_end(),
        job_path.to_string()
    );
    assert!(dir.path.join("result.json").is_file());

    // The settings it was given are the installation's, flattened, with the plate type set.
    let process = job_profile(&dir, "process.json");
    assert_eq!(process["layer_height"], "0.2");
    assert_eq!(process["wall_loops"], "2");
    assert_eq!(process["curr_bed_type"], "Textured PEI Plate");
    assert!(process.get("inherits").is_none(), "{process}");

    // Without the installation's profiles a real Bambu Studio is not run at all: the
    // built-in settings are for the stand-in slicer only.
    let mut config = SlicerConfig::new().unwrap();
    config.default_bed_type = Some("textured-pei".into());
    let plain = Slicer::new(config).unwrap();
    let ran = Script::new(SlicerKind::BambuStudio, "echo ran > ran.txt");
    let error = plain.slice_with(&ran.installation, &job).unwrap_err();
    assert_eq!(error.code(), "profile_not_found");
    assert!(error.to_string().contains("resources/profiles"), "{error}");
    assert!(!dir.path.join("ran.txt").exists());
}

#[test]
fn a_file_sliced_for_another_plate_or_with_moved_parts_is_refused() {
    if !sandbox_ready("a_file_sliced_for_another_plate_or_with_moved_parts_is_refused") {
        return;
    }
    let tree = SystemTree::new();
    let slicer = tree.slicer();
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let mut job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );
    job.arrange = false;
    job.extents[0].placed = true;

    // A program that does as it is told gives a result.
    let obedient = disobedient(":");
    let result = slicer.slice_with(&obedient.installation, &job).unwrap();
    assert_eq!(result.bed.as_ref().unwrap().id, "textured-pei");

    // One that ignores the plate type slices for its own default, the Cool Plate.
    let other_plate = disobedient(
        "case \"$a\" in *process.json) sed -i 's/Textured PEI Plate/Cool Plate/' \"${a#*;}\";; esac",
    );
    let error = slicer
        .slice_with(&other_plate.installation, &job)
        .unwrap_err();
    assert_eq!(error.code(), "bed_type_not_applied");
    assert!(
        error.to_string().contains("Textured PEI Plate")
            && error.to_string().contains("Cool Plate"),
        "{error}"
    );

    // One that arranges the parts although it was told not to.
    let arranging = disobedient("if [ \"$previous\" = --arrange ]; then a=1; fi");
    let error = slicer
        .slice_with(&arranging.installation, &job)
        .unwrap_err();
    assert_eq!(error.code(), "positions_not_kept");
    assert!(error.to_string().contains("X 0.0 to 20.0"), "{error}");
}

#[test]
fn a_slicer_in_a_shared_folder_reads_its_executable_and_nothing_beside_it() {
    use std::os::unix::fs::PermissionsExt;
    if !sandbox_ready("a_slicer_in_a_shared_folder_reads_its_executable_and_nothing_beside_it") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap().join("home/u");
    // The stand-in states its version only if it can neither read the file next to it nor
    // list the folder it lies in.
    let install = |folder: &str| {
        let folder = home.join(folder);
        std::fs::create_dir_all(&folder).unwrap();
        let private = folder.join("tax-return.txt");
        std::fs::write(&private, "private").unwrap();
        let path = folder.join("prusa-slicer");
        let body = format!(
            "#!/bin/sh\nif cat '{}' >/dev/null 2>&1 || ls '{}' >/dev/null 2>&1; then exit 1; fi\n\
             echo PrusaSlicer-2.9.7+linux-x64\n",
            private.display(),
            folder.display()
        );
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        hotend_slicer::detect::Candidate {
            kind: SlicerKind::PrusaSlicer,
            path,
            found: hotend_slicer::Found::Override,
        }
    };
    let policy = hotend_slicer::VersionPolicy::builtin().unwrap();

    let downloaded = install("Downloads");
    let installation = hotend_slicer::detect::probe(&downloaded, &policy, Some(&home));
    assert_eq!(
        installation.read_only,
        std::slice::from_ref(&downloaded.path)
    );
    assert_eq!(
        installation.version.as_ref().map(|v| v.as_str()),
        Some("2.9.7+linux-x64"),
        "the slicer could read what lies next to it, or could not run: {installation:?}"
    );

    // The same program in a folder of its own may read that folder, as before.
    let own = install("apps/PrusaSlicer");
    let installation = hotend_slicer::detect::probe(&own, &policy, Some(&home));
    assert_eq!(
        installation.read_only,
        [own.path.parent().unwrap().to_path_buf()]
    );
    assert_eq!(installation.version, None, "{installation:?}");
}

const MEASURED: &str = "02.09.01.70";

#[test]
fn a_file_that_does_not_state_its_plate_or_heats_the_bed_beyond_it_is_refused() {
    if !sandbox_ready("a_file_that_does_not_state_its_plate_or_heats_the_bed_beyond_it_is_refused")
    {
        return;
    }
    let tree = SystemTree::new();
    let slicer = tree.slicer();
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let job = placed_box_job(&dir);

    // A file that states the plate the job asked for and heats the bed as that plate's
    // setting says is a result. What is reported is the file's own statement.
    let good = replay(&real_shaped_gcode(MEASURED, TEXTURED_55, 55, BOX_KEPT));
    let result = slicer.slice_with(&good.installation, &job).unwrap();
    let bed = result.bed.as_ref().unwrap();
    assert_eq!(
        (bed.id.as_str(), bed.plate_type.as_str(), bed.temperature_c),
        ("textured-pei", "Textured PEI Plate", Some(55.0))
    );
    assert!(
        !codes(&result.warnings)
            .iter()
            .any(|c| c.starts_with("bed_"))
    );

    // A file that states no plate type was not sliced for the plate that was asked for,
    // whatever temperature it commands. Nothing is reported as if it had been.
    let silent = replay(&real_shaped_gcode(
        MEASURED,
        "; textured_plate_temp = 55\n; cool_plate_temp = 35\n",
        35,
        BOX_KEPT,
    ));
    let error = slicer.slice_with(&silent.installation, &job).unwrap_err();
    assert_eq!(error.code(), "bed_type_not_applied");
    assert!(
        error.to_string().contains("Textured PEI Plate")
            && error.to_string().contains("states no plate type"),
        "{error}"
    );

    // The plate is stated, but the commands heat the bed far beyond that plate's setting.
    let hot = replay(&real_shaped_gcode(MEASURED, TEXTURED_55, 110, BOX_KEPT));
    let error = slicer.slice_with(&hot.installation, &job).unwrap_err();
    assert_eq!(error.code(), "bed_temperature_not_applied");
    assert!(
        error.to_string().contains("110") && error.to_string().contains("55"),
        "{error}"
    );

    // The plate is stated, its temperature is not: the commands cannot be checked.
    let unstated = replay(&real_shaped_gcode(
        MEASURED,
        "; curr_bed_type = Textured PEI Plate\n",
        55,
        BOX_KEPT,
    ));
    let error = slicer.slice_with(&unstated.installation, &job).unwrap_err();
    assert_eq!(error.code(), "bed_temperature_not_applied");

    // A bed that stays colder than the plate's setting harms nobody; the result says so.
    let cold = replay(&real_shaped_gcode(MEASURED, TEXTURED_55, 35, BOX_KEPT));
    let result = slicer.slice_with(&cold.installation, &job).unwrap();
    assert_eq!(result.bed.as_ref().unwrap().temperature_c, Some(35.0));
    assert!(
        codes(&result.warnings).contains(&"bed_temperature_low"),
        "{:?}",
        result.warnings
    );
}

#[test]
fn parts_that_were_moved_or_cannot_be_located_are_refused() {
    if !sandbox_ready("parts_that_were_moved_or_cannot_be_located_are_refused") {
        return;
    }
    let tree = SystemTree::new();
    let slicer = tree.slicer();
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let mut job = placed_box_job(&dir);
    let sliced = |body: &str| real_shaped_gcode(MEASURED, TEXTURED_55, 55, body);

    // The parts were printed far from where the plate has them, and one line still
    // reaches over the place they should be in: the printed box covers the placed one.
    let moved = replay(&sliced(
        "G1 X200 Y200 Z0.2 F3000\nG1 X250 Y200 E1\nG1 X250 Y250 E1\nG1 X1 Y1 E1\n",
    ));
    let error = slicer.slice_with(&moved.installation, &job).unwrap_err();
    assert_eq!(error.code(), "positions_not_kept");
    assert!(error.to_string().contains("250.0"), "{error}");

    // A file without a move that prints: nothing says where the parts went.
    let nothing = replay(&sliced("G1 X5 Y5 Z0.2 F3000\n"));
    let error = slicer.slice_with(&nothing.installation, &job).unwrap_err();
    assert_eq!(error.code(), "positions_unknown");

    // The outline a real program recorded beside its G-code is its own claim, not what the
    // printer runs. It does not stand in for moves that cannot be read.
    let claimed = replay_archive(&sliced_archive(
        &sliced("G1 X5 Y5 Z0.2 F3000\n"),
        &[(
            "Metadata/plate_1.json",
            br#"{"bbox_all": [0.0, 0.0, 20.0, 30.0]}"#,
        )],
    ));
    let error = slicer.slice_with(&claimed.installation, &job).unwrap_err();
    assert_eq!(error.code(), "positions_unknown");

    // Parts in the middle of the bed with a brim around them: the printed box is a little
    // larger than the parts, which is what a brim or a skirt does.
    job.extents[0].min = [50.0, 50.0, 0.0];
    job.extents[0].max = [70.0, 80.0, 10.0];
    let brim = replay(&sliced(
        "G1 X45 Y45 Z0.2 F3000\nG1 X75 Y45 E1\nG1 X75 Y85 E1\nG1 X45 Y85 E1\nG1 X45 Y45 E1\n",
    ));
    let result = slicer.slice_with(&brim.installation, &job).unwrap();
    assert_eq!(result.extent.as_ref().unwrap().x, [45.0, 75.0]);
    assert!(!codes(&result.warnings).contains(&"printed_outside_parts"));

    // The same parts with something printed 100 mm away. One filament needs no prime
    // tower, so nothing explains it.
    let far = "G1 X50.2 Y50.2 Z0.2 F3000\nG1 X69.8 Y50.2 E1\nG1 X69.8 Y79.8 E1\n\
               G1 X170 Y180 F3000\nG1 X180 Y180 E1\n";
    let stray = replay(&sliced(far));
    let error = slicer.slice_with(&stray.installation, &job).unwrap_err();
    assert_eq!(error.code(), "positions_not_kept");
    // With two filaments a prime tower stands apart from the parts: a result that says
    // how far outside the parts the file prints.
    let mut two = job.clone();
    two.filaments.push(filament(2, "petg-basic", "PETG", 0));
    two.extents.push(hotend_slicer::Extent {
        slot: 2,
        ..two.extents[0].clone()
    });
    let result = slicer.slice_with(&stray.installation, &two).unwrap();
    let warning = result
        .warnings
        .iter()
        .find(|w| w.code == "printed_outside_parts")
        .unwrap();
    assert!(warning.message.contains("110"), "{}", warning.message);

    // Whatever the slicer was asked, a file that prints where the nozzle cannot go is
    // refused: the reach was checked on the parts, and the file is what the printer runs.
    let mut arranged = job.clone();
    arranged.arrange = true;
    arranged.extents[0].placed = false;
    let beyond = replay(&sliced(
        "G1 X240 Y100 Z0.2 F3000\nG1 X262 Y100 E1\nG1 X262 Y130 E1\n",
    ));
    let error = slicer
        .slice_with(&beyond.installation, &arranged)
        .unwrap_err();
    assert_eq!(error.code(), "out_of_reach");
    assert!(error.to_string().contains("262"), "{error}");
}

#[test]
fn the_version_the_file_states_is_the_one_the_policy_judges() {
    if !sandbox_ready("the_version_the_file_states_is_the_one_the_policy_judges") {
        return;
    }
    let tree = SystemTree::new();
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let job = placed_box_job(&dir);
    let sliced_by = |version: &str| real_shaped_gcode(version, TEXTURED_55, 55, BOX_KEPT);
    let said = |script: &mut Script, version: &str| {
        script.installation.version = hotend_slicer::SlicerVersion::parse(version);
    };

    // A policy with one pinned release, as the table is meant to become.
    let mut pinned = tree.config();
    pinned.policy = hotend_slicer::VersionPolicy::from_toml(
        "schema = 1\nrevision = \"test\"\n[[slicer]]\nkind = \"bambu-studio\"\nunknown = \"warn\"\n\
         [[slicer.rule]]\nfrom = \"2.9.1.70\"\nbelow = \"2.9.1.71\"\nverdict = \"allow\"\n\
         [[slicer.rule]]\nverdict = \"refuse\"\nreason = \"only the pinned release is run\"\n",
    )
    .unwrap();
    let pinned = Slicer::new(pinned).unwrap();

    // The program stated the pinned version when it was asked; the file it wrote states
    // another. The file is the statement of the program that really sliced.
    let mut other = replay(&sliced_by("01.02.03.04"));
    said(&mut other, MEASURED);
    let error = pinned.slice_with(&other.installation, &job).unwrap_err();
    assert_eq!(error.code(), "slicer_version_refused");
    assert!(error.to_string().contains("01.02.03.04"), "{error}");
    // A program that said nothing is judged the same way.
    let quiet = replay(&sliced_by("01.02.03.04"));
    let error = pinned.slice_with(&quiet.installation, &job).unwrap_err();
    assert_eq!(error.code(), "slicer_version_refused");
    // The pinned release, stated by both or by the file alone, is used without a warning.
    for stated in [Some(MEASURED), None] {
        let mut same = replay(&sliced_by(MEASURED));
        if let Some(version) = stated {
            said(&mut same, version);
        }
        let result = pinned.slice_with(&same.installation, &job).unwrap();
        assert_eq!(result.slicer.version, MEASURED);
        let codes = codes(&result.warnings);
        assert!(
            !codes.contains(&"slicer_version") && !codes.contains(&"slicer_version_mismatch"),
            "{codes:?}"
        );
    }

    // With the built-in policy (every version warns today) a differing file is reported
    // with the file's version, the policy's words about that version, and the difference.
    let builtin = tree.slicer();
    let mut newer = replay(&sliced_by("02.09.01.71"));
    said(&mut newer, MEASURED);
    let result = builtin.slice_with(&newer.installation, &job).unwrap();
    assert_eq!(result.slicer.version, "02.09.01.71");
    let version = result
        .warnings
        .iter()
        .find(|w| w.code == "slicer_version")
        .unwrap();
    assert!(
        version.message.contains("not the Bambu Studio version"),
        "{}",
        version.message
    );
    assert!(codes(&result.warnings).contains(&"slicer_version_mismatch"));
}

#[test]
fn a_real_slice_is_not_run_for_a_plate_nobody_chose() {
    if !sandbox_ready("a_real_slice_is_not_run_for_a_plate_nobody_chose") {
        return;
    }
    let tree = SystemTree::new();
    let mut config = tree.config();
    config.default_bed_type = None;
    let slicer = Slicer::new(config).unwrap();
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let mut job = placed_box_job(&dir);

    // Neither the job nor the settings say which plate is on the printer. The program's
    // own default heats for the Cool Plate, and Hotend's guess would be no better.
    let ran = Script::new(SlicerKind::BambuStudio, "echo ran > ran.txt");
    let error = slicer.slice_with(&ran.installation, &job).unwrap_err();
    assert_eq!(error.code(), "bed_type_not_chosen");
    let text = error.to_string();
    assert!(
        text.contains("`plate_type`") && text.contains("cool") && text.contains("textured-pei"),
        "{text}"
    );
    assert!(!dir.path.join("ran.txt").exists());
    assert!(!dir.path.join("profiles").exists());

    // With a plate the program runs.
    job.bed_type = Some("cool".into());
    let error = slicer.slice_with(&ran.installation, &job).unwrap_err();
    assert_eq!(error.code(), "result_missing");
    assert!(dir.path.join("ran.txt").exists());

    // The stand-in's file is not printable, so it may still slice for a stated default.
    job.bed_type = None;
    let result = slicer.slice(&job).unwrap();
    assert_eq!(result.bed.unwrap().id, "textured-pei");
}

#[test]
fn a_failed_real_run_names_what_no_real_run_has_confirmed() {
    if !sandbox_ready("a_failed_real_run_names_what_no_real_run_has_confirmed") {
        return;
    }
    let tree = SystemTree::new();
    let slicer = tree.slicer();
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let mut job = placed_box_job(&dir);
    job.process_profile = "fine-0.12mm".into();
    let unconfirmed = [
        "--filament-map-mode=Manual --filament-map=1",
        "0.12mm Fine @BBL X2D",
    ];

    // The real program prints nothing. When it leaves no result, or one that says it
    // failed, the error names the parts of the command line no real run has confirmed.
    for (body, code) in [
        ("exit 0", "result_missing"),
        ("exit 3", "slicer_failed"),
        (
            "printf '{\"error_string\": \"unknown option\", \"return_code\": -2}' > output/result.json",
            "slicer_failed",
        ),
    ] {
        let failing = Script::new(SlicerKind::BambuStudio, body);
        let error = slicer.slice_with(&failing.installation, &job).unwrap_err();
        assert_eq!(error.code(), code, "{body}");
        let text = error.to_string();
        for part in unconfirmed {
            assert!(text.contains(part), "{body}: `{part}` is not in: {text}");
        }
    }

    // What was measured is not listed: the plate, which goes into the process settings and
    // not onto the command line, and the profile names the real program was run with.
    job.process_profile = "standard-0.20mm".into();
    job.bed_type = Some("cool".into());
    let silent = Script::new(SlicerKind::BambuStudio, "exit 0");
    let text = slicer
        .slice_with(&silent.installation, &job)
        .unwrap_err()
        .to_string();
    assert!(
        !text.contains("curr-bed-type") && !text.contains("has not been sliced with"),
        "{text}"
    );
    assert!(!text.contains("0.20mm Standard"), "{text}");
}

/// The fixtures' 3MF with further entries.
fn model_with(extra: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Read;
    let mut archive =
        zip::ZipArchive::new(std::fs::File::open(fixture("box_20x30x10.3mf")).unwrap()).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        let name = String::from_utf8(entry.name_raw().to_vec()).unwrap();
        entries.push((name, bytes));
    }
    let mut all: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    all.extend_from_slice(extra);
    zip_of(&all)
}

#[test]
fn a_model_file_that_carries_more_than_the_model_is_not_handed_to_the_slicer() {
    if !sandbox_ready("a_model_file_that_carries_more_than_the_model_is_not_handed_to_the_slicer") {
        return;
    }
    let tree = SystemTree::new();
    let slicer = tree.slicer();
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let mut job = placed_box_job(&dir);
    job.inputs = vec!["input/plate.3mf".into()];
    let plate = dir.path.join("input/plate.3mf");
    let ran = Script::new(SlicerKind::BambuStudio, "echo ran > ran.txt");
    let not_run = || !dir.path.join("ran.txt").exists() && !dir.path.join("profiles").exists();

    // Slicer settings, per-layer G-code and per-object settings that a project file can
    // carry. The plate is written by the sandboxed worker, so it is hostile input too.
    for extra in [
        "Metadata/project_settings.config",
        "Metadata/custom_gcode_per_layer.xml",
        "Metadata/model_settings.config",
        "Metadata/Slic3r_PE.config",
        "Metadata/plate_1.gcode",
        "3D/Objects/part.gcode",
        "anything.txt",
    ] {
        std::fs::write(&plate, model_with(&[(extra, b"x = 1")])).unwrap();
        let error = slicer.slice_with(&ran.installation, &job).unwrap_err();
        assert_eq!(error.code(), "input_refused", "{extra}");
        assert!(error.to_string().contains(extra), "{error}");
        assert!(not_run(), "{extra}");
    }
    // The stand-in is not given such a file either, and neither is PrusaSlicer, which
    // reads settings from a project file of its own kind.
    assert_eq!(slicer.slice(&job).unwrap_err().code(), "input_refused");
    let prusa = slicer_with_fake(&[SlicerKind::PrusaSlicer]);
    let mut prusa_job = prusa_job(&dir, "plate.3mf");
    prusa_job.inputs = job.inputs.clone();
    assert_eq!(prusa.slice(&prusa_job).unwrap_err().code(), "input_refused");

    // Something that is not an archive at all, and an archive with an unsafe name.
    std::fs::write(&plate, b"solid not a zip archive").unwrap();
    let error = slicer.slice_with(&ran.installation, &job).unwrap_err();
    assert_eq!(error.code(), "input_refused");
    std::fs::write(&plate, model_with(&[("3D/Objects/../../x.model", b"x")])).unwrap();
    let error = slicer.slice_with(&ran.installation, &job).unwrap_err();
    assert_eq!(error.code(), "input_refused");
    assert!(not_run());

    // The model alone, also split into object files, is sliced.
    std::fs::write(
        &plate,
        model_with(&[
            ("3D/Objects/object_1.model", b"<model/>"),
            ("3D/_rels/3dmodel.model.rels", b"<Relationships/>"),
        ]),
    )
    .unwrap();
    let result = slicer.slice(&job).unwrap();
    assert_eq!(result.layers, Some(50));
}
