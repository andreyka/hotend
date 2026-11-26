//! End-to-end tests of the `hotend` binary: the offline demo, a scripted chat, `doctor`,
//! the modelling commands and the way to a print, each as a real process on a throwaway
//! data directory.
//!
//! Everything below the command line is real: the sandbox launcher, the Python engine and
//! agent, the tool gateway and the store. Nothing here uses a network, a provider key, a
//! real slicer or a printer: the slicer is the stand-in and the printer the simulated one.
//! Every run gets provider-key-shaped canaries in its environment, and the tests assert
//! that none of them shows up in any output or file.
//!
//! A print is confirmed at a terminal only. The tests that confirm one give the process a
//! pseudo-terminal with `script` (util-linux) and type the answer into it.
//!
//! Like the other integration tests these need `python/.venv` and a usable sandbox; without
//! them they fail, or skip when `HOTEND_TEST_ALLOW_SKIP=1`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::Value;

/// Values no test may ever see again: they are put in the environment of every `hotend`
/// process the way a user's real keys would be.
const CANARIES: [(&str, &str); 6] = [
    ("ANTHROPIC_API_KEY", "sk-ant-canary-9f3b7c1d5e2a4f6089ab"),
    ("OPENAI_API_KEY", "sk-proj-canary-70e1d2c3b4a5968778ff"),
    ("GEMINI_API_KEY", "AIzaCanary-1b2c3d4e5f60718293a4b5"),
    ("OPENROUTER_API_KEY", "sk-or-canary-5a4b3c2d1e0f99887766"),
    // The search services' keys: made up, in no real key's shape.
    ("KEENABLE_API_KEY", "keenable-canary-4d3c2b1a0f9e8d7c6b5a"),
    ("TAVILY_API_KEY", "tavily-canary-0a1b2c3d4e5f60718293"),
];
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
const DEMO_TOOL_CALLS: [&str; 7] = [
    "list_cards",
    "write_model_code",
    "write_model_code",
    "render_views",
    "render_views",
    "slice",
    "propose_print",
];

/// What the agent says in each turn of `builtin:cable_box_demo`, read from the script.
fn shipped_demo_turns_said() -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../python/hotend_agent/scripts/cable_box_demo.json");
    let script: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    script["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|turn| turn["say"].as_str().unwrap().trim().to_owned())
        .collect()
}

fn hotend_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hotend"))
}

/// Builds the sandbox launcher and the stand-in slicer when they are not yet next to the
/// `hotend` binary: `cargo test -p hotend-cli` does not build other packages' binaries.
fn ensure_tools() {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let dir = hotend_exe().parent().unwrap().to_path_buf();
        let present = ["hotend-sandbox", "hotend-fake-slicer"].iter().all(|name| {
            dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
                .is_file()
        });
        if present {
            return;
        }
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
        let mut build = Command::new(cargo);
        build
            .args([
                "build",
                "-p",
                "hotend-sandbox",
                "-p",
                "hotend-slicer",
                "--bins",
            ])
            .arg("--manifest-path")
            .arg(manifest);
        if !cfg!(debug_assertions) {
            build.arg("--release");
        }
        assert!(build.status().expect("cannot run cargo").success());
    });
}

/// A `hotend` command with a clean, hostile-to-leaks environment: the canaries are set,
/// and nothing of the developer's own Hotend configuration is inherited.
fn hotend(args: &[&str]) -> Command {
    ensure_tools();
    let mut command = Command::new(hotend_exe());
    command.args(args);
    for name in [
        "HOTEND_DATA_DIR",
        "HOTEND_SECRETS",
        "HOTEND_UNSAFE_NO_SANDBOX",
        "HOTEND_AGENT_SCRIPT",
        "HOTEND_BAMBU_STUDIO",
        "HOTEND_PRUSA_SLICER",
        "HOTEND_STAND_IN_OPENER",
    ] {
        command.env_remove(name);
    }
    for (name, value) in CANARIES {
        command.env(name, value);
    }
    // The hand-off path in its local form, also when the tests run inside WSL: a WSL
    // hand-off needs a folder on a Windows drive, which no test writes to. The WSL form is
    // tested in hotend-printer and hotend-core.
    command.env("HOTEND_HANDOFF_HOST", "linux");
    command
}

/// The end of the question `hotend` writes before it reads an answer.
const QUESTION: &str = "anything else to leave the job pending:";
/// The end of the question of `hotend research on`.
const RESEARCH_QUESTION: &str = "anything else to leave web research off:";

/// When what is typed reaches the terminal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Typed {
    /// After the question is on the terminal, as a person answers it.
    AfterTheQuestion,
    /// Before the command starts: it is held back until the terminal has echoed what was
    /// typed, so the text is in the terminal's input queue before `hotend` runs at all
    /// (and so before it shows its review and question), however `script` schedules the
    /// forwarding of its input.
    Ahead,
}

/// The same command with a pseudo-terminal as its standard input and output, and `typed`
/// typed into it once the command has asked its question. `script` runs it; its output
/// (stdout and stderr together) is the terminal's.
fn at_a_terminal(args: &[&str], typed: &str) -> Ran {
    on_a_terminal(args, typed, Typed::AfterTheQuestion, "")
}

/// `shell` is appended to the command line `script` runs (a redirection).
fn on_a_terminal(args: &[&str], typed: &str, when: Typed, shell: &str) -> Ran {
    use std::io::{Read, Write};
    let quoted: Vec<String> = std::iter::once(hotend_exe().display().to_string())
        .chain(args.iter().map(|arg| (*arg).to_owned()))
        .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
        .collect();
    // `hotend` builds the clean environment; only the program changes.
    let template = hotend(&[]);
    // Typed ahead: the shell `script` runs waits for the gate file before it starts
    // `hotend`, and the gate is opened only once the typed text was echoed (below).
    let gate_dir = tempfile::tempdir().unwrap();
    let gate = gate_dir.path().join("typed");
    let wait = match when {
        Typed::Ahead => format!(
            "while [ ! -e '{}' ]; do sleep 0.01; done; ",
            gate.display().to_string().replace('\'', "'\\''")
        ),
        Typed::AfterTheQuestion => String::new(),
    };
    let mut command = Command::new("script");
    command.args([
        "-qec",
        &format!("{wait}{}{shell}", quoted.join(" ")),
        "/dev/null",
    ]);
    for (name, value) in template.get_envs() {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .expect("cannot run `script` (util-linux), which gives the test a terminal");
    let mut keyboard = child.stdin.take().unwrap();
    // The terminal's output, read as it comes: the question is the moment to answer, and
    // the echo of what was typed ahead the moment to let `hotend` start.
    let mut screen = child.stdout.take().unwrap();
    let (asked, question) = std::sync::mpsc::channel::<()>();
    let (echoed_tx, echoed) = std::sync::mpsc::channel::<()>();
    let echo = match when {
        // All of it, as the terminal echoes it (a new line comes back as CR LF).
        Typed::Ahead => typed.replace('\n', "\r\n"),
        Typed::AfterTheQuestion => String::new(),
    };
    assert!(
        when != Typed::Ahead || !echo.is_empty(),
        "typed ahead needs text to type"
    );
    let reader = std::thread::spawn(move || {
        let (mut seen, mut chunk) = (Vec::new(), [0_u8; 4096]);
        let (mut told, mut told_echo) = (false, echo.is_empty());
        while let Ok(read) = screen.read(&mut chunk) {
            if read == 0 {
                break;
            }
            seen.extend_from_slice(&chunk[..read]);
            let shown = String::from_utf8_lossy(&seen);
            if !told_echo && shown.contains(&echo) {
                told_echo = true;
                let _ = echoed_tx.send(());
            }
            if !told && (shown.contains(QUESTION) || shown.contains(RESEARCH_QUESTION)) {
                told = true;
                let _ = asked.send(());
            }
        }
        seen
    });
    if when == Typed::Ahead {
        keyboard.write_all(typed.as_bytes()).unwrap();
        keyboard.flush().unwrap();
        // The terminal echoes what it received: from then on the text is waiting in its
        // input queue, and only then may `hotend` start.
        if echoed
            .recv_timeout(std::time::Duration::from_secs(60))
            .is_err()
        {
            let _ = child.kill();
            panic!("the terminal did not echo what was typed ahead");
        }
        std::fs::write(&gate, b"").unwrap();
    }
    match question.recv_timeout(std::time::Duration::from_secs(900)) {
        Ok(()) if when == Typed::AfterTheQuestion => {
            // A command that has already ended reads nothing; that is not this helper's
            // failure.
            let _ = keyboard.write_all(typed.as_bytes());
        }
        // Typed ahead, or the command ended without asking: nothing more is typed.
        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            panic!(
                "`hotend {}` neither asked nor ended in 15 minutes",
                args.join(" ")
            );
        }
    }
    // The end of the input: a command still waiting for an answer reads an empty one.
    drop(keyboard);
    let output = child.wait_with_output().unwrap();
    let ran = Ran {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&reader.join().unwrap()).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    assert_no_secret("terminal output", ran.stdout.as_bytes());
    ran
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(mut command: Command) -> Ran {
    let Output {
        status,
        stdout,
        stderr,
    } = command.output().expect("cannot run hotend");
    let ran = Ran {
        code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    };
    assert_no_secret("stdout", ran.stdout.as_bytes());
    assert_no_secret("stderr", ran.stderr.as_bytes());
    ran
}

fn ok(command: Command) -> Ran {
    let ran = run(command);
    assert_eq!(
        ran.code, 0,
        "stdout:\n{}\nstderr:\n{}",
        ran.stdout, ran.stderr
    );
    ran
}

/// Whether the machine can run sandboxed Python at all, as `hotend doctor` sees it.
/// Without that these tests prove nothing, so they fail; with `HOTEND_TEST_ALLOW_SKIP=1`
/// they are skipped instead.
fn ready(test: &str) -> bool {
    static PROBLEM: OnceLock<Option<String>> = OnceLock::new();
    let problem = PROBLEM.get_or_init(|| {
        let ran = run(hotend(&["doctor", "--json"]));
        match serde_json::from_str::<Value>(&ran.stdout) {
            Ok(report) if report["ok"] == true => None,
            Ok(report) => Some(format!(
                "hotend doctor reports missing requirements: {}",
                report["checks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|check| check["status"] == "fail")
                    .map(|check| format!("{}: {}", check["id"], check["detail"]))
                    .collect::<Vec<_>>()
                    .join("; ")
            )),
            Err(error) => Some(format!("hotend doctor --json printed no JSON ({error})")),
        }
    });
    let Some(problem) = problem else {
        return true;
    };
    if std::env::var_os("HOTEND_TEST_ALLOW_SKIP").is_some_and(|v| v == "1") {
        println!("SKIPPED {test}: {problem}");
        return false;
    }
    panic!("{test} cannot run: {problem} (set HOTEND_TEST_ALLOW_SKIP=1 to skip instead)");
}

fn is_key_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

/// What looks like a credential in `bytes`: a canary, a provider-style key (`sk-` and 20 or
/// more key characters), a bearer token or a credential header.
fn secret_like(bytes: &[u8]) -> Option<String> {
    let contains = |needle: &[u8]| bytes.windows(needle.len()).any(|window| window == needle);
    for (name, value) in CANARIES {
        if contains(value.as_bytes()) {
            return Some(format!("the value of {name}"));
        }
    }
    let lower = bytes.to_ascii_lowercase();
    // The search services' keys start with Keenable's prefix (kept reversed here, so that no
    // file of the build holds the prefix next to other key-like bytes, which a secret
    // scanner would count) and `tvly-`.
    let keenable: Vec<u8> = b"_neek".iter().rev().copied().collect();
    for (prefix, least) in [
        (&b"sk-"[..], 20),
        (b"bearer ", 16),
        (keenable.as_slice(), 16),
        (b"tvly-", 16),
    ] {
        let mut from = 0;
        while let Some(at) = lower[from..]
            .windows(prefix.len())
            .position(|window| window == prefix)
        {
            let start = from + at + prefix.len();
            let run = bytes[start..]
                .iter()
                .take_while(|b| is_key_char(**b))
                .count();
            // `desk-...` or `task-...` is a word, not a key prefix.
            let word_before = (from + at)
                .checked_sub(1)
                .is_some_and(|before| bytes[before].is_ascii_alphanumeric());
            if run >= least && !word_before {
                let shown = String::from_utf8_lossy(&bytes[from + at..start + 6]).into_owned();
                return Some(format!("a credential-shaped string starting `{shown}`"));
            }
            from = start;
        }
    }
    for header in ["x-api-key:", "x-goog-api-key:", "authorization:"] {
        if lower
            .windows(header.len())
            .any(|window| window == header.as_bytes())
        {
            return Some(format!("the credential header `{header}`"));
        }
    }
    None
}

fn assert_no_secret(what: &str, bytes: &[u8]) {
    if let Some(found) = secret_like(bytes) {
        panic!("{what} contains {found}");
    }
}

/// Every regular file below `dir`.
fn files_below(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                found.push(entry.path());
            }
        }
    }
    found
}

fn assert_no_secret_in_files(dir: &Path) -> usize {
    let files = files_below(dir);
    for path in &files {
        assert_no_secret(&path.display().to_string(), &std::fs::read(path).unwrap());
    }
    files.len()
}

fn json_file(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap())
        .unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

/// Width and height of a PNG, after checking its signature and header chunk.
fn png_size(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).unwrap();
    assert!(
        bytes.starts_with(PNG_MAGIC),
        "{} is not a PNG",
        path.display()
    );
    assert_eq!(&bytes[12..16], b"IHDR", "{}", path.display());
    let number = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (number(16), number(20))
}

/// Decodes one buffer of mesh.json (PROTOCOL.md section 3.4) into its 4-byte items.
fn buffer(value: &Value) -> Vec<[u8; 4]> {
    assert_eq!(value["codec"], "b64", "{value}");
    let bytes = BASE64.decode(value["buffer"].as_str().unwrap()).unwrap();
    let declared = value["shape"][0].as_u64().unwrap() as usize;
    assert_eq!(
        bytes.len(),
        declared * 4,
        "the buffer does not have its declared length"
    );
    bytes
        .chunks_exact(4)
        .map(|c| c.try_into().unwrap())
        .collect()
}

#[test]
fn the_secret_scanner_finds_what_it_should_and_nothing_else() {
    assert!(secret_like(b"key = sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA").is_some());
    assert!(secret_like(b"Authorization: Bearer abcdefghijklmnop1234").is_some());
    assert!(secret_like(b"{\"x-api-key\": 1} x-api-key: abc").is_some());
    assert!(secret_like(CANARIES[2].1.as_bytes()).is_some());
    for harmless in [
        &b"the task-list sk-1 desk-organizer-with-many-compartments bearer of news"[..],
        b"provider-key.anthropic is the keychain entry; ANTHROPIC_API_KEY is the variable",
        b"args_sha256 088938e0b38e8dba609f8c5484b3dc1f0abe3ef82f254b650f0678e3aa0fda4a",
    ] {
        assert_eq!(secret_like(harmless), None);
    }
}

#[test]
fn the_demo_runs_end_to_end_and_leaves_valid_artefacts() {
    if !ready("the_demo_runs_end_to_end_and_leaves_valid_artefacts") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    let untouched = tmp.path().join("users-own-data");
    let mut command = hotend(&["demo", "--out", out.to_str().unwrap()]);
    // The demo must keep to its own data directory, whatever the user has configured.
    command.env("HOTEND_DATA_DIR", &untouched);
    let ran = ok(command);
    assert!(
        !untouched.exists(),
        "the demo wrote into the user's data directory"
    );
    let out = out.canonicalize().unwrap();

    // The transcript: every stage of the loop, in order, and what was simulated.
    let mut at = 0;
    for stage in [
        "Simulated:",
        "] Describe",
        "] Contract",
        "tool list_cards",
        "] Code",
        "-> status: check_error",
        "FAIL min_wall [box]",
        "] Fix",
        "-> status: ok",
        // The example meets its own inside size (a guess, so only a warning, but met).
        "] Checks: version 2 passes",
        "checks: 0 fail",
        "] Views",
        "-> views of version 2: iso (1 image shown to the agent)",
        "] Mesh",
        "] Prepare and slice",
        "-> status: ok (version 2, profile demo-0.20mm, plate 1 of 1)",
        "prepared plate (real):",
        "print time:",
        "orientation on the plate:",
        "- lid: upside down.",
        "every piece fits a plate",
        "warning fake_slicer",
        "] Print job",
        "status: pending",
        "] Summary",
        "] G-code check",
        "SIMULATED: nothing",
        "G-code check: passed",
        "] Print: SIMULATED printer",
        "Not run: the job stays pending",
        "Artefacts of version 2",
        "0 model requests sent",
    ] {
        match ran.stdout[at..].find(stage) {
            Some(found) => at += found + stage.len(),
            None => panic!("`{stage}` is missing or out of order in:\n{}", ran.stdout),
        }
    }
    assert!(ran.stdout.contains("not printable"), "{}", ran.stdout);
    assert!(
        ran.stdout.contains(&out.display().to_string()),
        "{}",
        ran.stdout
    );

    // model.py: the fixed version of the script the agent wrote.
    let model = std::fs::read_to_string(out.join("model.py")).unwrap();
    assert!(
        model.contains("PARAMS = {") && model.contains("def build(p, ctx):"),
        "{model}"
    );
    assert!(model.contains("snap_lid"), "{model}");

    // STEP: an ISO 10303-21 file with header and data sections, closed properly.
    let step = std::fs::read_to_string(out.join("model.step")).unwrap();
    assert!(
        step.starts_with("ISO-10303-21;\nHEADER;"),
        "{}",
        &step[..80]
    );
    assert!(step.contains("FILE_SCHEMA") && step.contains("\nDATA;"));
    assert!(step.trim_end().ends_with("END-ISO-10303-21;"));
    assert!(
        step.contains("MANIFOLD_SOLID_BREP"),
        "the STEP file holds no solid"
    );

    // mesh.json: two parts with real, self-consistent triangle data.
    let mesh = json_file(&out.join("mesh.json"));
    let parts: Vec<&String> = mesh["hotend"]["parts"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(parts, ["box", "lid"]);
    let instances = mesh["instances"].as_array().unwrap();
    assert_eq!(instances.len(), 2);
    for instance in instances {
        let vertices = buffer(&instance["vertices"]).len() / 3;
        let triangles = buffer(&instance["triangles"]);
        assert!(
            triangles.len() >= 36 && triangles.len().is_multiple_of(3),
            "{}",
            triangles.len()
        );
        let highest = triangles
            .iter()
            .map(|i| i32::from_le_bytes(*i))
            .max()
            .unwrap();
        assert!(
            (highest as usize) < vertices,
            "a triangle points past the vertices"
        );
        let finite = buffer(&instance["vertices"])
            .iter()
            .all(|v| f32::from_le_bytes(*v).is_finite());
        assert!(finite);
    }

    // Views: four 768 x 768 PNG images.
    for view in ["iso", "front", "top", "right"] {
        let path = out.join("views").join(format!("{view}.png"));
        assert_eq!(png_size(&path), (768, 768), "{}", path.display());
        assert!(
            std::fs::metadata(&path).unwrap().len() > 2000,
            "{view} is nearly empty"
        );
    }

    // The slice result: time, grams and layers, labelled as simulated, bound to the file.
    let slice = json_file(&out.join("slice-result.json"));
    assert_eq!(slice["simulated"], true);
    let result = &slice["result"];
    assert_eq!(result["status"], "ok");
    assert!(result["time_s"].as_f64().unwrap() > 60.0, "{result}");
    assert!(result["layers"].as_u64().unwrap() > 10, "{result}");
    let filament = result["filament"].as_array().unwrap();
    assert_eq!(filament.len(), 1);
    assert!(filament[0]["grams"].as_f64().unwrap() > 1.0, "{result}");
    assert!(filament[0]["metres"].as_f64().unwrap() > 0.1, "{result}");
    assert_eq!(
        filament[0]["nozzle_id"], 1,
        "PLA goes to the main nozzle (device id 1)"
    );
    let warnings: Vec<&str> = result["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert_eq!(
        warnings,
        ["fake_slicer", "demo_profile", "unverified_printer_facts"]
    );
    let sliced = PathBuf::from(slice["sliced_file"].as_str().unwrap());
    assert!(sliced.starts_with(out.join("data")), "{}", sliced.display());
    let sha256 = hotend_core::import::sha256_file(&sliced).unwrap();
    assert_eq!(result["sha256"], sha256.as_str());

    // The plate the slicer read: both parts placed, the lid turned over, nothing left out.
    let preparation = &slice["preparation"];
    assert_eq!(preparation["printer_id"], "bambu-x2d");
    assert_eq!(preparation["plates"].as_array().unwrap().len(), 1);
    assert_eq!(preparation["unplaced"].as_array().unwrap().len(), 0);
    let plate = out
        .join("data/projects")
        .join(slice["project_id"].as_str().unwrap())
        .join("versions/2/prepare/plates/1/plate.3mf");
    assert!(
        std::fs::read(&plate).unwrap().starts_with(b"PK\x03\x04"),
        "the plate is a 3MF archive"
    );

    // The pending print job: a record bound to that file, and nothing more. Without
    // `--simulate-print` and a typed yes it was not confirmed, sent or run.
    let pending = slice["print_jobs"].as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["status"], "pending");
    assert_eq!(pending[0]["delivery"], Value::Null);
    assert_eq!(pending[0]["slice_sha256"], sha256.as_str());
    assert_eq!(pending[0]["version"], 2);
    assert!(!out.join("simulated-printer-log.json").exists());

    // The store itself, read independently of what the demo printed.
    let store = hotend_core::store::Store::open(&out.join("data/hotend.db")).unwrap();
    let project_id = slice["project_id"].as_str().unwrap();
    let mut audit = store.list_audit(Some(project_id), 1000).unwrap();
    audit.reverse();
    let tools: Vec<&hotend_core::AuditEntry> =
        audit.iter().filter(|entry| entry.kind == "tool").collect();
    let names: Vec<&str> = tools.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, DEMO_TOOL_CALLS);
    for entry in &tools {
        assert_eq!(entry.outcome, "ok", "{entry:?}");
        let hash = entry.args_sha256.as_deref().unwrap();
        assert!(
            hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "{entry:?}"
        );
        assert!(entry.run_id.is_some(), "{entry:?}");
    }
    let count = |kind: &str| audit.iter().filter(|entry| entry.kind == kind).count();
    assert_eq!(
        count("egress"),
        0,
        "the scripted agent must not make a model request"
    );
    assert_eq!(count("version"), 2);
    assert_eq!(count("prepare"), 1);
    assert_eq!(count("print"), 0, "no print step without a confirmation");
    // Each slice tool call sliced once, and each slice wrote one entry of its own.
    let slices: Vec<_> = audit.iter().filter(|entry| entry.kind == "slice").collect();
    assert_eq!(
        slices.len(),
        DEMO_TOOL_CALLS
            .iter()
            .filter(|name| **name == "slice")
            .count()
    );
    for entry in &slices {
        assert_eq!(
            (entry.name.as_str(), entry.outcome.as_str()),
            ("slice", "ok")
        );
        assert_eq!(entry.detail["project_id"], project_id);
        assert_eq!(entry.detail["sha256"], sha256.as_str());
        assert_eq!(entry.detail["previews_added_by_hotend"], false);
    }
    assert!(count("job") >= 6, "{} engine jobs", count("job"));
    let agent: Vec<_> = audit.iter().filter(|entry| entry.kind == "agent").collect();
    assert_eq!(agent.len(), 1);
    assert_eq!(agent[0].outcome, "done");
    assert_eq!(agent[0].detail["llm_requests"], 0);
    let versions = store.list_versions(project_id).unwrap();
    let statuses: Vec<_> = versions.iter().map(|v| (v.number, v.status)).collect();
    assert_eq!(
        statuses,
        [
            (1, hotend_core::VersionStatus::CheckError),
            (2, hotend_core::VersionStatus::Ok)
        ]
    );
    assert_eq!(versions[0].params["wall"], 1.0);
    assert_eq!(versions[1].params["wall"], 2.0);
    assert_eq!(store.list_pending_prints(project_id).unwrap().len(), 1);
    drop(store);

    // The audit log file is the same record.
    let logged = json_file(&out.join("audit-log.json"));
    let logged: Vec<&str> = logged
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "tool")
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(logged, DEMO_TOOL_CALLS);

    // Nothing that looks like a credential anywhere: artefacts, store, job leftovers.
    let scanned = assert_no_secret_in_files(&out);
    assert!(scanned >= 20, "only {scanned} files were scanned");

    // A second run into the same folder is refused instead of mixing two runs.
    let again = run(hotend(&["demo", "--out", out.to_str().unwrap()]));
    assert_eq!(again.code, 2, "{}", again.stderr);
    assert!(
        again.stderr.contains("already holds a demo run"),
        "{}",
        again.stderr
    );
}

#[test]
fn the_demo_runs_the_job_on_the_simulated_printer_only_after_yes_at_a_terminal() {
    if !ready("the_demo_runs_the_job_on_the_simulated_printer_only_after_yes_at_a_terminal") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();

    // Without a terminal the flag is refused before anything runs: nobody could be asked.
    let refused_out = tmp.path().join("refused");
    let mut refused = hotend(&[
        "demo",
        "--simulate-print",
        "--out",
        refused_out.to_str().unwrap(),
    ]);
    // Nothing in the environment stands in for the person.
    for name in [
        "HOTEND_CONFIRM",
        "HOTEND_YES",
        "HOTEND_SIMULATE_PRINT",
        "CI",
    ] {
        refused.env(name, "yes");
    }
    refused.stdin(std::process::Stdio::null());
    let refused = run(refused);
    assert_eq!(refused.code, 2, "{}", refused.stdout);
    assert!(
        refused
            .stderr
            .contains("confirmed by a person at a terminal"),
        "{}",
        refused.stderr
    );
    assert!(!refused_out.exists(), "the demo started anyway");

    // At a terminal, with `yes` typed: the job runs on the simulated printer to its end.
    let out = tmp.path().join("out");
    let ran = at_a_terminal(
        &["demo", "--simulate-print", "--out", out.to_str().unwrap()],
        "yes\n",
    );
    assert_eq!(ran.code, 0, "{}", ran.stdout);
    let mut at = 0;
    for stage in [
        "] G-code check",
        "G-code check: passed",
        "] Print: SIMULATED printer",
        "Run this job on the SIMULATED printer? Nothing will be printed.",
        ": starting (simulated_printer) [simulated printer]",
        "[simulated] print job",
        ": printing (simulated_printer)",
        ": finished (simulated_printer)",
        "nothing was printed",
        "No object exists",
        "simulated-printer-log.json",
    ] {
        match ran.stdout[at..].find(stage) {
            Some(found) => at += found + stage.len(),
            None => panic!("`{stage}` is missing or out of order in:\n{}", ran.stdout),
        }
    }
    let out = out.canonicalize().unwrap();
    let slice = json_file(&out.join("slice-result.json"));
    let jobs = slice["print_jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["status"], "finished");
    assert_eq!(jobs[0]["delivery"], "simulated_printer");
    assert_eq!(jobs[0]["detail"]["simulated"], true);
    // What the simulated printer received: one start command, for the file that was
    // reviewed.
    let log = std::fs::read_to_string(out.join("simulated-printer-log.json")).unwrap();
    assert_eq!(log.matches("project_file").count(), 1, "{log}");
    let store = hotend_core::store::Store::open(&out.join("data/hotend.db")).unwrap();
    let mut audit = store
        .list_audit(Some(slice["project_id"].as_str().unwrap()), 1000)
        .unwrap();
    audit.reverse();
    let steps: Vec<&str> = audit
        .iter()
        .filter(|entry| entry.kind == "print")
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(steps, ["starting", "printing", "finished"]);
    // The agent's part of the run did not grow: its last tool call is the proposal.
    let tools: Vec<&str> = audit
        .iter()
        .filter(|entry| entry.kind == "tool")
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(tools, DEMO_TOOL_CALLS);
    drop(store);

    // At a terminal, with anything but `yes`: the job stays pending and nothing is sent.
    let declined_out = tmp.path().join("declined");
    let declined = at_a_terminal(
        &[
            "demo",
            "--simulate-print",
            "--out",
            declined_out.to_str().unwrap(),
        ],
        "y\n",
    );
    assert_eq!(declined.code, 0, "{}", declined.stdout);
    assert!(
        declined
            .stdout
            .contains("Not confirmed: the job stays pending"),
        "{}",
        declined.stdout
    );
    let slice = json_file(&declined_out.join("slice-result.json"));
    assert_eq!(slice["print_jobs"][0]["status"], "pending");
    assert!(!declined_out.join("simulated-printer-log.json").exists());
    assert_no_secret_in_files(tmp.path());
}

#[test]
fn chat_streams_the_agents_text_and_tool_activity() {
    if !ready("chat_streams_the_agents_text_and_tool_activity") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().to_str().unwrap();
    // The real agent process in scripted mode, stopped by the turn budget after the two
    // model versions: list the cards, write the thin-walled version, write the fix.
    let ran = ok(hotend(&[
        "--data-dir",
        data,
        "chat",
        "Cable box",
        "A box that hides a power strip on my desk, with a lid that snaps on.",
        "--script",
        "builtin:cable_box_demo",
        "--max-turns",
        "3",
    ]));
    // What the agent says is the shipped script's to word: each of its first three turns
    // is printed whole, between the tool lines.
    let said = shipped_demo_turns_said();
    let mut at = 0;
    for piece in [
        said[0].as_str(),
        "  [tool list_cards]",
        said[1].as_str(),
        "  [tool write_model_code: First version",
        "  [-> status: check_error]",
        said[2].as_str(),
        "  [-> status: ok]",
    ] {
        match ran.stdout[at..].find(piece) {
            Some(found) => at += found + piece.len(),
            None => panic!("`{piece}` is missing or out of order in:\n{}", ran.stdout),
        }
    }
    assert!(ran.stderr.contains("SCRIPTED RUN"), "{}", ran.stderr);
    assert!(ran.stderr.contains("created project"), "{}", ran.stderr);
    assert!(
        ran.stderr.contains("3 model requests, 3 tool calls"),
        "{}",
        ran.stderr
    );
    assert!(
        ran.stderr.contains("2 new version(s); current version 2"),
        "{}",
        ran.stderr
    );
    assert!(ran.stderr.contains("used all its turns"), "{}", ran.stderr);

    let shown = ok(hotend(&[
        "--data-dir",
        data,
        "project",
        "show",
        "Cable box",
        "--json",
    ]));
    let detail: Value = serde_json::from_str(&shown.stdout).unwrap();
    let statuses: Vec<&str> = detail["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["check_error", "ok"]);
    assert_eq!(detail["transcript"][0]["role"], "user");
    assert_eq!(detail["transcript"][1]["role"], "assistant");

    // The scripted run's placeholder provider is not left behind as the user's choice,
    // and a real chat without a provider or a key says what to do instead of starting.
    let unset = run(hotend(&["--data-dir", data, "chat", "Cable box", "Thanks"]));
    assert_eq!(unset.code, 2, "{}", unset.stderr);
    assert!(unset.stderr.contains("--provider"), "{}", unset.stderr);
    let mut no_key = hotend(&[
        "--data-dir",
        data,
        "chat",
        "Cable box",
        "Thanks",
        "--provider",
        "openai",
        "--model",
        "some-model",
        "--secrets",
        "env",
    ]);
    no_key.env_remove("OPENAI_API_KEY");
    let no_key = run(no_key);
    assert_eq!(no_key.code, 2, "{}", no_key.stderr);
    assert!(
        no_key.stderr.contains("set OPENAI_API_KEY"),
        "{}",
        no_key.stderr
    );
    assert_no_secret_in_files(tmp.path());
}

#[test]
fn doctor_reports_the_machine_and_fails_on_a_missing_requirement() {
    if !ready("doctor_reports_the_machine_and_fails_on_a_missing_requirement") {
        return;
    }
    let ran = ok(hotend(&["doctor", "--json"]));
    let report: Value = serde_json::from_str(&ran.stdout).unwrap();
    assert_eq!(report["ok"], true);
    let checks = report["checks"].as_array().unwrap();
    let check = |id: &str| {
        checks
            .iter()
            .find(|check| check["id"] == id)
            .unwrap_or_else(|| panic!("no check `{id}` in {report}"))
    };
    for id in ["sandbox", "python", "engine", "agent"] {
        assert_eq!(check(id)["status"], "ok", "{}", check(id));
        assert_eq!(check(id)["required"], true);
    }
    // The agent self-test ran the real agent process: one tool call, no model request.
    let agent = check("agent")["detail"].as_str().unwrap();
    assert!(
        agent.contains("1 tool call") && agent.contains("no model was asked"),
        "{agent}"
    );
    for guarantee in [
        "network",
        "filesystem",
        "privileges",
        "resources",
        "environment",
    ] {
        assert_eq!(check(&format!("sandbox.{guarantee}"))["status"], "ok");
    }
    // The read-only rule is tried for real, not only listed.
    let read_only = check("sandbox.read_only");
    assert_eq!(read_only["status"], "ok", "{read_only}");
    assert_eq!(read_only["required"], true);
    assert!(
        checks.iter().all(|check| check["id"] != "sandbox.launcher"),
        "no launcher override is set"
    );
    let engine = check("engine")["detail"].as_str().unwrap();
    assert!(
        engine.contains("volume 6000 mm3") && engine.contains("build123d"),
        "{engine}"
    );
    assert_eq!(report["sandbox"]["usable"], true);
    // Every slicer Hotend knows is listed with the printers it serves, found or not.
    let slicers = report["slicers"].as_array().unwrap();
    let serves = |kind: &str| {
        slicers
            .iter()
            .find(|slicer| slicer["kind"] == kind)
            .unwrap_or_else(|| panic!("no slicer `{kind}` in {report}"))["printers"]
            .clone()
    };
    assert_eq!(serves("bambu-studio"), serde_json::json!(["bambu-x2d"]));
    assert_eq!(serves("prusa-slicer"), serde_json::json!(["prusa-mk4s"]));
    assert_eq!(check("slicer.fake")["status"], "info");
    let printer = check("printer")["detail"].as_str().unwrap();
    assert!(printer.contains("simulated printer only"), "{printer}");
    // The canaries are in the environment: the report says which providers have a key
    // there, never the key (the scan in `run` covers the values).
    let env = check("secrets.env")["detail"].as_str().unwrap();
    assert!(
        env.contains("anthropic, openai, google, openrouter"),
        "{env}"
    );
    assert!(checks.iter().any(|check| check["id"] == "secrets.keyring"));

    // The table says the same.
    let table = ok(hotend(&["doctor"]));
    assert!(
        table.stdout.contains("All hard requirements are met"),
        "{}",
        table.stdout
    );
    assert!(
        table.stdout.contains("engine self-test") && table.stdout.contains("agent self-test"),
        "{}",
        table.stdout
    );

    // A substituted launcher is reported: honoured by a debug build, ignored by a release
    // build.
    let launcher = hotend_exe().with_file_name("hotend-sandbox");
    let mut overridden = hotend(&["doctor", "--json"]);
    overridden.env("HOTEND_SANDBOX_BIN", &launcher);
    let overridden: Value = serde_json::from_str(&ok(overridden).stdout).unwrap();
    let launcher_check = overridden["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "sandbox.launcher")
        .expect("the launcher override must be reported");
    let expected = if cfg!(debug_assertions) {
        "warn"
    } else {
        "info"
    };
    assert_eq!(launcher_check["status"], expected, "{launcher_check}");

    // A missing Python environment is a hard failure that names the fix.
    let mut broken = hotend(&["doctor"]);
    broken.env("HOTEND_PYTHON", "/nonexistent/python");
    let broken = run(broken);
    assert_eq!(broken.code, 1, "{}", broken.stdout);
    assert!(
        broken.stdout.contains("1 hard requirement(s) missing"),
        "{}",
        broken.stdout
    );
    assert!(
        broken.stdout.contains("uv sync --frozen"),
        "{}",
        broken.stdout
    );
    assert!(broken.stdout.contains("HOTEND_PYTHON"), "{}", broken.stdout);
    let mut broken = hotend(&["doctor", "--json"]);
    broken.env("HOTEND_PYTHON", "/nonexistent/python");
    let broken = run(broken);
    assert_eq!(broken.code, 1);
    let report: Value = serde_json::from_str(&broken.stdout).unwrap();
    assert_eq!(report["ok"], false);
    let status = |id: &str| {
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["id"] == id)
            .map(|check| check["status"].clone())
    };
    assert_eq!(status("python"), Some(Value::from("fail")));
    assert_eq!(status("engine"), Some(Value::from("skipped")));
    assert_eq!(status("agent"), Some(Value::from("skipped")));
}

/// `hotend snapshot`: name the state at any time, keep changing the model, export the named
/// state's files, go back to it and continue from there without losing the newer versions.
#[test]
fn snapshots_are_named_listed_exported_and_gone_back_to() {
    if !ready("snapshots_are_named_listed_exported_and_gone_back_to") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let data = data.to_str().unwrap();
    let files = tmp.path().join("files");
    std::fs::create_dir(&files).unwrap();
    let files_arg = files.to_str().unwrap();
    let model = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python/tests/models/box.py");
    let at = |args: &[&str]| {
        let mut all = vec!["--data-dir", data];
        all.extend_from_slice(args);
        hotend(&all)
    };
    ok(at(&["build", model.to_str().unwrap()]));
    let none = ok(at(&["snapshot", "list"]));
    assert!(none.stdout.contains("no snapshots yet"), "{}", none.stdout);

    // Name the current version, change the model, name that too.
    let created = ok(at(&[
        "snapshot",
        "create",
        "first try",
        "--note",
        "plain box",
    ]));
    assert!(
        created
            .stdout
            .contains("snapshot \"first try\" names version 1"),
        "{}",
        created.stdout
    );
    ok(at(&["param", "set", "width", "70"]));
    let wide = json_of(at(&["snapshot", "create", "narrower", "--json"]));
    assert_eq!(wide["version"], 2);
    let older = json_of(at(&[
        "snapshot",
        "create",
        "again 1",
        "--version",
        "1",
        "--json",
    ]));
    assert_eq!(older["version"], 1);
    let taken = run(at(&["snapshot", "create", "FIRST TRY"]));
    assert_eq!(taken.code, 2, "{}", taken.stderr);
    assert!(
        taken.stderr.contains("already has a snapshot"),
        "{}",
        taken.stderr
    );
    let bad = run(at(&["snapshot", "create", "two\nlines"]));
    assert_eq!(bad.code, 2, "{}", bad.stderr);
    let listed = json_of(at(&["snapshot", "list", "--json"]));
    let named: Vec<(&str, u64)> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["name"].as_str().unwrap(), s["version"].as_u64().unwrap()))
        .collect();
    assert_eq!(named, [("first try", 1), ("narrower", 2), ("again 1", 1)]);
    let table = ok(at(&["snapshot", "list"]));
    assert!(table.stdout.contains("narrower"), "{}", table.stdout);

    // Export: the snapshot's own folder with STEP, 3MF and the manifest.
    let missing = run(at(&[
        "snapshot",
        "export",
        "first try",
        "--to",
        "/no/such/folder",
    ]));
    assert_eq!(missing.code, 2, "{}", missing.stderr);
    let export = json_of(at(&[
        "snapshot",
        "export",
        "first try",
        "--to",
        files_arg,
        "--json",
    ]));
    let folder = PathBuf::from(export["folder"].as_str().unwrap());
    assert!(folder.starts_with(std::fs::canonicalize(&files).unwrap()));
    let mut written: Vec<String> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    written.sort_unstable();
    assert_eq!(written, ["model.3mf", "model.step", "snapshot.json"]);
    assert_eq!(export["printed"], Value::Null);
    let manifest = json_file(&folder.join("snapshot.json"));
    assert_eq!(manifest["snapshot"]["name"], "first try");
    assert_eq!(manifest["snapshot"]["version"], 1);
    for file in manifest["files"].as_array().unwrap() {
        let path = folder.join(file["name"].as_str().unwrap());
        assert_eq!(
            hotend_core::import::sha256_file(&path).unwrap(),
            file["sha256"].as_str().unwrap()
        );
    }
    let shown = ok(at(&["snapshot", "export", "narrower", "--to", files_arg]));
    assert!(
        shown.stdout.contains("(version 2) written to"),
        "{}",
        shown.stdout
    );
    assert!(shown.stdout.contains("model.step"), "{}", shown.stdout);
    // Exporting again into the same folder is refused; the first export stays as it was.
    let manifest_bytes = std::fs::read(folder.join("snapshot.json")).unwrap();
    let again = run(at(&["snapshot", "export", "first try", "--to", files_arg]));
    assert_eq!(again.code, 2, "{}", again.stderr);
    assert!(
        again.stderr.contains("already exists") && again.stderr.contains("Choose another"),
        "{}",
        again.stderr
    );
    assert_eq!(
        std::fs::read(folder.join("snapshot.json")).unwrap(),
        manifest_bytes
    );

    // Go back to the first snapshot, change it: version 3 continues from version 1, and
    // version 2 is still there, reachable by its snapshot.
    let restored = ok(at(&["snapshot", "restore", "first try"]));
    assert!(
        restored
            .stdout
            .contains("version 1 (snapshot \"first try\") is the current version"),
        "{}",
        restored.stdout
    );
    ok(at(&["param", "set", "width", "50"]));
    let versions = json_of(at(&["versions", "--json"]));
    let lineage: Vec<(u64, Value)> = versions
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v["number"].as_u64().unwrap(), v["parent"].clone()))
        .collect();
    assert_eq!(
        lineage,
        [(1, Value::Null), (2, Value::from(1)), (3, Value::from(1))]
    );
    let table = ok(at(&["versions"]));
    assert!(table.stdout.contains("from"), "{}", table.stdout);
    ok(at(&["snapshot", "restore", "narrower"]));
    let current = json_of(at(&["project", "show", "--json"]));
    assert_eq!(current["project"]["current_version"], 2, "{current}");

    // Deleting a name keeps its version.
    let deleted = ok(at(&["snapshot", "delete", "narrower"]));
    assert!(
        deleted.stdout.contains("version 2 and its files stay"),
        "{}",
        deleted.stdout
    );
    assert_eq!(
        json_of(at(&["versions", "--json"]))
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let gone = run(at(&["snapshot", "restore", "narrower"]));
    assert_eq!(gone.code, 1, "{}", gone.stderr);

    // Every step is on the audit log.
    let audit = json_of(at(&["audit", "--limit", "200", "--json"]));
    let mut actions: Vec<&str> = audit
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .filter(|entry| entry["kind"] == "snapshot")
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    actions.dedup();
    assert_eq!(actions, ["create", "export", "restore", "delete"]);
}

#[test]
fn a_model_file_is_built_checked_exported_rendered_and_sliced() {
    if !ready("a_model_file_is_built_checked_exported_rendered_and_sliced") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let data = data.to_str().unwrap();
    let files = tmp.path().join("files");
    std::fs::create_dir(&files).unwrap();
    let files_arg = files.to_str().unwrap();
    let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python/tests/models");
    let model = |name: &str| models.join(name).to_str().unwrap().to_owned();

    // Nothing to act on yet.
    let none = run(hotend(&["--data-dir", data, "checks"]));
    assert_eq!(none.code, 2, "{}", none.stderr);
    assert!(
        none.stderr.contains("hotend project new"),
        "{}",
        none.stderr
    );

    // build: a version with the parameter applied, in a project named after the file.
    let built = ok(hotend(&[
        "--data-dir",
        data,
        "build",
        &model("box.py"),
        "--param",
        "width=90",
    ]));
    assert!(built.stdout.contains("created project"), "{}", built.stdout);
    assert!(
        built.stdout.contains("status: ok\nversion: 1 created"),
        "{}",
        built.stdout
    );
    assert!(built.stdout.contains("width=90 mm"), "{}", built.stdout);
    assert!(
        built.stdout.contains("- part: 90 x 60 x 40 mm"),
        "{}",
        built.stdout
    );

    // A model that raises: no version, the line number, exit code 1.
    let failed = run(hotend(&[
        "--data-dir",
        data,
        "build",
        &model("broken_exception.py"),
        "--project",
        "box",
    ]));
    assert_eq!(failed.code, 1, "{}", failed.stdout);
    assert!(
        failed.stdout.contains("status: model_error"),
        "{}",
        failed.stdout
    );
    assert!(
        failed.stdout.contains("the current version is still 1"),
        "{}",
        failed.stdout
    );
    assert!(failed.stdout.contains("at line 9"), "{}", failed.stdout);
    // An out-of-range parameter is refused by the model, not clamped.
    let refused = run(hotend(&[
        "--data-dir",
        data,
        "build",
        &model("box.py"),
        "--param",
        "width=9000",
    ]));
    assert_eq!(refused.code, 1, "{}", refused.stdout);
    assert!(
        refused.stdout.contains("ParameterError"),
        "{}",
        refused.stdout
    );
    let bad = run(hotend(&[
        "--data-dir",
        data,
        "build",
        &model("box.py"),
        "--param",
        "width",
    ]));
    assert_eq!(bad.code, 2, "{}", bad.stderr);

    // checks
    let checks = ok(hotend(&[
        "--data-dir",
        data,
        "checks",
        "--only",
        "min_wall,valid_solid",
    ]));
    assert!(
        checks.stdout.contains("status: ok (version 1)"),
        "{}",
        checks.stdout
    );
    assert!(
        checks.stdout.contains("- pass: valid_solid, min_wall"),
        "{}",
        checks.stdout
    );
    let unknown = run(hotend(&[
        "--data-dir",
        data,
        "checks",
        "--only",
        "nonsense",
    ]));
    assert_eq!(unknown.code, 2, "{}", unknown.stderr);

    // export: all three formats, each a real file of its kind.
    let exported = ok(hotend(&[
        "--data-dir",
        data,
        "export",
        "--format",
        "step",
        "--format",
        "3mf",
        "--format",
        "stl",
        "--out",
        files_arg,
    ]));
    let step = std::fs::read_to_string(files.join("model.step")).unwrap();
    assert!(step.starts_with("ISO-10303-21;"), "{}", &step[..40]);
    assert!(step.trim_end().ends_with("END-ISO-10303-21;"));
    let threemf = std::fs::read(files.join("model.3mf")).unwrap();
    assert!(
        threemf.starts_with(b"PK\x03\x04"),
        "a 3MF file is a zip archive"
    );
    let stl = std::fs::read(files.join("part.stl")).unwrap();
    assert!(stl.len() > 84, "{} bytes of STL", stl.len());
    let sha256 = hotend_core::import::sha256_file(&files.join("model.step")).unwrap();
    assert!(exported.stdout.contains(&sha256), "{}", exported.stdout);
    let missing = run(hotend(&[
        "--data-dir",
        data,
        "export",
        "--format",
        "step",
        "--out",
        "/nonexistent",
    ]));
    assert_eq!(missing.code, 2, "{}", missing.stderr);

    // views: plain and cut open.
    let views = ok(hotend(&[
        "--data-dir",
        data,
        "views",
        "--view",
        "iso",
        "--view",
        "top",
        "--size",
        "256",
        "--out",
        files_arg,
    ]));
    assert_eq!(views.stdout.lines().count(), 2, "{}", views.stdout);
    assert_eq!(png_size(&files.join("iso.png")), (256, 256));
    assert_eq!(png_size(&files.join("top.png")), (256, 256));
    ok(hotend(&[
        "--data-dir",
        data,
        "views",
        "--view",
        "front",
        "--section",
        "XZ@1",
        "--size",
        "256",
        "--out",
        files_arg,
    ]));
    assert_eq!(png_size(&files.join("front_section_xz.png")), (256, 256));

    // slice: without an installed Bambu Studio the real path refuses and names the fix;
    // the stand-in slices and says what it is.
    let real = run(hotend(&["--data-dir", data, "slice"]));
    assert_eq!(real.code, 1, "{}", real.stderr);
    assert!(
        real.stderr.contains("slicer_not_installed"),
        "{}",
        real.stderr
    );
    assert!(
        real.stderr.contains("HOTEND_BAMBU_STUDIO"),
        "{}",
        real.stderr
    );
    let fake = ok(hotend(&["--data-dir", data, "slice", "--fake"]));
    assert!(
        fake.stdout
            .contains("status: ok (version 1, profile demo-0.20mm, plate 1 of 1)"),
        "{}",
        fake.stdout
    );
    assert!(
        fake.stdout.contains("print time:") && fake.stdout.contains(" g, "),
        "{}",
        fake.stdout
    );
    assert!(
        fake.stdout.contains("warning fake_slicer"),
        "{}",
        fake.stdout
    );
    assert!(fake.stdout.contains("not printable"), "{}", fake.stdout);
    let sliced = fake
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("sliced file: "))
        .expect("no sliced file line");
    assert!(Path::new(sliced).is_file(), "{sliced}");

    // project list and show.
    let list = ok(hotend(&["--data-dir", data, "project", "list", "--json"]));
    let list: Value = serde_json::from_str(&list.stdout).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["name"], "box");
    assert_eq!(list[0]["current_version"], 1);
    let show = ok(hotend(&["--data-dir", data, "project", "show"]));
    assert!(
        show.stdout
            .lines()
            .any(|line| line.starts_with("1 *") && line.split_whitespace().nth(3) == Some("ok")),
        "{}",
        show.stdout
    );

    assert_no_secret_in_files(tmp.path());
}

/// Runs a command that must succeed and reads its standard output as JSON.
fn json_of(command: Command) -> Value {
    let ran = ok(command);
    serde_json::from_str(&ran.stdout)
        .unwrap_or_else(|error| panic!("not JSON ({error}):\n{}", ran.stdout))
}

#[test]
fn a_version_is_changed_measured_prepared_and_printed_only_after_yes_at_a_terminal() {
    if !ready("a_version_is_changed_measured_prepared_and_printed_only_after_yes_at_a_terminal") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let data = data.to_str().unwrap();
    let files = tmp.path().join("files");
    std::fs::create_dir(&files).unwrap();
    let files_arg = files.to_str().unwrap();
    let model =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python/tests/models/cable_box.py");
    let at = |args: &[&str]| {
        let mut all = vec!["--data-dir", data];
        all.extend_from_slice(args);
        hotend(&all)
    };
    ok(at(&["build", model.to_str().unwrap()]));

    // param set: a new version with one value changed; a value outside its range is not.
    let changed = ok(at(&["param", "set", "width", "130"]));
    assert!(
        changed
            .stdout
            .contains("version: 2 created and now current"),
        "{}",
        changed.stdout
    );
    let refused = run(at(&["param", "set", "width", "9000"]));
    assert_eq!(refused.code, 1, "{}", refused.stdout);
    let unknown = run(at(&["param", "set", "nonsense", "1"]));
    assert_ne!(unknown.code, 0, "{}", unknown.stdout);

    // versions and revert: the list, then an earlier version current again.
    let versions = json_of(at(&["versions", "--json"]));
    let versions = versions.as_array().unwrap();
    assert_eq!(versions.len(), 2, "a refused change makes no version");
    assert_eq!(versions[1]["origin"], "set_parameter");
    assert_eq!(versions[1]["parent"], 1);
    assert_eq!(versions[1]["params"]["width"], 130.0);
    let reverted = ok(at(&["revert", "1"]));
    assert!(
        reverted.stdout.contains("version 1 is the current version"),
        "{}",
        reverted.stdout
    );
    let table = ok(at(&["versions"]));
    assert!(table.stdout.contains("1 *"), "{}", table.stdout);
    let missing = run(at(&["revert", "9"]));
    assert_eq!(missing.code, 1, "{}", missing.stderr);
    ok(at(&["revert", "2"]));

    // measure: the engine's answer for the current version.
    let measured = json_of(at(&[
        "measure",
        "--query",
        r#"{"kind": "bbox", "part": "lid"}"#,
        "--json",
    ]));
    assert_eq!(measured[0]["ok"], true, "{measured}");
    assert!(
        measured[0]["size"][0].as_f64().unwrap() > 130.0,
        "{measured}"
    );
    let bad = run(at(&["measure", "--query", "not json"]));
    assert_eq!(bad.code, 2, "{}", bad.stderr);

    // questions: what the contract still rests on a guess for.
    let questions = json_of(at(&["questions", "--json"]));
    let requirements: Vec<&str> = questions
        .as_array()
        .unwrap()
        .iter()
        .map(|question| question["requirement"].as_str().unwrap())
        .collect();
    assert!(
        requirements.contains(&"holds_power_strip"),
        "{requirements:?}"
    );
    let asked = ok(at(&["questions"]));
    assert!(
        asked.stdout.contains("[holds_power_strip]"),
        "{}",
        asked.stdout
    );

    // prepare: orientation and plates, with two lids.
    let prepared = json_of(at(&["prepare", "--copies", "lid=2", "--json"]));
    assert_eq!(prepared["version"], 2);
    assert_eq!(prepared["printer_id"], "bambu-x2d");
    let pieces: Vec<&str> = prepared["plates"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    assert_eq!(pieces, ["box", "lid.1", "lid.2"]);
    let lid = prepared["orientations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|orientation| orientation["part"] == "lid")
        .unwrap();
    assert_eq!(lid["copies"], 2);
    assert_eq!(lid["turned"], "upside down");
    let bad = run(at(&["prepare", "--copies", "lid=two"]));
    assert_eq!(bad.code, 2, "{}", bad.stderr);

    // slice: the stand-in slices that plate.
    let sliced = json_of(at(&["slice", "--fake", "--json"]));
    assert_eq!(
        sliced["record"]["job"]["pieces"],
        serde_json::json!(["box", "lid.1", "lid.2"])
    );
    let sha256 = sliced["summary"]["sha256"].as_str().unwrap().to_owned();

    // A version laid out again is not the one that was sliced: no job for the old file.
    ok(at(&["prepare"]));
    let stale = run(at(&["print", "propose"]));
    assert_eq!(stale.code, 1, "{}", stale.stdout);
    assert!(stale.stderr.contains("slice it first"), "{}", stale.stderr);
    let sliced = json_of(at(&["slice", "--fake", "--json"]));
    let one_lid = sliced["summary"]["sha256"].as_str().unwrap().to_owned();
    assert_ne!(one_lid, sha256);

    // printers: the simulated printer and the ways this build can deliver. Direct control
    // of a real printer is not built, so it is not listed.
    let printers = json_of(at(&["printers", "--json"]));
    assert_eq!(printers.as_array().unwrap().len(), 1);
    assert_eq!(printers[0]["simulated"], true);
    let enabled: Vec<(&str, bool)> = printers[0]["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["kind"].as_str().unwrap(), d["enabled"].as_bool().unwrap()))
        .collect();
    assert_eq!(
        enabled,
        [
            ("simulated_printer", true),
            ("bambu_connect", true),
            ("export_to_folder", true)
        ]
    );
    // The simulated printer speaks for itself, not with a real printer's sentences.
    let simulated = &printers[0]["deliveries"][0];
    assert_eq!(simulated["name"], "Simulated printer");
    let said = format!("{} {}", simulated["can"], simulated["cannot"]).to_lowercase();
    assert!(said.contains("simulated inside hotend"), "{said}");
    assert!(
        said.contains("nothing is heated, moved or printed"),
        "{said}"
    );
    for real in ["bambu cloud", "usb", "pause"] {
        assert!(
            !said.contains(real),
            "`{real}` on the simulated printer: {said}"
        );
    }
    let listed = ok(at(&["printers"]));
    assert!(listed.stdout.contains("SIMULATED"), "{}", listed.stdout);
    assert!(
        !listed.stdout.contains("Developer Mode") && !listed.stdout.contains("not enabled"),
        "{}",
        listed.stdout
    );

    // No build plate was chosen in the settings when the version was sliced, so the sliced
    // file cannot be checked for the plate on the printer: the review says so and names the
    // setting. Nothing is assumed, and what the file says of itself does not count.
    assert_eq!(sliced["record"]["job"]["plate_type"], Value::Null);
    let job = json_of(at(&["print", "propose", "--json"]));
    let unchosen = job["id"].as_str().unwrap().to_owned();
    let refused = run(at(&["print", "review", &unchosen]));
    assert_eq!(refused.code, 1, "{}", refused.stdout);
    assert!(
        refused.stderr.contains("`plate_type`: cool, textured-pei"),
        "{}",
        refused.stderr
    );
    ok(at(&["print", "cancel", &unchosen]));
    let unknown = run(at(&["settings", "set", "--plate-type", "Glass Plate"]));
    assert_ne!(unknown.code, 0, "{}", unknown.stdout);
    let set = json_of(at(&[
        "settings",
        "set",
        "--plate-type",
        "textured-pei",
        "--json",
    ]));
    assert_eq!(set["plate_type"], "textured-pei");
    let sliced = json_of(at(&["slice", "--fake", "--json"]));
    assert_eq!(sliced["record"]["job"]["plate_type"], "textured-pei");
    let nozzles = sliced["record"]["job"]["nozzles"].as_array().unwrap();
    assert_eq!(nozzles.len(), 2, "{nozzles:?}");
    assert!(
        nozzles.iter().all(|nozzle| nozzle["diameter_mm"] == 0.4),
        "{nozzles:?}"
    );
    let one_lid = sliced["summary"]["sha256"].as_str().unwrap().to_owned();

    // print propose, review, status: all headless, none delivers anything.
    let job = json_of(at(&["print", "propose", "--note", "first", "--json"]));
    assert_eq!(job["status"], "pending");
    let job_id = job["id"].as_str().unwrap().to_owned();
    let review = json_of(at(&["print", "review", &job_id, "--json"]));
    assert_eq!(review["simulated"], true);
    assert_eq!(review["summary"]["sha256"], one_lid.as_str());
    assert_eq!(review["summary"]["plate"], 1);
    assert_eq!(review["summary"]["filaments"][0]["material"], "PLA");
    assert_eq!(review["spools"][0]["slot"], 1);
    let shown = ok(at(&["print", "review", &job_id]));
    assert!(
        shown.stdout.contains("SIMULATED: nothing will be printed")
            && shown.stdout.contains("Nothing was confirmed or sent"),
        "{}",
        shown.stdout
    );
    // The text a person confirms is the printer layer's, shown word for word and in one
    // piece: it names the plate that was chosen and ends with the file's hash.
    let checked = review["review_text"].as_str().unwrap();
    assert!(shown.stdout.contains(checked), "{}", shown.stdout);
    assert!(
        checked.contains("Build plate: Textured PEI Plate, as chosen for this job")
            && checked.ends_with(&format!("SHA-256 of the file: {one_lid}.")),
        "{checked}"
    );
    let status = json_of(at(&["print", "status", &job_id, "--json"]));
    assert_eq!(status[0]["status"], "pending", "a review changes nothing");

    // print project: the editable Bambu Studio project is made by the real Bambu Studio
    // only. The stand-in makes none and says so; saving a copy needs a person at a terminal
    // before anything is made, and nothing is written into the folder.
    let before: Vec<_> = std::fs::read_dir(files_arg).unwrap().collect();
    let refused = run(at(&["print", "project", "--fake"]));
    assert_eq!(refused.code, 1, "{}", refused.stdout);
    assert!(
        refused.stderr.contains("real Bambu Studio") && refused.stderr.contains("stand-in"),
        "{}",
        refused.stderr
    );
    let mut headless = at(&["print", "project", "--fake", "--out", files_arg]);
    headless.env("HOTEND_YES", "yes");
    headless.stdin(std::process::Stdio::null());
    let headless = run(headless);
    assert_eq!(headless.code, 2, "{}", headless.stdout);
    assert!(
        headless
            .stderr
            .contains("confirmed by a person at a terminal"),
        "{}",
        headless.stderr
    );
    let both = run(at(&["print", "project", "--out", files_arg, "--json"]));
    assert_eq!(both.code, 2, "{}", both.stderr);
    for flag in ["--yes", "--force"] {
        let flagged = run(at(&["print", "project", "--out", files_arg, flag]));
        assert_eq!(flagged.code, 2, "{flag}: {}", flagged.stderr);
    }
    assert_eq!(
        std::fs::read_dir(files_arg).unwrap().count(),
        before.len(),
        "nothing was saved"
    );

    // print confirm without a terminal: refused, whatever the arguments or environment.
    let pending = |what: &str| {
        let status = json_of(at(&["print", "status", &job_id, "--json"]));
        assert_eq!(status[0]["status"], "pending", "{what}");
        assert_eq!(status[0]["delivery"], Value::Null, "{what}");
    };
    let mut headless = at(&["print", "confirm", &job_id]);
    for name in ["HOTEND_CONFIRM", "HOTEND_YES", "HOTEND_ASSUME_YES", "CI"] {
        headless.env(name, "yes");
    }
    headless.stdin(std::process::Stdio::null());
    let headless = run(headless);
    assert_eq!(headless.code, 2, "{}", headless.stdout);
    assert!(
        headless
            .stderr
            .contains("confirmed by a person at a terminal"),
        "{}",
        headless.stderr
    );
    pending("no terminal");
    // `yes` on a pipe is not a person either.
    let mut piped = at(&["print", "confirm", &job_id]);
    piped.stdin(std::process::Stdio::piped());
    let mut piped = piped
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let _ = piped.stdin.take().unwrap().write_all(b"yes\nyes\n");
    }
    assert_eq!(piped.wait_with_output().unwrap().status.code(), Some(2));
    pending("yes on a pipe");
    for flag in ["--yes", "-y", "--confirm", "--force"] {
        let flagged = run(at(&["print", "confirm", &job_id, flag]));
        assert_eq!(flagged.code, 2, "{flag}: {}", flagged.stderr);
    }
    pending("a made-up flag");

    // At a terminal, an answer other than `yes` leaves it pending.
    let declined = at_a_terminal(&["--data-dir", data, "print", "confirm", &job_id], "Y\n");
    assert_eq!(declined.code, 1, "{}", declined.stdout);
    assert!(
        declined.stdout.contains("not confirmed"),
        "{}",
        declined.stdout
    );
    pending("declined at the terminal");

    // A `yes` typed before the review and the question were on the terminal is not an
    // answer to them: it is thrown away, and the end of the input leaves the job pending.
    let ahead = on_a_terminal(
        &["--data-dir", data, "print", "confirm", &job_id],
        "yes\nyes\n",
        Typed::Ahead,
        "",
    );
    assert_eq!(ahead.code, 1, "{}", ahead.stdout);
    assert!(ahead.stdout.contains(QUESTION), "{}", ahead.stdout);
    assert!(ahead.stdout.contains("not confirmed"), "{}", ahead.stdout);
    pending("yes typed ahead of the question");

    // A review that goes to a file is a review nobody read: refused, also with `yes`
    // typed at the terminal.
    let unseen = tmp.path().join("unseen-review.txt");
    for when in [Typed::Ahead, Typed::AfterTheQuestion] {
        let redirected = on_a_terminal(
            &["--data-dir", data, "print", "confirm", &job_id],
            "yes\n",
            when,
            &format!(" > '{}'", unseen.display()),
        );
        assert_eq!(redirected.code, 2, "{}", redirected.stdout);
        assert!(
            redirected
                .stdout
                .contains("standard output is not a terminal"),
            "{}",
            redirected.stdout
        );
        let written = std::fs::read_to_string(&unseen).unwrap();
        assert!(!written.contains(QUESTION), "{written}");
        pending("the review went to a file");
    }

    // At a terminal, `yes`: the simulated printer runs the job to its end.
    let confirmed = at_a_terminal(
        &[
            "--data-dir",
            data,
            "print",
            "confirm",
            &job_id,
            "--time-scale",
            "3600",
        ],
        "yes\n",
    );
    assert_eq!(confirmed.code, 0, "{}", confirmed.stdout);
    let mut found = 0;
    for stage in [
        "SIMULATED: nothing will be printed",
        "Run this job on the SIMULATED printer?",
        ": starting (simulated_printer)",
        ": printing (simulated_printer)",
        ": finished (simulated_printer)",
    ] {
        match confirmed.stdout[found..].find(stage) {
            Some(index) => found += index + stage.len(),
            None => panic!(
                "`{stage}` is missing or out of order in:\n{}",
                confirmed.stdout
            ),
        }
    }
    let status = json_of(at(&["print", "status", &job_id, "--json"]));
    assert_eq!(status[0]["status"], "finished");
    assert_eq!(status[0]["delivery"], "simulated_printer");
    assert_eq!(status[0]["detail"]["simulated"], true);
    // A job is confirmed once.
    let again = at_a_terminal(&["--data-dir", data, "print", "confirm", &job_id], "yes\n");
    assert_eq!(again.code, 1, "{}", again.stdout);

    // The other two ways: a folder, and the hand-off to Bambu Connect.
    let job = json_of(at(&["print", "propose", "--json"]));
    let second = job["id"].as_str().unwrap().to_owned();
    let no_folder = run(at(&["print", "review", &second, "--to", "folder"]));
    assert_eq!(no_folder.code, 2, "{}", no_folder.stderr);
    let exported = at_a_terminal(
        &[
            "--data-dir",
            data,
            "print",
            "confirm",
            &second,
            "--to",
            "folder",
            "--out",
            files_arg,
        ],
        "yes\n",
    );
    assert_eq!(exported.code, 0, "{}", exported.stdout);
    let status = json_of(at(&["print", "status", &second, "--json"]));
    assert_eq!(status[0]["status"], "exported");
    let written = PathBuf::from(status[0]["detail"]["path"].as_str().unwrap());
    assert!(written.starts_with(&files), "{}", written.display());
    assert_eq!(
        hotend_core::import::sha256_file(&written).unwrap().as_str(),
        one_lid
    );
    // A sliced file without the plate preview is not handed to Bambu Connect: its import
    // dialog would accept it and then import nothing.
    let job = json_of(at(&["print", "propose", "--json"]));
    let no_preview = job["id"].as_str().unwrap().to_owned();
    let refused = run(at(&[
        "print",
        "review",
        &no_preview,
        "--to",
        "bambu-connect",
    ]));
    assert_ne!(refused.code, 0, "{}", refused.stdout);
    // With previews off, the refusal says to switch them on and slice again.
    for part in [
        "Bambu Connect cannot import this slice",
        "Metadata/plate_1.png",
        "Switch on Hotend-made plate previews in Settings and slice again",
    ] {
        assert!(refused.stderr.contains(part), "{part}: {}", refused.stderr);
    }
    ok(at(&["print", "cancel", &no_preview]));
    // With Hotend-made plate previews the version is sliced again, the previews are in
    // the file the check reads, and the review says who made them.
    let shown = ok(at(&["settings", "show"]));
    assert!(
        shown
            .stdout
            .contains("plate previews:     the slicer's own"),
        "{}",
        shown.stdout
    );
    let set = json_of(at(&[
        "settings",
        "set",
        "--previews",
        "hotend-made",
        "--json",
    ]));
    assert_eq!(set["previews"], "hotend_made");
    // The slice made before the previews were switched on is refused with the one thing
    // left to do: slice again.
    let job = json_of(at(&["print", "propose", "--json"]));
    let outdated = job["id"].as_str().unwrap().to_owned();
    let refused = run(at(&["print", "review", &outdated, "--to", "bambu-connect"]));
    assert_ne!(refused.code, 0, "{}", refused.stdout);
    assert!(
        refused
            .stderr
            .contains("This slice was made before Hotend drew plate previews. Slice again"),
        "{}",
        refused.stderr
    );
    ok(at(&["print", "cancel", &outdated]));
    let sliced = json_of(at(&["slice", "--fake", "--json"]));
    assert!(
        sliced["summary"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "hotend_made_previews"),
        "{sliced}"
    );
    let with_previews = sliced["summary"]["sha256"].as_str().unwrap().to_owned();
    assert_ne!(with_previews, one_lid);
    let job = json_of(at(&["print", "propose", "--json"]));
    let third = job["id"].as_str().unwrap().to_owned();
    let review = ok(at(&["print", "review", &third, "--to", "bambu-connect"]));
    assert!(
        review.stdout.contains("plate previews made by Hotend"),
        "{}",
        review.stdout
    );
    let handed = at_a_terminal(
        &[
            "--data-dir",
            data,
            "print",
            "confirm",
            &third,
            "--to",
            "bambu-connect",
        ],
        "yes\n",
    );
    assert_eq!(handed.code, 0, "{}", handed.stdout);
    assert!(
        handed.stdout.contains("bambu-connect://import-file?path="),
        "{}",
        handed.stdout
    );
    // The command line prints the link and opens nothing, and says so.
    assert!(
        handed.stdout.contains("the command line does not open it"),
        "{}",
        handed.stdout
    );
    // A pending job can be cancelled; a delivered one cannot be confirmed again.
    let job = json_of(at(&["print", "propose", "--json"]));
    let fourth = job["id"].as_str().unwrap().to_owned();
    let cancelled = ok(at(&["print", "cancel", &fourth]));
    assert!(
        cancelled.stdout.contains("cancelled"),
        "{}",
        cancelled.stdout
    );
    let all = json_of(at(&["print", "status", "--json"]));
    let mut statuses: Vec<&str> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["status"].as_str().unwrap())
        .collect();
    statuses.sort_unstable();
    assert_eq!(
        statuses,
        [
            "cancelled",
            "cancelled",
            "cancelled",
            "cancelled",
            "exported",
            "finished",
            "handed_off"
        ]
    );

    // gauge and calibrate: a gauge plate for the version's joints, then what was measured.
    let gauges = json_of(at(&["gauge", "--out", files_arg, "--json"]));
    assert!(!gauges["gauges"].as_array().unwrap().is_empty(), "{gauges}");
    let plate = PathBuf::from(gauges["files"][0]["path"].as_str().unwrap());
    assert!(
        std::fs::read(&plate).unwrap().starts_with(b"PK\x03\x04"),
        "{}",
        plate.display()
    );
    let nothing = json_of(at(&["calibrate", "--json"]));
    assert_eq!(nothing["table"], Value::Null);
    let recorded = json_of(at(&[
        "calibrate",
        "--reading",
        "hole:6.49=6.31",
        "--reading",
        "peg:6.0=6.08",
        "--json",
    ]));
    assert_eq!(recorded["printer_id"], "bambu-x2d");
    let measured_at = recorded["measured_at"].as_str().unwrap();
    assert!(
        measured_at.len() == 20 && measured_at.starts_with("20") && measured_at.ends_with('Z'),
        "the core supplies the date: {measured_at}"
    );
    let quantities: Vec<&str> = recorded["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["quantity"].as_str().unwrap())
        .collect();
    assert_eq!(quantities, ["hole_offset", "peg_offset"]);
    let table = json_of(at(&["calibrate", "--json"]));
    assert_eq!(
        table["table"]["entries"][0]["hole_offset"]["provenance"],
        "measured"
    );
    assert_eq!(
        table["table"]["entries"][0]["hole_offset"]["measured_at"],
        measured_at
    );
    let bad = run(at(&["calibrate", "--reading", "slot:6=6"]));
    assert_eq!(bad.code, 2, "{}", bad.stderr);

    // settings: read, change, and the slicer choice (PrusaSlicer brings its printer).
    let before = json_of(at(&["settings", "show", "--json"]));
    assert_eq!(before["printer_id"], "bambu-x2d");
    assert_eq!(before["provider"], Value::Null);
    // A material the slicer has no profile for is refused, with the ones it has.
    let unknown = run(at(&["settings", "set", "--material", "petg-hf"]));
    assert_ne!(unknown.code, 0, "{}", unknown.stdout);
    assert!(
        unknown.stderr.contains("`petg-hf`")
            && unknown
                .stderr
                .contains("known materials are: pla-basic, pla-generic, petg-basic"),
        "{}",
        unknown.stderr
    );
    assert_eq!(
        json_of(at(&["settings", "show", "--json"]))["material"]["id"],
        before["material"]["id"]
    );
    let set = json_of(at(&[
        "settings",
        "set",
        "--material",
        "petg-basic",
        "--family",
        "PETG",
        "--density",
        "1.27",
        "--provider",
        "ollama",
        "--model",
        "some-model",
        "--max-turns",
        "30",
        "--json",
    ]));
    assert_eq!(set["material"]["id"], "petg-basic");
    assert_eq!(set["max_turns"], 30);
    let after = json_of(at(&["settings", "show", "--json"]));
    assert_eq!(after, set);
    let shown = ok(at(&["settings", "show"]));
    assert!(
        shown.stdout.contains("ollama / some-model"),
        "{}",
        shown.stdout
    );
    let slicers = json_of(at(&["settings", "slicers", "--json"]));
    let kinds: Vec<&str> = slicers
        .as_array()
        .unwrap()
        .iter()
        .map(|slicer| slicer["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"bambu-studio") && kinds.contains(&"prusa-slicer"),
        "{kinds:?}"
    );
    ok(at(&["settings", "set", "--slicer", "prusa-slicer"]));
    let prusa = json_of(at(&["settings", "show", "--json"]));
    assert_eq!(prusa["printer_id"], "prusa-mk4s");
    let unknown = run(at(&["settings", "set", "--slicer", "nonsense"]));
    assert_eq!(unknown.code, 2, "{}", unknown.stderr);
    let unknown = run(at(&["settings", "set", "--printer", "nonsense"]));
    assert_ne!(unknown.code, 0, "{}", unknown.stdout);

    // audit: every step above is on record, and no step was taken by a tool call.
    let audit = json_of(at(&["audit", "--all", "--limit", "1000", "--json"]));
    let audit = audit.as_array().unwrap();
    let names = |kind: &str| -> Vec<&str> {
        audit
            .iter()
            .rev()
            .filter(|entry| entry["kind"] == kind)
            .map(|entry| entry["name"].as_str().unwrap())
            .collect()
    };
    assert_eq!(
        names("print"),
        [
            "cancelled",
            "starting",
            "printing",
            "finished",
            "exported",
            "cancelled",
            "cancelled",
            "handed_off",
            "cancelled"
        ]
    );
    // The hand-off says what opened the link and that nothing was started (the command
    // line opens nothing), and names neither the link nor the file.
    let handed_off: Vec<&Value> = audit
        .iter()
        .filter(|entry| entry["kind"] == "print" && entry["name"] == "handed_off")
        .collect();
    assert_eq!(handed_off.len(), 1);
    let detail = &handed_off[0]["detail"];
    assert_eq!(detail["opener"], "stand-in opener: nothing was opened");
    assert_eq!(detail["opener_started"], false);
    assert_eq!(detail.get("opener_error"), None, "{detail}");
    let text = detail.to_string();
    assert!(!text.contains("bambu-connect://"), "{text}");
    assert!(!text.contains(".gcode.3mf"), "{text}");
    assert!(!text.contains(data), "{text}");
    // Each of the four slices wrote one entry, with the hash of the file it made.
    assert_eq!(names("slice"), ["slice"; 4]);
    let slices: Vec<&Value> = audit
        .iter()
        .rev()
        .filter(|entry| entry["kind"] == "slice")
        .collect();
    assert!(
        slices
            .iter()
            .all(|entry| entry["outcome"] == "ok"
                && entry["args_sha256"] == entry["detail"]["sha256"]),
        "{slices:?}"
    );
    assert_eq!(slices[3]["detail"]["sha256"], with_previews.as_str());
    assert_eq!(slices[3]["detail"]["previews_added_by_hotend"], true);
    assert_eq!(slices[0]["detail"]["previews_added_by_hotend"], false);
    assert!(
        slices
            .iter()
            .all(|entry| !entry["detail"].to_string().contains(data)),
        "{slices:?}"
    );
    assert_eq!(names("prepare").len(), 2);
    assert_eq!(names("calibration"), ["bambu-x2d"]);
    assert_eq!(names("gauge").len(), 1);
    assert!(names("version").contains(&"revert"));
    assert!(names("tool").is_empty(), "no agent ran here");
    let project_only = json_of(at(&["audit", "--json"]));
    assert!(
        project_only
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["kind"] != "calibration"),
        "the calibration belongs to the printer, not to a project"
    );
    let table = ok(at(&["audit", "--limit", "5"]));
    assert_eq!(table.stdout.lines().count(), 7, "{}", table.stdout);

    assert_no_secret_in_files(tmp.path());
}

#[test]
fn web_research_is_switched_on_only_by_a_person_and_is_narrowed_and_shown() {
    let test = "web_research_is_switched_on_only_by_a_person_and_is_narrowed_and_shown";
    if !ready(test) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().to_str().unwrap();
    let cli = |args: &[&str]| {
        let mut all = vec!["--data-dir", data];
        all.extend_from_slice(args);
        hotend(&all)
    };
    let state = |expected: &str| {
        let status = json_of(cli(&["research", "status", "--json"]));
        assert_eq!(status["state"], expected, "{status}");
        status
    };
    ok(cli(&["project", "new", "holder"]));

    // Without a provider there is nothing to search with; with one that has no web
    // research in Hotend, the command says so and changes nothing.
    assert!(
        ok(cli(&["research", "status"]))
            .stdout
            .contains("needs an AI provider")
    );
    ok(cli(&[
        "settings",
        "set",
        "--provider",
        "openai",
        "--model",
        "m",
    ]));
    state("unsupported_provider");
    let other = run(cli(&["research", "on"]));
    assert_eq!(other.code, 2, "{}", other.stdout);
    assert!(
        other.stderr.contains("not available with openai"),
        "{}",
        other.stderr
    );

    // With a provider that has it: off until a person agrees, and no pipe can agree.
    ok(cli(&[
        "settings",
        "set",
        "--provider",
        "anthropic",
        "--model",
        "m",
    ]));
    let waiting = state("needs_consent");
    assert_eq!(waiting["search"], false);
    assert_eq!(waiting["acknowledged"], false);
    let mut piped = cli(&["research", "on"]);
    piped.stdin(std::process::Stdio::null());
    let piped = run(piped);
    assert_eq!(piped.code, 2);
    assert!(
        piped.stderr.contains("a person at a terminal") && piped.stderr.contains("type `yes`"),
        "{}",
        piped.stderr
    );
    state("needs_consent");

    // At a terminal the person reads what leaves the computer and what it costs. Anything
    // but `yes` leaves it off.
    let declined = at_a_terminal(&["--data-dir", data, "research", "on"], "y\n");
    assert_eq!(declined.code, 0, "{}", declined.stdout);
    for piece in [
        "search queries and the addresses of the pages it reads go to your AI provider",
        "$10 per 1,000 searches",
        "It can never start a print or give the agent a new tool",
        "web research stays off; nothing was changed",
    ] {
        assert!(
            declined.stdout.contains(piece),
            "{piece}\n{}",
            declined.stdout
        );
    }
    state("needs_consent");
    let agreed = at_a_terminal(&["--data-dir", data, "research", "on"], "yes\n");
    assert_eq!(agreed.code, 0, "{}", agreed.stdout);
    assert!(
        agreed.stdout.contains("web research: on"),
        "{}",
        agreed.stdout
    );
    let on = state("on");
    assert_eq!(on["search"], true);
    assert_eq!(on["fetch"], true);
    assert_eq!(on["acknowledged"], true);
    assert_eq!(on["project"]["enabled"], true);
    assert_eq!(
        (on["searches_cap"].as_u64(), on["fetches_cap"].as_u64()),
        (Some(8), Some(6))
    );
    // The second time nothing is asked.
    let again = run({
        let mut command = cli(&["research", "on"]);
        command.stdin(std::process::Stdio::null());
        command
    });
    assert_eq!(again.code, 0, "{}", again.stderr);

    // Narrowing: sites, limits, PDFs and links. A wrong host name is refused with the fix.
    ok(cli(&[
        "research",
        "allow",
        "maker.example",
        "docs.maker.example",
    ]));
    let allowed = state("on");
    assert_eq!(
        allowed["project"]["allowed_domains"],
        serde_json::json!(["maker.example", "docs.maker.example"])
    );
    ok(cli(&["research", "block", "ads.example"]));
    let blocked = state("on");
    assert_eq!(blocked["project"]["allowed_domains"], serde_json::json!([]));
    assert_eq!(
        blocked["project"]["blocked_domains"],
        serde_json::json!(["ads.example"])
    );
    for bad in [
        "https://maker.example",
        "maker.example/specs",
        "*.example",
        "10.0.0.1",
    ] {
        let refused = run(cli(&["research", "allow", bad]));
        assert_eq!(refused.code, 2, "{bad}");
        assert!(
            refused.stderr.contains("is not a host name"),
            "{}",
            refused.stderr
        );
    }
    ok(cli(&["research", "block"]));
    ok(cli(&[
        "research",
        "limit",
        "--searches",
        "2",
        "--fetches",
        "0",
    ]));
    let limited = state("on");
    assert_eq!(limited["searches_cap"], 2);
    assert_eq!(
        limited["fetch"], false,
        "no page may be fetched with a limit of 0"
    );
    assert_eq!(run(cli(&["research", "limit"])).code, 2);
    let text = ok(cli(&["research", "status"])).stdout;
    for piece in [
        "web research: on",
        "searches: 0 of 2 used in this project (at most 3 per message)",
        "sites: no limit",
        "links inside fetched pages: not followed; fetched PDFs: not kept",
        "agreement that queries go to the AI provider: given",
    ] {
        assert!(text.contains(piece), "{piece}\n{text}");
    }
    ok(cli(&["research", "pdf", "on"]));
    ok(cli(&["research", "links", "on"]));
    let loose = state("on");
    assert_eq!(loose["project"]["allow_pdf"], true);
    assert_eq!(loose["project"]["follow_links"], true);

    // Nothing was searched: no notes, no log, and a note that does not exist cannot be
    // confirmed.
    assert!(
        ok(cli(&["research", "notes"]))
            .stdout
            .contains("no number from the web is recorded")
    );
    assert_eq!(
        json_of(cli(&["research", "notes", "--json"])),
        serde_json::json!([])
    );
    assert!(
        ok(cli(&["research", "log"]))
            .stdout
            .contains("has not searched the web")
    );
    assert_eq!(run(cli(&["research", "confirm", "r1"])).code, 1);

    // The defaults for every project, and what the settings say.
    let defaults = json_of(cli(&[
        "research",
        "defaults",
        "--searches-per-message",
        "2",
        "--cache",
        "5m",
        "--json",
    ]));
    assert_eq!(defaults["searches_per_turn"], 2);
    assert_eq!(defaults["cache_ttl"], "5m");
    assert!(defaults["acknowledged_at_ms"].is_i64());
    assert!(
        ok(cli(&["settings", "show"]))
            .stdout
            .contains("web research:       on for projects that do not say otherwise; 2 searches")
    );

    // Off for the project, and the agreement can be taken back for all of them.
    ok(cli(&["research", "off"]));
    assert_eq!(state("off")["search"], false);
    ok(cli(&["research", "on"]));
    state("on");
    ok(cli(&["research", "withdraw"]));
    state("needs_consent");

    // Every change is in the audit log; nothing here printed or stored a key.
    let audit = json_of(cli(&["audit", "--all", "--limit", "100", "--json"]));
    let research: Vec<&Value> = audit
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "research")
        .collect();
    assert!(
        research
            .iter()
            .any(|e| e["name"] == "consent" && e["outcome"] == "agreed")
            && research
                .iter()
                .any(|e| e["name"] == "consent" && e["outcome"] == "withdrawn")
            && research.iter().filter(|e| e["name"] == "settings").count() >= 8,
        "{research:?}"
    );
    assert_no_secret_in_files(tmp.path());
}

#[test]
fn keys_are_reported_as_set_or_not_and_never_taken_from_the_command_line() {
    // No sandbox or Python needed: only the secret stores are asked.
    let status = ok(hotend(&["key", "status", "--secrets", "env", "--json"]));
    let report: Value = serde_json::from_str(&status.stdout).unwrap();
    assert_eq!(report["store"], "env");
    let state = |provider: &str| {
        report["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider"] == provider)
            .map(|row| row["key"]["state"].as_str().unwrap().to_owned())
    };
    assert_eq!(state("anthropic").as_deref(), Some("set"));
    assert_eq!(state("google").as_deref(), Some("set"));
    assert_eq!(state("ollama").as_deref(), Some("not_needed"));
    let mut without = hotend(&["key", "status", "--secrets", "env"]);
    without.env_remove("OPENAI_API_KEY");
    let without = ok(without);
    let openai = without
        .stdout
        .lines()
        .find(|line| line.starts_with("openai"))
        .unwrap();
    assert!(
        openai.contains("not set") && openai.contains("OPENAI_API_KEY"),
        "{openai}"
    );

    // A key given as an argument is refused before anything reads it.
    // (`run` fails the test if the value comes back on stdout or stderr.)
    let argv = run(hotend(&[
        "key",
        "set",
        "anthropic",
        "sk-ant-in-argv-0000000000000000",
    ]));
    assert_eq!(argv.code, 2);
    assert!(
        argv.stderr.contains("never read from the command line"),
        "{}",
        argv.stderr
    );
    // The environment store is read-only, and says where the key goes instead.
    let read_only = run(hotend(&["key", "set", "anthropic", "--secrets", "env"]));
    assert_eq!(read_only.code, 2, "{}", read_only.stderr);
    assert!(
        read_only.stderr.contains("ANTHROPIC_API_KEY"),
        "{}",
        read_only.stderr
    );
}

#[test]
fn the_research_source_is_chosen_at_the_command_line_and_needs_agreement_again() {
    let test = "the_research_source_is_chosen_at_the_command_line_and_needs_agreement_again";
    if !ready(test) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().to_str().unwrap();
    let cli = |args: &[&str]| {
        let mut all = vec!["--data-dir", data];
        all.extend_from_slice(args);
        let mut command = hotend(&all);
        // The search keys come from the canary variables, read as the model key is.
        command.env("HOTEND_SECRETS", "env");
        command
    };
    let status = || json_of(cli(&["research", "status", "--json"]));
    ok(cli(&["project", "new", "holder"]));
    ok(cli(&[
        "settings",
        "set",
        "--provider",
        "ollama",
        "--model",
        "m",
    ]));

    // A local model has no built-in search: the state says so and points at Keenable.
    let before = status();
    assert_eq!(before["state"], "unsupported_provider", "{before}");
    assert_eq!(before["source"], "built_in");
    let refused = run(cli(&["research", "on"]));
    assert_eq!(refused.code, 2);
    assert!(
        refused.stderr.contains("research source keenable"),
        "{}",
        refused.stderr
    );

    // Choosing Keenable needs an agreement for Keenable, given only at a terminal.
    let chosen = ok(cli(&["research", "source", "keenable"]));
    assert!(
        chosen.stdout.contains("hotend research on"),
        "{}",
        chosen.stdout
    );
    let keenable = status();
    assert_eq!(keenable["state"], "needs_consent", "{keenable}");
    assert_eq!(keenable["source"], "keenable");
    assert_eq!(keenable["service_host"], "api.keenable.ai");
    assert_eq!(keenable["key_present"], true);
    assert_eq!(keenable["keyless"], false);
    let not_a_terminal = run(cli(&["research", "on"]));
    assert_ne!(not_a_terminal.code, 0);
    assert_eq!(status()["state"], "needs_consent");
    let text = ok(cli(&["research", "status"])).stdout;
    assert!(
        text.contains("source: keenable; Keenable key: stored"),
        "{text}"
    );
    assert!(text.contains("agreement that queries go to Keenable (api.keenable.ai): not given"));

    // Tavily, then off; `key status` lists the services without showing a key.
    ok(cli(&["research", "source", "tavily"]));
    assert_eq!(status()["source"], "tavily");
    ok(cli(&["research", "source", "off"]));
    assert_eq!(status()["state"], "off");
    let keys = json_of(cli(&["key", "status", "--json"]));
    let services = keys["search_services"].as_array().unwrap();
    assert_eq!(services.len(), 2);
    assert_eq!(services[0]["service"], "keenable");
    assert_eq!(services[0]["key"]["state"], "set");
    let shown = ok(cli(&["settings", "show"])).stdout;
    assert!(shown.contains("source: off"), "{shown}");
    for ran in [shown, text, keys.to_string()] {
        assert!(secret_like(ran.as_bytes()).is_none(), "{ran}");
    }
    let settings = json_of(cli(&["settings", "show", "--json"]));
    assert!(secret_like(settings.to_string().as_bytes()).is_none());
}
