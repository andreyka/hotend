//! Hotend's print settings on the way to a real Bambu Studio, with scripts standing in for the
//! program inside the sandbox: the plan's values reach the flattened profiles through the keymap,
//! a value never measured is refused before the program runs, the plate of the plan and of the
//! job must agree, a nozzle size without a profile is refused, and a slice without settings is
//! labelled. The profile tree is made up; only key names are the measured ones.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[cfg(unix)]
mod common;

#[cfg(unix)]
mod unix {
    use super::common::*;
    use hotend_slicer::SlicerKind;

    /// A tree whose process and filament profiles carry the keys of the holder recipe, with
    /// made-up stock values.
    fn recipe_tree() -> SystemTree {
        let tree = SystemTree::new();
        tree.put(
            "process",
            "0.20mm Standard @BBL X2D",
            r#"{"type": "process", "name": "0.20mm Standard @BBL X2D", "from": "system",
                "instantiation": "true", "inherits": "fdm_process_common",
                "layer_height": "0.2", "initial_layer_print_height": "0.2",
                "brim_width": "5", "brim_object_gap": "0.1",
                "skirt_loops": "0", "initial_layer_speed": ["50", "50", "50", "50", "50", "50"],
                "initial_layer_infill_speed": ["105", "105", "105", "105", "105", "105"],
                "enable_support": "0", "enable_prime_tower": "1",
                "sparse_infill_density": "15%", "sparse_infill_pattern": "grid",
                "seam_position": "aligned",
                "compatible_printers": ["Bambu Lab X2D 0.4 nozzle"]}"#,
        );
        tree.put(
            "filament",
            "Generic PLA @BBL X2D 0.4 nozzle",
            r#"{"type": "filament", "name": "Generic PLA @BBL X2D 0.4 nozzle", "from": "system",
                "instantiation": "true", "inherits": "fdm_filament_common",
                "filament_type": ["PLA"], "filament_density": ["1.24"],
                "textured_plate_temp": ["55"], "textured_plate_temp_initial_layer": ["55"],
                "close_fan_the_first_x_layers": ["1"], "first_x_layer_fan_speed": ["40"],
                "additional_cooling_fan_speed": ["70"],
                "compatible_printers": ["Bambu Lab X2D 0.4 nozzle"]}"#,
        );
        tree
    }

    fn recipe_job(dir: &JobDir) -> hotend_slicer::SliceJob {
        let mut job = placed_box_job(dir);
        job.filaments = vec![filament(1, "pla-generic", "PLA", 1)];
        job.bed_type = Some("textured-pei".into());
        job.print_settings = Some(recipe_settings());
        job.nozzle_diameter_mm = Some(0.4);
        job
    }

    #[test]
    fn the_plans_values_reach_the_profiles_and_an_unproven_file_is_refused() {
        if !sandbox_ready("the_plans_values_reach_the_profiles_and_an_unproven_file_is_refused") {
            return;
        }
        let tree = recipe_tree();
        let slicer = tree.slicer();
        let dir = JobDir::new(&["box_20x30x10.3mf"]);
        let job = recipe_job(&dir);
        let silent = Script::new(SlicerKind::BambuStudio, "echo \"$*\" > args.txt; exit 0");
        let error = slicer.slice_with(&silent.installation, &job).unwrap_err();
        assert_eq!(error.code(), "result_missing");
        // The profiles the program was given: the installed ones with the plan on top.
        let process = job_profile(&dir, "process.json");
        assert_eq!(process["curr_bed_type"], "Textured PEI Plate");
        assert_eq!(process["brim_type"], "outer_only");
        assert_eq!(process["brim_object_gap"], "0");
        assert_eq!(process["wall_loops"], "3");
        assert_eq!(process["enable_prime_tower"], "0");
        assert_eq!(
            process["initial_layer_speed"],
            serde_json::json!(["20", "20", "20", "20", "20", "20"])
        );
        let filament = job_profile(&dir, "filament_1.json");
        assert_eq!(
            filament["textured_plate_temp_initial_layer"],
            serde_json::json!(["65"])
        );
        assert_eq!(filament["textured_plate_temp"], serde_json::json!(["60"]));
        assert_eq!(
            filament["additional_cooling_fan_speed"],
            serde_json::json!(["30"])
        );
        assert_eq!(filament["filament_colour"], serde_json::json!(["#898989"]));
        // The machine profile gets no inventory for a slice.
        let machine = job_profile(&dir, "machine.json");
        assert!(machine.get("extruder_nozzle_count").is_none(), "{machine}");
        // The plate is in the process settings, not on the command line.
        let args = std::fs::read_to_string(dir.path.join("args.txt")).unwrap();
        assert!(!args.contains("curr-bed-type"), "{args}");
        assert!(
            args.contains("Generic") || args.contains("filament_1.json"),
            "{args}"
        );

        // A program that hands back a file that shows the asked settings and a first layer
        // that keeps to them: the slice is proven. One that hands back a file without them:
        // refused by the proof, and nothing of it is a result.
        let config = echo_block(&recipe_settings());
        let proven = replay(&real_shaped_gcode("02.09.01.70", &config, 65, RECIPE_BODY));
        let result = slicer.slice_with(&proven.installation, &job).unwrap();
        let codes = codes(&result.warnings);
        assert!(codes.contains(&"print_settings_proven"), "{codes:?}");
        let unproven = replay(&real_shaped_gcode("02.09.01.70", TEXTURED_55, 55, BOX_KEPT));
        let error = slicer.slice_with(&unproven.installation, &job).unwrap_err();
        assert_eq!(error.code(), "proof_incomplete", "{error}");
        let fast = replay(&real_shaped_gcode(
            "02.09.01.70",
            &config,
            65,
            &RECIPE_BODY.replace("F1200", "F3000"),
        ));
        let error = slicer.slice_with(&fast.installation, &job).unwrap_err();
        assert_eq!(error.code(), "first_layer_too_fast", "{error}");
    }

    /// Four layers of the fixtures' box where it lies, made up for these tests: the fans set
    /// before the first layer, brim and walls at 20 mm/s and the bottom at 30 mm/s on the first
    /// layer, the bed lowered to 60 C on layer 2, the part fan on from layer 4.
    const RECIPE_BODY: &str = "\
M106 S0
M106 P2 S0
M106 P10 S0
; CHANGE_LAYER
; FEATURE: Brim
G1 X0.2 Y0.2 Z0.2 F1200
G1 X19.8 Y0.2 E1
; FEATURE: Outer wall
G1 X19.8 Y29.8 E1
; FEATURE: Bottom surface
G1 X0.2 Y29.8 E1 F1800
; CHANGE_LAYER
M140 S60
; FEATURE: Outer wall
G1 X0.2 Y0.2 Z0.4 E1 F6000
; CHANGE_LAYER
G1 X19.8 Y0.2 Z0.6 E1
; CHANGE_LAYER
M106 S255
G1 X19.8 Y29.8 Z0.8 E1
";

    #[test]
    fn a_value_never_measured_is_refused_before_the_program_runs() {
        if !sandbox_ready("a_value_never_measured_is_refused_before_the_program_runs") {
            return;
        }
        let tree = recipe_tree();
        let slicer = tree.slicer();
        let dir = JobDir::new(&["box_20x30x10.3mf"]);
        let mut job = recipe_job(&dir);
        // The slender plan's first layer of 0.25 mm (level 2 on Textured PEI) has never been
        // sliced with. (Until 18 November 2026 the calm plan's `no_brim` was refused here.)
        job.print_settings = Some(slender_settings());
        let ran = Script::new(SlicerKind::BambuStudio, "echo ran > ran.txt");
        let error = slicer.slice_with(&ran.installation, &job).unwrap_err();
        assert_eq!(error.code(), "setting_unmeasured");
        let text = error.to_string();
        assert!(
            text.contains("first layer height") && text.contains("never been measured"),
            "{text}"
        );
        assert!(!dir.path.join("ran.txt").exists());

        // A key the installed profile lacks is not invented.
        tree.put(
            "filament",
            "Generic PLA @BBL X2D 0.4 nozzle",
            r#"{"type": "filament", "name": "Generic PLA @BBL X2D 0.4 nozzle", "from": "system",
                "instantiation": "true", "inherits": "fdm_filament_common",
                "filament_density": ["1.24"], "textured_plate_temp": ["55"],
                "compatible_printers": ["Bambu Lab X2D 0.4 nozzle"]}"#,
        );
        let mut job = recipe_job(&dir);
        job.nozzle_diameter_mm = Some(0.4);
        let error = slicer.slice_with(&ran.installation, &job).unwrap_err();
        assert_eq!(error.code(), "setting_key_missing");
        assert!(!dir.path.join("ran.txt").exists());
    }

    #[test]
    fn plate_nozzle_and_missing_settings_are_named() {
        if !sandbox_ready("plate_nozzle_and_missing_settings_are_named") {
            return;
        }
        let tree = recipe_tree();
        let slicer = tree.slicer();
        let dir = JobDir::new(&["box_20x30x10.3mf"]);
        let ran = Script::new(SlicerKind::BambuStudio, "echo ran > ran.txt");

        // The plan is for the Textured PEI plate; the job asks for another.
        let mut job = recipe_job(&dir);
        job.bed_type = Some("cool".into());
        let error = slicer.slice_with(&ran.installation, &job).unwrap_err();
        assert_eq!(error.code(), "invalid_job");
        assert!(error.to_string().contains("textured-pei"), "{error}");

        // A nozzle Hotend has no profile for, for the real program and for the stand-in.
        let mut job = recipe_job(&dir);
        job.nozzle_diameter_mm = Some(0.6);
        let error = slicer.slice_with(&ran.installation, &job).unwrap_err();
        assert_eq!(error.code(), "nozzle_not_supported");
        let text = error.to_string();
        assert!(text.contains("0.6 mm") && text.contains("0.4"), "{text}");
        assert!(!dir.path.join("ran.txt").exists());
        let stand_in_tree = SystemTree::new();
        let stand_in = stand_in_tree.slicer();
        let mut job = placed_box_job(&dir);
        job.nozzle_diameter_mm = Some(0.6);
        assert_eq!(
            stand_in.slice(&job).unwrap_err().code(),
            "nozzle_not_supported"
        );

        // Without print settings a real slice runs as before and says so.
        let mut job = recipe_job(&dir);
        job.print_settings = None;
        let silent = Script::new(SlicerKind::BambuStudio, "exit 0");
        let error = slicer.slice_with(&silent.installation, &job).unwrap_err();
        assert_eq!(error.code(), "result_missing");
        let process = job_profile(&dir, "process.json");
        assert!(process.get("brim_type").is_none(), "{process}");

        // The stand-in slices as it always has, settings or not, and its file stays labelled.
        let mut job = placed_box_job(&dir);
        job.print_settings = Some(recipe_settings());
        job.bed_type = Some("textured-pei".into());
        let result = stand_in.slice(&job).unwrap();
        let codes = codes(&result.warnings);
        assert!(codes.contains(&"fake_slicer"), "{codes:?}");
        assert!(!codes.contains(&"print_settings_proven"), "{codes:?}");
        assert!(!codes.contains(&"print_settings_missing"), "{codes:?}");

        // A project is made by the real program only.
        let error = stand_in.export_project(&job).unwrap_err();
        assert_eq!(error.code(), "project_export_failed");
    }
}
