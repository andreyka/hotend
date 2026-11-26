//! Snapshots: take the model as it is at any time, keep iterating, export that state's files
//! later and go back to it without losing the newer line.
//!
//! Through the core's facade with the stand-in slicer and the stand-in link opener (a
//! Bambu Connect hand-off that opens nothing). Nothing here talks to a model, a real printer
//! or the network.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use common::*;
use hotend_core::{
    AgentCommand, ConfirmationAuthority, Core, CoreConfig, CoreError, DeliveryChoice, ExportFormat,
    MemorySecretStore, ProviderKind, ProviderSettings, RunEvent, Secret, SecretBackend,
    SecretStore, Settings,
};
use hotend_slicer::Programs;
use serde_json::{Value, json};

const MODEL: &str = include_str!("../../../python/tests/models/cable_box.py");
/// A key for the in-memory secret store; the scripted agent never sends a request.
const TEST_KEY: &str = "sk-test-integration-0123456789";

/// The scripted agent of hotend-core's integration tests.
fn scripted_agent(runtime: &hotend_core::PythonRuntime) -> AgentCommand {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../hotend-core/tests/fixtures")
        .canonicalize()
        .unwrap();
    let mut read_only = runtime.read_only.clone();
    read_only.push(fixtures.clone());
    AgentCommand {
        program: runtime.python.clone(),
        args: vec![
            "-I".into(),
            fixtures
                .join("fake_agent.py")
                .to_string_lossy()
                .into_owned(),
        ],
        read_only,
    }
}

fn authority() -> &'static ConfirmationAuthority {
    static AUTHORITY: OnceLock<ConfirmationAuthority> = OnceLock::new();
    AUTHORITY.get_or_init(|| ConfirmationAuthority::take().expect("taken once"))
}

struct Fixture {
    tmp: tempfile::TempDir,
    core: Core,
    project: String,
}

impl Fixture {
    fn versions_dir(&self) -> PathBuf {
        self.tmp
            .path()
            .join("data/projects")
            .join(&self.project)
            .join("versions")
    }
}

/// A core with the stand-in slicer and the cable box built as version 1.
async fn built(test: &str) -> Option<Fixture> {
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
    let secrets = Arc::new(MemorySecretStore::default());
    let core = Core::open(CoreConfig {
        data_dir: Some(tmp.path().join("data")),
        secrets: SecretBackend::Custom(secrets.clone()),
        agent: Some(scripted_agent(&runtime)),
        runtime: Some(runtime),
        ..CoreConfig::default()
    })
    .await
    .unwrap();
    hotend_slicer::install(&core, Programs::StandIn).unwrap();
    secrets
        .set("provider-key.anthropic", Secret::new(TEST_KEY))
        .unwrap();
    core.settings_set(Settings {
        provider: Some(ProviderSettings {
            kind: ProviderKind::Anthropic,
            model: "test-model".into(),
        }),
        plate_type: Some("textured-pei".to_owned()),
        // The stand-in slicer draws no plate preview; a Bambu Connect hand-off needs one.
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
    assert_eq!(built.version.map(|v| v.number), Some(1));
    Some(Fixture {
        tmp,
        core,
        project: project.id,
    })
}

/// Every file and folder under `dir` with the SHA-256 of each file's bytes.
fn tree(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if entry.file_type().unwrap().is_dir() {
                out.insert(format!("{name}/"), String::new());
                walk(root, &path, out);
            } else {
                out.insert(name, sha256(&path));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The entries of a 3MF (a zip) by name, with each object's `UUID="..."` value blanked.
fn entries(archive: &[u8]) -> BTreeMap<String, String> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).unwrap();
    let mut out = BTreeMap::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).unwrap();
        let mut text = String::new();
        entry.read_to_string(&mut text).unwrap();
        let mut blanked = String::new();
        let mut rest = text.as_str();
        while let Some(at) = rest.find("UUID=\"") {
            let (head, tail) = rest.split_at(at + "UUID=\"".len());
            blanked.push_str(head);
            rest = tail.find('"').map_or("", |end| &tail[end..]);
        }
        blanked.push_str(rest);
        out.insert(entry.name().unwrap().into_owned(), blanked);
    }
    out
}

/// The lines of a STEP file without their entity numbers and references, sorted: the same
/// entities with the same values, whatever order and numbering the writer chose.
fn step_entities(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines: Vec<String> = text
        .lines()
        .map(|line| {
            let mut out = String::new();
            let mut chars = line.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '#' {
                    while chars.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                    }
                    out.push('#');
                } else {
                    out.push(c);
                }
            }
            out
        })
        .collect();
    lines.sort_unstable();
    lines
}

/// Proposes the current slice and hands it over to Bambu Connect (the stand-in opens
/// nothing): the job and the file it placed.
async fn hand_off(core: &Core, project: &str, note: &str) -> (hotend_core::PrintJob, PathBuf) {
    let job = core.print_propose(project, note).await.unwrap();
    let review = core
        .print_review(&job.id, DeliveryChoice::BambuConnect)
        .await
        .unwrap();
    let handed = core
        .print_confirm(authority(), &job.id, &review.review_text)
        .await
        .unwrap();
    assert_eq!(handed.status, "handed_off");
    let path = handed.detail.path.clone().unwrap();
    assert_eq!(sha256(&path), job.slice_sha256);
    (job, path)
}

/// Removes every place the printed file of version 1 could still be found but the copy kept
/// with a snapshot: the hand-off file and the slice folder's file (changed).
async fn lose_the_print(core: &Core, project: &str, handed: &Path) {
    let _ = std::fs::remove_file(handed);
    let slice = core.slice_get(project, Some(1)).await.unwrap().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&slice.output_path, std::fs::Permissions::from_mode(0o644))
            .unwrap();
    }
    std::fs::write(&slice.output_path, b"changed").unwrap();
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort_unstable();
    names
}

#[tokio::test]
async fn a_snapshot_exports_its_versions_files_and_the_exact_printed_file() {
    let test = "a_snapshot_exports_its_versions_files_and_the_exact_printed_file";
    let Some(fixture) = built(test).await else {
        return;
    };
    let core = &fixture.core;
    let project = fixture.project.as_str();

    // Version 1 is sliced and handed over to Bambu Connect (the stand-in opens nothing).
    core.slice_run(project, "demo-0.20mm", None).await.unwrap();
    let job = core.print_propose(project, "hand off v1").await.unwrap();
    let review = core
        .print_review(&job.id, DeliveryChoice::BambuConnect)
        .await
        .unwrap();
    let handed = core
        .print_confirm(authority(), &job.id, &review.review_text)
        .await
        .unwrap();
    assert_eq!(handed.status, "handed_off");
    let handed_file = handed.detail.path.clone().unwrap();
    let printed_bytes = std::fs::read(&handed_file).unwrap();
    assert_eq!(sha256(&handed_file), job.slice_sha256);

    // Named after it was printed: the exact file is kept with the snapshot.
    let first = core
        .snapshot_create(project, "first print", None, "as handed over")
        .await
        .unwrap();
    assert_eq!(first.version, 1);
    assert_eq!(first.printed_job_id.as_deref(), Some(job.id.as_str()));
    assert_eq!(
        first.printed_sha256.as_deref(),
        Some(job.slice_sha256.to_lowercase().as_str())
    );
    // Version 1 is laid out again, sliced again (other bytes) and handed over again: the
    // snapshot still exports the file of the print it recorded.
    core.prepare_run(
        project,
        None,
        hotend_core::PrepareOptions {
            copies: [("lid".to_owned(), 2)].into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    core.slice_run(project, "demo-0.20mm", None).await.unwrap();
    let (again_job, _) = hand_off(core, project, "hand off v1 again").await;
    assert_eq!(again_job.version, 1);
    assert_ne!(again_job.slice_sha256, job.slice_sha256);
    // The design moves on: version 2, named too (never printed).
    let wider = core
        .model_build(
            project,
            MODEL,
            BTreeMap::from([("width".to_owned(), 130.0)]),
            "wider",
        )
        .await
        .unwrap();
    assert_eq!(wider.version.map(|v| v.number), Some(2));
    core.snapshot_create(project, "wider", None, "")
        .await
        .unwrap();
    let versions = tree(&fixture.versions_dir());

    // The files of the hand-off and of the slice folder may go: the kept copy is enough.
    std::fs::remove_file(&handed_file).unwrap();
    let out = tempfile::tempdir().unwrap();
    let export = core
        .snapshot_export(project, "first print", out.path())
        .await
        .unwrap();
    assert_eq!(
        export.folder,
        out.path().join("Cable box - first print (v1)")
    );
    let printed_name = handed_file
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut expected = vec![
        "model.3mf".to_owned(),
        "model.step".to_owned(),
        printed_name.clone(),
        "snapshot.json".to_owned(),
    ];
    expected.sort_unstable();
    assert_eq!(names(&export.folder), expected);

    // STEP and 3MF are the bytes `export_run` writes for that version.
    let direct = tempfile::tempdir().unwrap();
    let exported = core
        .export_run(
            project,
            Some(1),
            vec![ExportFormat::Step, ExportFormat::ThreeMf],
            None,
            direct.path(),
        )
        .await
        .unwrap();
    // Two exports of one version are not always byte-identical: the engine's 3MF writer
    // gives every object a new random UUID, and its STEP writer may order (and so number)
    // the colour styles differently. They match entry for entry apart from those.
    for file in &exported {
        let name = file.path.file_name().unwrap();
        let (ours, theirs) = (
            std::fs::read(export.folder.join(name)).unwrap(),
            std::fs::read(&file.path).unwrap(),
        );
        if file.format == ExportFormat::ThreeMf {
            assert_eq!(entries(&ours), entries(&theirs), "model.3mf");
        } else {
            assert_eq!(ours.len(), theirs.len(), "model.step");
            assert!(
                step_entities(&ours) == step_entities(&theirs),
                "model.step: other entities"
            );
            // The STEP is the version's own stored file, byte for byte.
            assert_eq!(
                ours,
                std::fs::read(fixture.versions_dir().join("1/model.step")).unwrap()
            );
        }
    }
    // The version stores a STEP (copied) and no 3MF (exported).
    let source = |kind: &str| {
        export
            .files
            .iter()
            .find(|file| file.kind == kind)
            .map(|file| file.source.as_str())
    };
    assert_eq!(source("step"), Some("copied"));
    assert_eq!(source("3mf"), Some("exported"));
    assert_eq!(source("printed"), Some("copied"));
    // The printed file is byte for byte the one that was handed over.
    let printed = export.printed.as_ref().unwrap();
    assert_eq!(printed.job_id, job.id);
    assert_eq!(printed.delivery.as_deref(), Some("bambu_connect"));
    assert_eq!(printed.sha256, job.slice_sha256.to_lowercase());
    assert_eq!(
        std::fs::read(export.folder.join(&printed_name)).unwrap(),
        printed_bytes
    );
    // The manifest names the snapshot and every file with its hash, as written.
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(export.folder.join("snapshot.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["snapshot"]["name"], "first print");
    assert_eq!(manifest["snapshot"]["version"], 1);
    assert_eq!(manifest["snapshot"]["note"], "as handed over");
    assert!(
        manifest["snapshot"]["created"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    assert_eq!(manifest["printed"]["job_id"], job.id.as_str());
    let listed = manifest["files"].as_array().unwrap();
    assert_eq!(listed.len(), 3);
    for entry in listed {
        let path = export.folder.join(entry["name"].as_str().unwrap());
        assert_eq!(entry["sha256"].as_str().unwrap(), sha256(&path), "{entry}");
        let expected = if entry["kind"] == "3mf" {
            "exported"
        } else {
            "copied"
        };
        assert_eq!(entry["source"], expected, "{entry}");
    }
    for file in &export.files {
        assert_eq!(file.sha256, sha256(&file.path), "{}", file.kind);
    }

    // A snapshot of a version that was never printed exports STEP, 3MF and the manifest.
    let wide = core
        .snapshot_export(project, "wider", out.path())
        .await
        .unwrap();
    assert!(wide.printed.is_none());
    assert_eq!(
        names(&wide.folder),
        ["model.3mf", "model.step", "snapshot.json"]
    );
    // Exporting again into the same folder is refused: nothing is replaced.
    let before = tree(out.path());
    let error = core
        .snapshot_export(project, "wider", out.path())
        .await
        .unwrap_err();
    assert!(
        matches!(&error, CoreError::InvalidInput(text)
            if text.contains("already exists") && text.contains("Choose another folder")),
        "{error}"
    );
    assert_eq!(tree(out.path()), before);

    // A relative or missing folder is refused; nothing is written.
    for folder in [Path::new("relative"), &out.path().join("missing")] {
        let error = core
            .snapshot_export(project, "wider", folder)
            .await
            .unwrap_err();
        assert!(matches!(error, CoreError::InvalidInput(_)), "{error}");
    }
    // No export, and no snapshot, changed a version or the current version.
    assert_eq!(tree(&fixture.versions_dir()), versions);
    let detail = core.project_get(project).await.unwrap();
    assert_eq!(detail.project.current_version, Some(2));
    let audit = core.audit_log(Some(project), 200).await.unwrap();
    let exports: Vec<&str> = audit
        .iter()
        .filter(|entry| entry.kind == "snapshot" && entry.name == "export")
        .map(|entry| entry.outcome.as_str())
        .collect();
    assert_eq!(
        exports,
        [
            "invalid_input",
            "invalid_input",
            "invalid_input",
            "ok",
            "ok"
        ]
    );
}

#[tokio::test]
async fn a_snapshot_made_before_the_print_exports_no_printed_file() {
    let test = "a_snapshot_made_before_the_print_exports_no_printed_file";
    let Some(fixture) = built(test).await else {
        return;
    };
    let core = &fixture.core;
    let project = fixture.project.as_str();
    core.slice_run(project, "demo-0.20mm", None).await.unwrap();
    // Named before version 1 was printed: it records no print, and a later print of the
    // same version does not become its file.
    let before = core
        .snapshot_create(project, "before", None, "")
        .await
        .unwrap();
    assert_eq!((before.printed_job_id, before.printed_sha256), (None, None));
    hand_off(core, project, "hand off").await;
    let out = tempfile::tempdir().unwrap();
    let export = core
        .snapshot_export(project, "before", out.path())
        .await
        .unwrap();
    assert!(export.printed.is_none() && export.printed_missing.is_none());
    assert_eq!(
        names(&export.folder),
        ["model.3mf", "model.step", "snapshot.json"]
    );
}

#[tokio::test]
async fn a_printed_file_that_is_gone_everywhere_still_exports_the_model_and_says_so() {
    let test = "a_printed_file_that_is_gone_everywhere_still_exports_the_model_and_says_so";
    let Some(fixture) = built(test).await else {
        return;
    };
    let core = &fixture.core;
    let project = fixture.project.as_str();
    core.slice_run(project, "demo-0.20mm", None).await.unwrap();
    let (job, handed) = hand_off(core, project, "hand off").await;
    let after = core
        .snapshot_create(project, "after", None, "")
        .await
        .unwrap();
    // The kept copy, the hand-off file and the slice folder's file all go.
    let kept = fixture
        .tmp
        .path()
        .join("data/projects")
        .join(project)
        .join("snapshots")
        .join(&after.id);
    assert_eq!(names(&kept).len(), 1);
    std::fs::remove_dir_all(&kept).unwrap();
    lose_the_print(core, project, &handed).await;

    let out = tempfile::tempdir().unwrap();
    let export = core
        .snapshot_export(project, "after", out.path())
        .await
        .unwrap();
    assert!(export.printed.is_none());
    let missing = export.printed_missing.as_ref().unwrap();
    assert_eq!(missing.job_id, job.id);
    assert_eq!(missing.sha256, job.slice_sha256.to_lowercase());
    assert!(
        missing.message.starts_with("printed file missing"),
        "{}",
        missing.message
    );
    assert_eq!(
        names(&export.folder),
        ["model.3mf", "model.step", "snapshot.json"]
    );
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(export.folder.join("snapshot.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["printed"], Value::Null);
    assert_eq!(manifest["printed_missing"]["job_id"], job.id.as_str());
    assert!(
        manifest["printed_missing"]["message"]
            .as_str()
            .unwrap()
            .starts_with("printed file missing")
    );
    assert_eq!(manifest["snapshot"]["printed_job_id"], job.id.as_str());
}

#[tokio::test]
async fn an_agents_snapshot_keeps_the_printed_file_too() {
    let test = "an_agents_snapshot_keeps_the_printed_file_too";
    let Some(fixture) = built(test).await else {
        return;
    };
    let core = &fixture.core;
    let project = fixture.project.as_str();
    core.slice_run(project, "demo-0.20mm", None).await.unwrap();
    let (job, handed) = hand_off(core, project, "hand off").await;
    let printed_bytes = std::fs::read(&handed).unwrap();

    let steps = json!([
        {"tool": "snapshot_model", "args": {"name": "Lid fits"}},
        {"say": "Saved this state as a snapshot."}
    ]);
    let mut run = core
        .message_send(project, &json!({"steps": steps}).to_string())
        .await
        .unwrap();
    let mut events = Vec::new();
    let all = async {
        while let Some(event) = run.next().await {
            events.push(event);
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(300), all)
        .await
        .expect("the run did not end");
    assert!(
        matches!(events.last(), Some(RunEvent::Finished { error: None, .. })),
        "{events:?}"
    );
    let snapshot = core.snapshots_list(project).await.unwrap().pop().unwrap();
    assert_eq!(snapshot.name, "Lid fits");
    assert_eq!(snapshot.printed_job_id.as_deref(), Some(job.id.as_str()));

    // Every other place of the file goes: the copy the agent's snapshot kept is exported.
    lose_the_print(core, project, &handed).await;
    let out = tempfile::tempdir().unwrap();
    let export = core
        .snapshot_export(project, "lid fits", out.path())
        .await
        .unwrap();
    let printed = export.printed.as_ref().unwrap();
    assert_eq!(printed.job_id, job.id);
    assert_eq!(
        std::fs::read(export.folder.join(&printed.file_name)).unwrap(),
        printed_bytes
    );
}

#[tokio::test]
async fn an_export_never_writes_into_a_folder_that_exists() {
    let test = "an_export_never_writes_into_a_folder_that_exists";
    let Some(fixture) = built(test).await else {
        return;
    };
    let core = &fixture.core;
    let project = fixture.project.as_str();
    // Names the store tells apart that make the same folder name, or names that differ only
    // in letter case beyond ASCII (one folder on many file systems).
    for (first, second) in [("Lid*", "Lid?"), ("\u{00DC}ber", "\u{00FC}ber")] {
        core.snapshot_create(project, first, None, "")
            .await
            .unwrap();
        core.snapshot_create(project, second, None, "")
            .await
            .unwrap();
        let out = tempfile::tempdir().unwrap();
        let export = core
            .snapshot_export(project, first, out.path())
            .await
            .unwrap();
        let written = tree(out.path());
        let error = core
            .snapshot_export(project, second, out.path())
            .await
            .unwrap_err();
        assert!(
            matches!(&error, CoreError::InvalidInput(text)
                if text.contains("already exists") && text.contains("remove that one")),
            "{first} / {second}: {error}"
        );
        assert_eq!(tree(out.path()), written, "{first} / {second}");
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(export.folder.join("snapshot.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["snapshot"]["name"], first);
    }
    // A file in the way is refused the same way.
    let out = tempfile::tempdir().unwrap();
    std::fs::write(out.path().join("Cable box - Lid_ (v1)"), b"mine").unwrap();
    let error = core
        .snapshot_export(project, "Lid*", out.path())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already exists"), "{error}");
    assert_eq!(
        std::fs::read(out.path().join("Cable box - Lid_ (v1)")).unwrap(),
        b"mine"
    );
}

#[tokio::test]
async fn going_back_to_a_snapshot_keeps_the_newer_line_and_branches_from_it() {
    let test = "going_back_to_a_snapshot_keeps_the_newer_line_and_branches_from_it";
    let Some(fixture) = built(test).await else {
        return;
    };
    let core = &fixture.core;
    let project = fixture.project.as_str();
    core.snapshot_create(project, "start", None, "")
        .await
        .unwrap();
    for width in [120.0, 130.0] {
        core.model_build(
            project,
            MODEL,
            BTreeMap::from([("width".to_owned(), width)]),
            "",
        )
        .await
        .unwrap();
    }
    core.snapshot_create(project, "widest", None, "")
        .await
        .unwrap();
    let line = tree(&fixture.versions_dir());

    // Back to the start: version 1 is current; versions 2 and 3 are untouched.
    let restored = core.snapshot_restore(project, "START").await.unwrap();
    assert_eq!(restored.number, 1);
    let detail = core.project_get(project).await.unwrap();
    assert_eq!(detail.project.current_version, Some(1));
    assert_eq!(detail.versions.len(), 3);
    assert_eq!(tree(&fixture.versions_dir()), line);

    // A new build continues from the restored version: version 4, parent 1.
    let next = core
        .model_build(
            project,
            MODEL,
            BTreeMap::from([("width".to_owned(), 110.0)]),
            "from the start",
        )
        .await
        .unwrap()
        .version
        .unwrap();
    assert_eq!((next.number, next.parent), (4, Some(1)));
    let versions = core.versions_list(project).await.unwrap();
    let parents: Vec<(u32, Option<u32>)> = versions.iter().map(|v| (v.number, v.parent)).collect();
    assert_eq!(
        parents,
        [(1, None), (2, Some(1)), (3, Some(2)), (4, Some(1))]
    );
    // The older line is still whole, and reachable by its snapshot.
    for (path, hash) in &line {
        assert_eq!(
            tree(&fixture.versions_dir()).get(path),
            Some(hash),
            "{path}"
        );
    }
    assert_eq!(
        core.snapshot_restore(project, "widest")
            .await
            .unwrap()
            .number,
        3
    );
    assert_eq!(
        core.project_get(project)
            .await
            .unwrap()
            .project
            .current_version,
        Some(3)
    );
    // Deleting a name removes the name only.
    let removed = core.snapshot_delete(project, "widest").await.unwrap();
    assert_eq!(removed.version, 3);
    assert_eq!(core.versions_list(project).await.unwrap().len(), 4);
    assert!(matches!(
        core.snapshot_restore(project, "widest").await,
        Err(CoreError::Store(_))
    ));
    let audit = core.audit_log(Some(project), 200).await.unwrap();
    let mut actions: Vec<&str> = audit
        .iter()
        .rev()
        .filter(|entry| entry.kind == "snapshot")
        .map(|entry| entry.name.as_str())
        .collect();
    actions.dedup();
    assert_eq!(actions, ["create", "restore", "delete"]);
}
