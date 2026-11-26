//! Helpers shared by the integration tests: the sandbox launcher, the fake slicer, model
//! fixtures and throwaway job folders.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

mod plans;
#[allow(unused_imports)]
pub use plans::*;

use hotend_slicer::{Extent, FilamentAssignment, SliceJob, Slicer, SlicerConfig, SlicerKind};
// Only the Unix-only `Script` stand-in uses these.
#[cfg(unix)]
use hotend_slicer::{Found, Installation, Verdict};

/// Builds the `hotend-sandbox` launcher when it is not yet next to the test binaries:
/// `cargo test -p hotend-slicer` does not build another package's binary by itself.
pub fn ensure_launcher() {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        if std::env::var_os("HOTEND_SANDBOX_BIN").is_some() {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let profile_dir = exe.parent().and_then(Path::parent).unwrap();
        let name = format!("hotend-sandbox{}", std::env::consts::EXE_SUFFIX);
        if profile_dir.join(&name).is_file() {
            return;
        }
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
        let mut build = std::process::Command::new(cargo);
        build
            .args(["build", "-p", "hotend-sandbox", "--manifest-path"])
            .arg(manifest);
        if !cfg!(debug_assertions) {
            build.arg("--release");
        }
        assert!(build.status().expect("cannot run cargo").success());
    });
}

/// Whether sandboxed tests can run here. Without a usable sandbox they prove nothing, so
/// that fails the test; only with `HOTEND_TEST_ALLOW_SKIP=1` it is skipped instead
/// (returns `false` after printing why).
pub fn sandbox_ready(test: &str) -> bool {
    ensure_launcher();
    let problem = match hotend_sandbox::probe() {
        Ok(report) if report.usable => return true,
        Ok(report) => format!("the sandbox is not usable here: {:?}", report.reason),
        Err(error) => error.to_string(),
    };
    if std::env::var_os("HOTEND_TEST_ALLOW_SKIP").is_some_and(|v| v == "1") {
        println!("SKIPPED {test}: {problem}");
        return false;
    }
    panic!("{test} cannot run: {problem} (set HOTEND_TEST_ALLOW_SKIP=1 to skip instead)");
}

/// The fake slicer built from this crate.
pub fn fake_slicer() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hotend-fake-slicer"))
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A job folder with the named fixtures copied into `input/`.
pub struct JobDir {
    _tmp: tempfile::TempDir,
    pub path: PathBuf,
}

impl JobDir {
    pub fn new(fixtures: &[&str]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(tmp.path()).unwrap().join("job");
        std::fs::create_dir_all(path.join("input")).unwrap();
        for name in fixtures {
            std::fs::copy(fixture(name), path.join("input").join(name)).unwrap();
        }
        Self { _tmp: tmp, path }
    }

    /// The folder that holds the job folder; outside the sandbox's reach.
    pub fn outside(&self) -> &Path {
        self.path.parent().unwrap()
    }
}

/// A slicer that uses the fake program for `kinds`, as an explicit setting.
pub fn slicer_with_fake(kinds: &[SlicerKind]) -> Slicer {
    let mut config = SlicerConfig::new().unwrap();
    config.executables = kinds
        .iter()
        .map(|kind| (*kind, fake_slicer()))
        .collect::<HashMap<_, _>>();
    Slicer::new(config).unwrap()
}

pub fn filament(slot: u32, profile: &str, family: &str, nozzle_id: i64) -> FilamentAssignment {
    FilamentAssignment {
        slot,
        filament_profile: profile.into(),
        family: family.into(),
        nozzle_id,
    }
}

/// The 20 x 30 x 10 mm box of the fixtures, printed by `slot`.
pub fn box_extent(slot: u32) -> Extent {
    Extent {
        name: "box".into(),
        slot,
        min: [0.0, 0.0, 0.0],
        max: [20.0, 30.0, 10.0],
        placed: false,
    }
}

pub fn x2d_job(dir: &JobDir, input: &str, filaments: Vec<FilamentAssignment>) -> SliceJob {
    SliceJob {
        job_dir: dir.path.clone(),
        inputs: vec![PathBuf::from("input").join(input)],
        printer: "bambu-x2d".into(),
        process_profile: "demo-0.20mm".into(),
        extents: filaments.iter().map(|f| box_extent(f.slot)).collect(),
        filaments,
        plate: 1,
        arrange: true,
        bed_type: None,
        print_settings: None,
        nozzle_diameter_mm: None,
        plate_previews: None,
    }
}

pub fn prusa_job(dir: &JobDir, input: &str) -> SliceJob {
    SliceJob {
        job_dir: dir.path.clone(),
        inputs: vec![PathBuf::from("input").join(input)],
        printer: "prusa-mk4s".into(),
        process_profile: "demo-0.20mm".into(),
        filaments: vec![filament(1, "pla-basic", "PLA", 0)],
        plate: 1,
        arrange: true,
        bed_type: None,
        extents: vec![box_extent(1)],
        print_settings: None,
        nozzle_diameter_mm: None,
        plate_previews: None,
    }
}

/// A shell script in its own folder, standing in for a misbehaving slicer.
#[cfg(unix)]
pub struct Script {
    _tmp: tempfile::TempDir,
    pub installation: Installation,
}

#[cfg(unix)]
impl Script {
    pub fn new(kind: SlicerKind, body: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let path = dir.join("slicer.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _tmp: tmp,
            installation: Installation {
                kind,
                path,
                found: Found::Override,
                version: None,
                fake: false,
                read_only: vec![dir],
                verdict: Verdict::Allow,
                note: None,
            },
        }
    }
}

/// A `result.json` in the shape of a successful run: plate 1, one filament.
pub const RESULT_OK: &str = r#"{"error_string": "Success.", "return_code": 0, "plate_index": 0, "sliced_plates": [{"id": 1, "total_predication": 600, "main_predication": 400, "filaments": [{"id": 1, "filament_id": "TST01", "total_used_g": 2.5, "main_used_g": 2.5}], "warning_message": ""}]}"#;

/// The plate metadata that goes with [`real_shaped_gcode`].
const SLICE_INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<config>
  <plate>
    <metadata key="index" value="1"/>
    <metadata key="prediction" value="600"/>
    <metadata key="filament_maps" value="1"/>
    <filament id="1" type="PLA" used_m="0.83" used_g="2.50"/>
  </plate>
</config>
"#;

/// The settings block of a file sliced for the textured plate at 55 C.
pub const TEXTURED_55: &str = "; curr_bed_type = Textured PEI Plate\n\
    ; textured_plate_temp = 55\n; textured_plate_temp_initial_layer = 55\n";

/// Moves that print the outline of the fixtures' 20 x 30 mm box where it lies.
pub const BOX_KEPT: &str = "G1 X0.2 Y0.2 Z0.2 F3000\nG1 X19.8 Y0.2 E1\nG1 X19.8 Y29.8 E1\n\
    G1 X0.2 Y29.8 E1\nG1 X0.2 Y0.2 E1\n";

/// G-code in the form a real Bambu Studio writes, made up for these tests: a header, a
/// settings block with the lines of `config`, a start routine that heats the bed to
/// `bed_c`, the moves of `body` between the two markers, and an end routine. Only the
/// markers, the comment forms and the command words are those of the real program.
pub fn real_shaped_gcode(version: &str, config: &str, bed_c: u32, body: &str) -> String {
    format!(
        "; HEADER_BLOCK_START\n; BambuStudio {version}\n\
         ; model printing time: 5m 0s; total estimated time: 10m 0s\n\
         ; total layer number: 50\n\
         ; total filament length [mm] : 830.00\n\
         ; total filament weight [g] : 2.50\n\
         ; filament_density: 1.24\n\
         ; max_z_height: 10.00\n\
         ; filament: 1\n\
         ; HEADER_BLOCK_END\n\n\
         ; CONFIG_BLOCK_START\n{config}; CONFIG_BLOCK_END\n\n\
         M140 S{bed_c}\nM190 S{bed_c}\nM104 S220\nG28\nG90\nM83\n\
         ; MACHINE_START_GCODE_END\n{body}; MACHINE_END_GCODE_START\nM140 S0\n"
    )
}

/// A zip archive with these entries.
pub fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, content) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// A `.gcode.3mf` for plate 1 that holds `gcode`, the plate metadata of one filament and
/// the `extra` entries.
pub fn sliced_archive(gcode: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
    let mut entries: Vec<(&str, &[u8])> = vec![
        ("Metadata/plate_1.gcode", gcode.as_bytes()),
        ("Metadata/slice_info.config", SLICE_INFO.as_bytes()),
    ];
    entries.extend_from_slice(extra);
    zip_of(&entries)
}

/// A program that prints nothing, reports success and leaves `gcode` as its sliced file:
/// a real Bambu Studio as far as Hotend can tell, whatever it was asked to do.
#[cfg(unix)]
pub fn replay(gcode: &str) -> Script {
    replay_archive(&sliced_archive(gcode, &[]))
}

/// [`replay`] for a sliced file given as bytes.
#[cfg(unix)]
pub fn replay_archive(archive: &[u8]) -> Script {
    let script = Script::new(
        SlicerKind::BambuStudio,
        &format!(
            "cp \"$(dirname \"$0\")/sliced.3mf\" output/plate_1.gcode.3mf\n\
             cat > output/result.json <<'END'\n{RESULT_OK}\nEND"
        ),
    );
    let folder = script.installation.path.parent().unwrap();
    std::fs::write(folder.join("sliced.3mf"), archive).unwrap();
    script
}

/// The fixtures' box as a job for the main nozzle, to be sliced where it lies.
pub fn placed_box_job(dir: &JobDir) -> SliceJob {
    let mut job = x2d_job(
        dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );
    job.arrange = false;
    job.extents[0].placed = true;
    job
}

pub fn codes(warnings: &[hotend_slicer::SliceWarning]) -> Vec<&str> {
    warnings.iter().map(|w| w.code.as_str()).collect()
}

/// SHA-256 of a file, computed independently of the crate under test.
pub fn sha256(path: &Path) -> String {
    hotend_core::import::sha256_file(path).unwrap()
}

/// A version folder as the core lays it out: a prepared plate (the 20 x 30 x 10 mm box of
/// the fixtures as one placed piece) and an empty slice output folder.
pub struct Version {
    _tmp: tempfile::TempDir,
    pub dir: PathBuf,
    pub output_dir: PathBuf,
    /// The far corner of the piece on the plate, as the preparation reports it.
    piece_max: [f64; 3],
}

impl Version {
    pub fn new(piece_max: [f64; 3]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path())
            .unwrap()
            .join("versions")
            .join("1");
        let output_dir = dir.join("slice").join("0123456789abcdef");
        std::fs::create_dir_all(&output_dir).unwrap();
        let plate_dir = dir.join("prepare/plates/1");
        std::fs::create_dir_all(&plate_dir).unwrap();
        std::fs::copy(fixture("box_20x30x10.3mf"), plate_dir.join("plate.3mf")).unwrap();
        Self {
            _tmp: tmp,
            dir,
            output_dir,
            piece_max,
        }
    }

    /// A request for the printer's main nozzle (device extruder 1 on the X2D, 0 elsewhere).
    pub fn request(
        &self,
        printer: &str,
        family: &str,
        material: &str,
    ) -> hotend_core::SliceRequest {
        hotend_core::SliceRequest {
            project_id: "0190f3a0-0000-7000-8000-000000000001".into(),
            version: 1,
            plate_file: self.dir.join("prepare/plates/1/plate.3mf"),
            pieces: vec![hotend_core::PlacedPiece {
                name: "box".into(),
                nozzle_id: if printer == "bambu-x2d" { 1 } else { 0 },
                min: [0.0, 0.0, 0.0],
                max: self.piece_max,
            }],
            output_dir: self.output_dir.clone(),
            profile: "demo-0.20mm".into(),
            plate: 1,
            printer_id: printer.into(),
            material: hotend_core::engine::MaterialContext {
                id: material.into(),
                family: family.into(),
                density_g_cm3: Some(1.24),
                max_overhang_deg: None,
            },
            plate_type: None,
            slicer_executables: Default::default(),
            print_settings: None,
            nozzle_diameter_mm: 0.4,
            plate_previews: None,
        }
    }
}

/// A small tree of system profiles in the layout of an installed Bambu Studio
/// (`<root>/BBL/{machine,process,filament}/*.json`). The file names are the ones Hotend's
/// table selects for the X2D; every setting in them is made up for these tests. The leaf
/// profiles hold almost nothing themselves: what the stand-in slicer needs (the density,
/// the temperatures, the bed size) is in their parents and templates, so a slice only
/// works when the tree was flattened.
pub struct SystemTree {
    _tmp: tempfile::TempDir,
    /// The folder that holds the vendor folders (`resources/profiles`).
    pub root: PathBuf,
}

impl SystemTree {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap().join("profiles");
        let tree = Self { _tmp: tmp, root };
        tree.put(
            "machine",
            "fdm_machine_common",
            r#"{"type": "machine", "name": "fdm_machine_common", "from": "system",
                "instantiation": "false", "printable_height": "200",
                "nozzle_diameter": ["0.4"], "test_only_number": 7}"#,
        );
        tree.put(
            "machine",
            "Test bed template",
            r#"{"type": "template", "name": "Test bed template",
                "printable_area": ["0x0", "256x0", "256x256", "0x256"]}"#,
        );
        tree.put(
            "machine",
            "Bambu Lab X2D 0.4 nozzle",
            r#"{"type": "machine", "name": "Bambu Lab X2D 0.4 nozzle", "from": "system",
                "instantiation": "true", "inherits": "fdm_machine_common",
                "include": ["Test bed template"], "printable_height": "260",
                "nozzle_diameter": ["0.4", "0.4"]}"#,
        );
        tree.put(
            "process",
            "fdm_process_common",
            r#"{"type": "process", "name": "fdm_process_common", "from": "system",
                "instantiation": "false", "layer_height": "0.3", "wall_loops": "2",
                "curr_bed_type": "Cool Plate"}"#,
        );
        for (name, layer) in [
            ("0.20mm Standard @BBL X2D", "0.2"),
            ("0.12mm Fine @BBL X2D", "0.12"),
        ] {
            tree.put(
                "process",
                name,
                &format!(
                    r#"{{"type": "process", "name": "{name}", "from": "system",
                        "instantiation": "true", "inherits": "fdm_process_common",
                        "layer_height": "{layer}",
                        "compatible_printers": ["Bambu Lab X2D 0.4 nozzle"]}}"#
                ),
            );
        }
        tree.put(
            "filament",
            "fdm_filament_common",
            r#"{"type": "filament", "name": "fdm_filament_common", "from": "system",
                "instantiation": "false", "filament_diameter": ["1.75"],
                "cool_plate_temp": ["35"], "textured_plate_temp": ["45"],
                "hot_plate_temp": ["45"], "eng_plate_temp": ["45"]}"#,
        );
        for (base, kind, density, nozzle, textured, id) in [
            ("Bambu PLA Basic", "PLA", "1.24", "220", "55", "TST01"),
            ("Bambu PETG Basic", "PETG", "1.27", "250", "70", "TST02"),
            ("Bambu TPU 95A", "TPU", "1.21", "230", "40", "TST03"),
        ] {
            tree.put(
                "filament",
                &format!("{base} @base"),
                &format!(
                    r#"{{"type": "filament", "name": "{base} @base", "from": "system",
                        "instantiation": "false", "inherits": "fdm_filament_common",
                        "filament_id": "{id}", "filament_type": ["{kind}"],
                        "filament_density": ["{density}"],
                        "nozzle_temperature": ["{nozzle}"],
                        "textured_plate_temp": ["{textured}"]}}"#
                ),
            );
            tree.put(
                "filament",
                &format!("{base} @BBL X2D 0.4 nozzle"),
                &format!(
                    r#"{{"type": "filament", "name": "{base} @BBL X2D 0.4 nozzle",
                        "from": "system", "instantiation": "true",
                        "inherits": "{base} @base",
                        "compatible_printers": ["Bambu Lab X2D 0.4 nozzle"]}}"#
                ),
            );
        }
        tree
    }

    /// Writes (or replaces) one profile of the vendor `BBL`.
    pub fn put(&self, role: &str, name: &str, json: &str) {
        let dir = self.root.join("BBL").join(role);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.json")), json).unwrap();
    }

    /// The configuration of [`SystemTree::slicer`].
    pub fn config(&self) -> SlicerConfig {
        let mut config = SlicerConfig::new().unwrap();
        config
            .executables
            .insert(SlicerKind::BambuStudio, fake_slicer());
        config.system_profiles = Some(self.root.clone());
        // The plate on the printer, as the application's settings would state it.
        config.default_bed_type = Some("textured-pei".into());
        config
    }

    /// A slicer that gives the stand-in program the flattened profiles of this tree, the
    /// way a real Bambu Studio is given those of its installation.
    pub fn slicer(&self) -> Slicer {
        Slicer::new(self.config()).unwrap()
    }
}

/// A flattened profile of a job, as written into its `profiles/` folder.
pub fn job_profile(dir: &JobDir, name: &str) -> serde_json::Value {
    let bytes = std::fs::read(dir.path.join("profiles").join(name)).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
