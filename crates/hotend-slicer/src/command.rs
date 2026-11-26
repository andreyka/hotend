//! Builds the command lines of the vendor slicers.
//!
//! Sources:
//! - Bambu Studio: the wiki page "Command Line Usage"
//!   (<https://raw.githubusercontent.com/wiki/bambulab/BambuStudio/Command-Line-Usage.md>):
//!   `--debug`, `--load-settings "machine.json;process.json"`, `--load-filaments
//!   "a.json;b.json"`, `--arrange`, `--slice <plate>`, `--outputdir`, `--export-3mf`, and
//!   `--key=value` for single settings (its example is `--curr-bed-type`); and a run of
//!   the real program, version 02.09.01.70, on 17 November 2026, which showed that
//!   `--debug 3 --slice 0` with `--arrange 0` slices a plain 3MF where its parts lie, that
//!   without `--arrange 0` the program arranges the parts itself, and that the sliced
//!   file, `plate_<n>.gcode` and `result.json` appear in `--outputdir`;
//! - PrusaSlicer: the program's own `--help` text: `--export-gcode`, `--datadir`, `--load`
//!   (repeatable), `--loglevel`, `--dont-arrange`, `--output`.
//!
//! Guarantees: the functions are pure; no argument is derived from model content or from
//! text an AI wrote, except profile ids, which are validated and only ever select a file;
//! `--arrange` is always stated, so the slicer never decides by itself whether to move the
//! parts; the plate type is always stated for Bambu Studio, whose own default is the Cool
//! Plate; and for a printer with a filament map rule the manual mode and the explicit map
//! are always present.
//!
//! The measured command line is a part of the one built here, not all of it. The run
//! passed `--debug 3 --slice 0 --load-settings .. --load-filaments .. --arrange 0
//! --outputdir .. --export-3mf .. <input>`. The plate type option and the two filament map
//! options were not in it; each has a `verified` flag in its data file
//! (`profiles/system-profiles.toml`, `profiles/printers.toml`), and a run that fails names
//! the ones that are still `false` ([`Invocation::unconfirmed`]).
//!
//! Bambu Studio is given absolute paths, as in the measured run. They go into
//! `;`-separated lists, so [`bambu_studio`] must not be given a job folder with a `;` in
//! its path; [`crate::Slicer`] refuses such a folder. PrusaSlicer is given the fixed
//! relative names, resolved against the working directory, which the sandbox launcher sets
//! to the job folder.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::printer::FilamentMapRule;
use crate::version::SlicerKind;

/// Folder below the job folder that holds the settings files this crate writes.
pub const PROFILES_DIR: &str = "profiles";
/// Folder below the job folder where the slicer writes.
pub const OUTPUT_DIR: &str = "output";
/// Folder below the job folder that PrusaSlicer uses as its data directory.
pub const CONFIG_DIR: &str = "slicer-config";

/// Everything a command line is built from. Paths are relative to the job folder, with `/`.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan<'a> {
    pub job_dir: &'a Path,
    pub inputs: &'a [String],
    pub filament_slots: usize,
    /// The explicit filament map (slicer extruder per slot) and how to write it.
    pub filament_map: Option<(&'a FilamentMapRule, &'a [u32])>,
    pub plate: u32,
    pub arrange: bool,
    /// The plate type. Bambu Studio only.
    pub bed_type: Option<BedArgument<'a>>,
    /// What the run makes. Bambu Studio only; PrusaSlicer always slices.
    pub mode: Mode,
}

/// What a Bambu Studio run makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Slice the plate and export the sliced file.
    #[default]
    Slice,
    /// Run A: export an editable project from the plate and the settings files, without
    /// `--slice`, so the project holds no G-code (measured).
    ExportProject,
    /// Run C: slice an exported project alone, with no settings option at all, so the run
    /// shows whether the project carries everything it needs.
    ResliceProject,
}

/// File name of run A's project.
pub const PROJECT_NAME: &str = "project.3mf";
/// File name of run C's sliced file.
pub const RESLICED_NAME: &str = "sliced.gcode.3mf";

/// How the plate type is written on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BedArgument<'a> {
    /// The option, for example `--curr-bed-type`.
    pub option: &'a str,
    /// The slicer's name of the plate type.
    pub value: &'a str,
    /// Whether a real program has been run with the option.
    pub option_verified: bool,
    /// Whether a real program has stated or accepted this plate name.
    pub value_verified: bool,
}

/// A ready command line and where its output is expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub args: Vec<OsString>,
    /// Paths relative to the job folder where the sliced file may appear, most likely first.
    pub output_candidates: Vec<String>,
    /// The parts of this command line that no run of a real program has confirmed, as
    /// they are written here, each with what is not known about it.
    pub unconfirmed: Vec<String>,
}

/// Relative path of a settings file in the job folder: `profiles/machine.json`,
/// `profiles/filament_2.ini`, ...
pub fn settings_path(kind: SlicerKind, role: &str, slot: Option<u32>) -> String {
    let extension = crate::settings::settings_extension(kind);
    match slot {
        Some(slot) => format!("{PROFILES_DIR}/{role}_{slot}.{extension}"),
        None => format!("{PROFILES_DIR}/{role}.{extension}"),
    }
}

/// File name of the sliced output for a plate.
pub fn output_name(kind: SlicerKind, plate: u32) -> String {
    match kind {
        SlicerKind::BambuStudio => format!("plate_{plate}.gcode.3mf"),
        SlicerKind::PrusaSlicer => format!("plate_{plate}.gcode"),
    }
}

/// The command line for `kind`.
pub fn build(kind: SlicerKind, plan: &Plan<'_>) -> Invocation {
    match kind {
        SlicerKind::BambuStudio => bambu_studio(plan),
        SlicerKind::PrusaSlicer => prusa_slicer(plan),
    }
}

fn filament_files(kind: SlicerKind, slots: usize) -> Vec<String> {
    (1..=slots as u32)
        .map(|slot| settings_path(kind, "filament", Some(slot)))
        .collect()
}

/// The `--debug` level of the measured run.
const BAMBU_DEBUG_LEVEL: &str = "3";

/// `bambu-studio --debug 3 --slice <0|n> --load-settings "<job>/profiles/machine.json;
/// <job>/profiles/process.json" --load-filaments "<job>/profiles/filament_1.json;..."
/// --curr-bed-type=<plate> --filament-map-mode=Manual --filament-map=1,2 --arrange <0|1>
/// --outputdir <job>/output --export-3mf plate_<n>.gcode.3mf <job>/<inputs>`
///
/// Plate 1 is asked for as `--slice 0` ("all plates"), the form that was run against the
/// real program; a plate file prepared by Hotend holds one plate. Any other plate is
/// asked for by its number, as the wiki documents.
pub fn bambu_studio(plan: &Plan<'_>) -> Invocation {
    let kind = SlicerKind::BambuStudio;
    let absolute = |relative: &str| plan.job_dir.join(relative).to_string_lossy().into_owned();
    let mut args: Vec<OsString> = vec!["--debug".into(), BAMBU_DEBUG_LEVEL.into()];
    let name = match plan.mode {
        Mode::Slice => output_name(kind, plan.plate),
        Mode::ExportProject => PROJECT_NAME.to_owned(),
        Mode::ResliceProject => RESLICED_NAME.to_owned(),
    };
    if plan.mode == Mode::ResliceProject {
        // Measured (run C of 17 November 2026): the project alone, no settings option.
        args.extend(
            ["--slice", "0", "--arrange", "0", "--outputdir"]
                .into_iter()
                .map(OsString::from),
        );
        args.push(plan.job_dir.join(OUTPUT_DIR).into());
        args.push("--export-3mf".into());
        args.push(name.clone().into());
        args.extend(
            plan.inputs
                .iter()
                .map(|input| OsString::from(absolute(input))),
        );
        return Invocation {
            args,
            output_candidates: vec![format!("{OUTPUT_DIR}/{name}")],
            unconfirmed: Vec::new(),
        };
    }
    let mut unconfirmed = Vec::new();
    if plan.mode == Mode::Slice {
        args.push("--slice".into());
        args.push(
            if plan.plate == 1 { 0 } else { plan.plate }
                .to_string()
                .into(),
        );
    }
    args.push("--load-settings".into());
    args.push(
        format!(
            "{};{}",
            absolute(&settings_path(kind, "machine", None)),
            absolute(&settings_path(kind, "process", None))
        )
        .into(),
    );
    args.push("--load-filaments".into());
    args.push(
        filament_files(kind, plan.filament_slots)
            .iter()
            .map(|file| absolute(file))
            .collect::<Vec<_>>()
            .join(";")
            .into(),
    );
    if let Some(bed) = plan.bed_type {
        let argument = format!("{}={}", bed.option, bed.value);
        match (bed.option_verified, bed.value_verified) {
            (true, true) => {}
            (false, true) => unconfirmed.push(format!(
                "`{argument}` (the option has not been run; the plate name is the one a \
                 real program stated)"
            )),
            (_, false) => unconfirmed.push(format!(
                "`{argument}` (the plate name `{}` has not been sliced with)",
                bed.value
            )),
        }
        args.push(argument.into());
    }
    if let Some((rule, map)) = plan.filament_map {
        let numbers: Vec<String> = map.iter().map(u32::to_string).collect();
        let mode = format!("{}={}", rule.mode_option, rule.mode_value);
        let maps = format!(
            "{}={}",
            rule.maps_option,
            numbers.join(&rule.maps_separator)
        );
        if !rule.verified {
            unconfirmed.push(format!(
                "`{mode} {maps}` (the spelling of the filament map options has not been run)"
            ));
        }
        args.push(mode.into());
        args.push(maps.into());
    }
    args.push("--arrange".into());
    args.push(if plan.arrange { "1" } else { "0" }.into());
    if plan.arrange {
        unconfirmed
            .push("`--arrange 1` (the runs so far had `--arrange 0` or no such option)".to_owned());
    }
    if plan.plate != 1 && plan.mode == Mode::Slice {
        unconfirmed.push(format!(
            "`--slice {}` (only `--slice 0` has been run)",
            plan.plate
        ));
    }
    args.push("--outputdir".into());
    args.push(plan.job_dir.join(OUTPUT_DIR).into());
    args.push("--export-3mf".into());
    args.push(name.clone().into());
    args.extend(
        plan.inputs
            .iter()
            .map(|input| OsString::from(absolute(input))),
    );
    Invocation {
        args,
        // Measured: the file named by `--export-3mf` appears in `--outputdir`. The working
        // directory is checked too, for a version that resolves the name against it.
        output_candidates: vec![format!("{OUTPUT_DIR}/{name}"), name],
        unconfirmed,
    }
}

/// `prusa-slicer --export-gcode --datadir <job>/slicer-config --loglevel 2 --load machine
/// --load process --load filament [--dont-arrange] --output <job>/output/plate_1.gcode
/// <inputs>`
pub fn prusa_slicer(plan: &Plan<'_>) -> Invocation {
    let kind = SlicerKind::PrusaSlicer;
    let name = output_name(kind, plan.plate);
    let output: PathBuf = plan.job_dir.join(OUTPUT_DIR).join(&name);
    let mut args: Vec<OsString> = vec!["--export-gcode".into()];
    args.push("--datadir".into());
    args.push(plan.job_dir.join(CONFIG_DIR).into());
    args.push("--loglevel".into());
    args.push("2".into());
    let mut settings = vec![
        settings_path(kind, "machine", None),
        settings_path(kind, "process", None),
    ];
    settings.extend(filament_files(kind, plan.filament_slots));
    for file in settings {
        args.push("--load".into());
        args.push(file.into());
    }
    if !plan.arrange {
        args.push("--dont-arrange".into());
    }
    args.push("--output".into());
    args.push(output.into());
    args.extend(plan.inputs.iter().map(OsString::from));
    Invocation {
        args,
        output_candidates: vec![format!("{OUTPUT_DIR}/{name}")],
        unconfirmed: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::printer::PrinterFacts;

    fn strings(invocation: &Invocation) -> Vec<String> {
        invocation
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn textured() -> BedArgument<'static> {
        BedArgument {
            option: "--curr-bed-type",
            value: "Textured PEI Plate",
            option_verified: false,
            value_verified: false,
        }
    }

    /// The command line as it is documented in PROTOCOL.md section 6. Its measured part is
    /// asserted by `the_measured_part_of_the_bambu_studio_command_line_is_unchanged`.
    #[test]
    fn bambu_studio_command_line_carries_the_plate_type_and_the_manual_map() {
        let facts = PrinterFacts::builtin().unwrap();
        let rule = facts
            .printer("bambu-x2d")
            .unwrap()
            .filament_map
            .as_ref()
            .unwrap();
        let inputs = ["input/model.3mf".to_owned()];
        let invocation = bambu_studio(&Plan {
            job_dir: Path::new("/data/job 1"),
            inputs: &inputs,
            filament_slots: 2,
            filament_map: Some((rule, &[2, 1])),
            plate: 1,
            arrange: false,
            bed_type: Some(textured()),
            mode: Mode::Slice,
        });
        assert_eq!(
            strings(&invocation),
            [
                "--debug",
                "3",
                "--slice",
                "0",
                "--load-settings",
                "/data/job 1/profiles/machine.json;/data/job 1/profiles/process.json",
                "--load-filaments",
                "/data/job 1/profiles/filament_1.json;/data/job 1/profiles/filament_2.json",
                "--curr-bed-type=Textured PEI Plate",
                "--filament-map-mode=Manual",
                "--filament-map=2,1",
                "--arrange",
                "0",
                "--outputdir",
                "/data/job 1/output",
                "--export-3mf",
                "plate_1.gcode.3mf",
                "/data/job 1/input/model.3mf",
            ]
        );
        assert_eq!(
            invocation.output_candidates,
            ["output/plate_1.gcode.3mf", "plate_1.gcode.3mf"]
        );
        // Both additions to the measured command line are named as not confirmed.
        assert_eq!(
            invocation.unconfirmed.len(),
            2,
            "{:?}",
            invocation.unconfirmed
        );
        assert!(invocation.unconfirmed[0].contains("--curr-bed-type=Textured PEI Plate"));
        assert!(
            invocation.unconfirmed[1].contains("--filament-map-mode=Manual --filament-map=2,1")
        );
    }

    /// The run of 02.09.01.70 on 17 November 2026 was `--debug 3 --slice 0 --load-settings
    /// <machine;process> --load-filaments <filament> --arrange 0 --outputdir <dir>
    /// --export-3mf plate_1.gcode.3mf <input>`, every path absolute. Without the options
    /// Hotend adds, the command line built here is that one, word for word.
    #[test]
    fn the_measured_part_of_the_bambu_studio_command_line_is_unchanged() {
        let inputs = ["input/plate.3mf".to_owned()];
        let invocation = bambu_studio(&Plan {
            job_dir: Path::new("/data/job"),
            inputs: &inputs,
            filament_slots: 1,
            filament_map: None,
            plate: 1,
            arrange: false,
            bed_type: None,
            mode: Mode::Slice,
        });
        assert_eq!(
            strings(&invocation),
            [
                "--debug",
                "3",
                "--slice",
                "0",
                "--load-settings",
                "/data/job/profiles/machine.json;/data/job/profiles/process.json",
                "--load-filaments",
                "/data/job/profiles/filament_1.json",
                "--arrange",
                "0",
                "--outputdir",
                "/data/job/output",
                "--export-3mf",
                "plate_1.gcode.3mf",
                "/data/job/input/plate.3mf",
            ]
        );
        assert!(invocation.unconfirmed.is_empty());
        // With the additions, the measured words keep their order around them.
        let facts = PrinterFacts::builtin().unwrap();
        let rule = facts
            .printer("bambu-x2d")
            .unwrap()
            .filament_map
            .as_ref()
            .unwrap();
        let full = strings(&bambu_studio(&Plan {
            job_dir: Path::new("/data/job"),
            inputs: &inputs,
            filament_slots: 1,
            filament_map: Some((rule, &[1])),
            plate: 1,
            arrange: false,
            bed_type: Some(BedArgument {
                option_verified: false,
                value_verified: true,
                value: "Cool Plate",
                ..textured()
            }),
            mode: Mode::Slice,
        }));
        let added: Vec<&String> = full
            .iter()
            .filter(|word| !strings(&invocation).contains(word))
            .collect();
        assert_eq!(
            added,
            [
                "--curr-bed-type=Cool Plate",
                "--filament-map-mode=Manual",
                "--filament-map=1"
            ]
        );
        let measured: Vec<&String> = full.iter().filter(|word| !added.contains(word)).collect();
        assert_eq!(measured, strings(&invocation).iter().collect::<Vec<_>>());
    }

    #[test]
    fn bambu_studio_plate_and_arrange_are_passed_through() {
        let inputs = ["input/a.stl".to_owned(), "input/b.stl".to_owned()];
        let plan = Plan {
            job_dir: Path::new("/j"),
            inputs: &inputs,
            filament_slots: 1,
            filament_map: None,
            plate: 3,
            arrange: true,
            bed_type: None,
            mode: Mode::Slice,
        };
        let invocation = bambu_studio(&plan);
        let args = strings(&invocation);
        let after = |flag: &str| args[args.iter().position(|a| a == flag).unwrap() + 1].clone();
        assert_eq!(after("--slice"), "3");
        assert_eq!(after("--arrange"), "1");
        assert_eq!(after("--export-3mf"), "plate_3.gcode.3mf");
        assert_eq!(after("--load-filaments"), "/j/profiles/filament_1.json");
        assert!(!args.iter().any(|a| a.contains("filament-map")));
        assert!(!args.iter().any(|a| a.contains("curr-bed-type")));
        assert_eq!(args[args.len() - 2..], ["/j/input/a.stl", "/j/input/b.stl"]);
        // Neither form was in a run of the real program, and both are named as such.
        assert_eq!(
            invocation.unconfirmed.len(),
            2,
            "{:?}",
            invocation.unconfirmed
        );
        assert!(invocation.unconfirmed[0].contains("--arrange 1"));
        assert!(invocation.unconfirmed[1].contains("--slice 3"));
        // Whether the slicer may move the parts is stated either way, never left to it.
        for arrange in [true, false] {
            let args = strings(&bambu_studio(&Plan {
                arrange,
                ..plan.clone()
            }));
            let at = args.iter().position(|a| a == "--arrange").unwrap();
            assert_eq!(args[at + 1], if arrange { "1" } else { "0" });
        }
    }

    /// Runs A and C of 17 November 2026: the project export is the slice without `--slice`;
    /// the proof run is the project alone with no settings option at all.
    #[test]
    fn the_project_runs_are_the_measured_ones() {
        let inputs = ["input/plate.3mf".to_owned()];
        let export = strings(&bambu_studio(&Plan {
            job_dir: Path::new("/data/job"),
            inputs: &inputs,
            filament_slots: 1,
            filament_map: None,
            plate: 1,
            arrange: false,
            bed_type: None,
            mode: Mode::ExportProject,
        }));
        assert_eq!(
            export,
            [
                "--debug",
                "3",
                "--load-settings",
                "/data/job/profiles/machine.json;/data/job/profiles/process.json",
                "--load-filaments",
                "/data/job/profiles/filament_1.json",
                "--arrange",
                "0",
                "--outputdir",
                "/data/job/output",
                "--export-3mf",
                "project.3mf",
                "/data/job/input/plate.3mf",
            ]
        );
        let project = ["input/project.3mf".to_owned()];
        let reslice = bambu_studio(&Plan {
            job_dir: Path::new("/data/job/proof"),
            inputs: &project,
            filament_slots: 1,
            filament_map: None,
            plate: 1,
            arrange: false,
            bed_type: Some(textured()),
            mode: Mode::ResliceProject,
        });
        assert_eq!(
            strings(&reslice),
            [
                "--debug",
                "3",
                "--slice",
                "0",
                "--arrange",
                "0",
                "--outputdir",
                "/data/job/proof/output",
                "--export-3mf",
                "sliced.gcode.3mf",
                "/data/job/proof/input/project.3mf",
            ]
        );
        assert_eq!(reslice.output_candidates, ["output/sliced.gcode.3mf"]);
    }

    #[test]
    fn prusa_slicer_command_line_matches_its_help_text() {
        let inputs = ["input/model.step".to_owned()];
        let plan = Plan {
            job_dir: Path::new("/data/job"),
            inputs: &inputs,
            filament_slots: 1,
            filament_map: None,
            plate: 1,
            arrange: true,
            bed_type: None,
            mode: Mode::Slice,
        };
        let invocation = prusa_slicer(&plan);
        assert_eq!(
            strings(&invocation),
            [
                "--export-gcode",
                "--datadir",
                "/data/job/slicer-config",
                "--loglevel",
                "2",
                "--load",
                "profiles/machine.ini",
                "--load",
                "profiles/process.ini",
                "--load",
                "profiles/filament_1.ini",
                "--output",
                "/data/job/output/plate_1.gcode",
                "input/model.step",
            ]
        );
        assert_eq!(invocation.output_candidates, ["output/plate_1.gcode"]);
        let fixed = prusa_slicer(&Plan {
            arrange: false,
            ..plan
        });
        assert!(strings(&fixed).contains(&"--dont-arrange".to_owned()));
    }
}
