//! The Bambu Connect hand-off through the shell, on a real core (stand-in slicer), with
//! the stand-in opener in place of the real one. What it proves:
//!
//! - after the person confirms in the native prompt, the checked file is placed in the
//!   hand-off folder byte for byte, and the opener is asked to open exactly the link stored
//!   with the job, once; the job records the path, the opener and that nothing was opened;
//! - the prompt says where the file goes and that the stand-in opens nothing;
//! - "Cancel" asks the opener nothing;
//! - a file that changed after the check, or that fails the check, never reaches the
//!   opener, by any path;
//! - under WSL, a hand-off without a Windows drive folder is refused before the prompt,
//!   with the sentence that says what to do, and the way is listed as not available.
//!
//! No real opener exists in this test: the core is given the stand-in, and nothing here
//! starts a program.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test stops at the first surprise"
)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hotend_core::{ApiError, Core, HostKind, StandInOpener};
use hotend_desktop::confirm::{Answer, ConfirmationPrompt, ConfirmationText};
use hotend_desktop::grants::NoPickers;
use hotend_desktop::ipc::{self, DispatchError};
use hotend_desktop::links::LinkOpener as BrowserOpener;
use hotend_desktop::shell::{Desktop, Shell};
use serde_json::{Value, json};
use url::Url;

/// Answers every prompt with what the test queued, and keeps what it was shown.
#[derive(Default)]
struct Prompt {
    shown: Mutex<Vec<ConfirmationText>>,
    answers: Mutex<Vec<Answer>>,
}

impl ConfirmationPrompt for Prompt {
    fn ask(&self, text: &ConfirmationText) -> Answer {
        self.shown.lock().unwrap().push(text.clone());
        let mut answers = self.answers.lock().unwrap();
        assert!(!answers.is_empty(), "an answer is queued for every prompt");
        answers.remove(0)
    }
}

struct NoBrowser;

impl BrowserOpener for NoBrowser {
    fn open(&self, url: &Url) -> Result<(), String> {
        panic!("no browser link is opened here: {url}")
    }
}

struct Fixture {
    shell: Shell,
    core: Core,
    prompt: Arc<Prompt>,
    opener: Arc<StandInOpener>,
    _data: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let data = tempfile::tempdir().unwrap();
    let core = support::open_core(data.path()).await;
    let opener = Arc::new(StandInOpener::new());
    core.set_link_opener(opener.clone());
    core.set_handoff_host(HostKind::Linux);
    // The stand-in slicer draws no plate preview, and Bambu Connect is not handed a file
    // without one: the slices of these tests carry Hotend-made previews.
    let mut settings = core.settings_get().await.unwrap();
    settings.previews = hotend_core::PreviewsMode::HotendMade;
    core.settings_set(settings).await.unwrap();
    let prompt = Arc::new(Prompt::default());
    let shell = Shell::new(
        Ok(core.clone()),
        Desktop {
            prompt: prompt.clone(),
            can_confirm: true,
            pickers: Arc::new(NoPickers),
            links: Arc::new(NoBrowser),
        },
    );
    Fixture {
        shell,
        core,
        prompt,
        opener,
        _data: data,
    }
}

async fn call(shell: &Shell, command: &str, args: Value) -> Value {
    match ipc::dispatch(shell, command, args).await {
        Ok(value) => value,
        Err(error) => panic!("{command} failed: {error:?}"),
    }
}

async fn refused(shell: &Shell, command: &str, args: Value) -> ApiError {
    match ipc::dispatch(shell, command, args).await {
        Err(DispatchError::Failed(error)) => error,
        other => panic!("{command} should have been refused, got {other:?}"),
    }
}

/// The cable box built, sliced and proposed: the project id and the pending job.
async fn pending_job(shell: &Shell) -> (String, Value) {
    let project = call(shell, "project_create", json!({"name": "Cable box"})).await;
    let project_id = project["id"].as_str().unwrap().to_owned();
    call(
        shell,
        "model_build",
        json!({"projectId": project_id, "code": support::cable_box_code(), "params": {}, "note": "test"}),
    )
    .await;
    let sliced = call(
        shell,
        "slice_run",
        json!({"projectId": project_id, "profile": "demo-0.20mm", "plate": null}),
    )
    .await;
    assert_eq!(sliced["summary"]["status"], "ok", "{sliced}");
    let job = call(
        shell,
        "print_propose",
        json!({"projectId": project_id, "note": ""}),
    )
    .await;
    (project_id, job)
}

fn sha256(path: &Path) -> String {
    hotend_core::import::sha256_file(path).unwrap().to_string()
}

/// The sliced file of a job: the file under the data folder with the job's hash, outside
/// the hand-off folder.
fn sliced_file(data: &Path, sha: &str) -> PathBuf {
    fn walk(dir: &Path, sha: &str, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "handoff") {
                    walk(&path, sha, found);
                }
            } else if path.to_string_lossy().ends_with(".gcode.3mf") && sha256(&path) == sha {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(data, sha, &mut found);
    found
        .into_iter()
        .next()
        .expect("the sliced file is in the data folder")
}

const CONNECT: fn() -> Value = || json!({"kind": "bambu_connect"});

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_confirmed_hand_off_places_the_checked_file_and_asks_the_opener_once() {
    let fx = fixture().await;
    let shell = &fx.shell;
    let (_project, job) = pending_job(shell).await;
    let job_id = job["id"].as_str().unwrap().to_owned();
    let sha = job["slice_sha256"].as_str().unwrap().to_owned();

    // The review names the folder and what opens the link.
    let review = call(
        shell,
        "print_review",
        json!({"jobId": job_id, "delivery": CONNECT()}),
    )
    .await;
    let folder = fx.core.data_dir().join("handoff").canonicalize().unwrap();
    assert_eq!(review["handoff"]["folder"], folder.to_str().unwrap());
    assert_eq!(review["handoff"]["opens"], false);
    assert_eq!(review["handoff"]["windows_folder"], Value::Null);

    // "Cancel" leaves the job pending and asks the opener nothing.
    fx.prompt.answers.lock().unwrap().push(Answer::Cancel);
    let cancelled = call(
        shell,
        "print_confirm",
        json!({"jobId": job_id, "delivery": CONNECT()}),
    )
    .await;
    assert_eq!(cancelled["confirmed"], false);
    assert!(fx.opener.asked().is_empty());

    fx.prompt.answers.lock().unwrap().push(Answer::Confirm);
    let confirmed = call(
        shell,
        "print_confirm",
        json!({"jobId": job_id, "delivery": CONNECT()}),
    )
    .await;
    assert_eq!(confirmed["confirmed"], true);
    let handed = &confirmed["job"];
    assert_eq!(handed["status"], "handed_off", "{handed}");
    let detail = &handed["detail"];

    // The dialog said what the code does: it saves, it opens nothing, and where.
    let shown = fx.prompt.shown.lock().unwrap().last().cloned().unwrap();
    assert_eq!(shown.title, "Save this file for Bambu Connect?");
    assert_eq!(shown.confirm_label, "Save");
    assert!(
        shown
            .body
            .contains(&format!("\nFolder: {}\n", folder.display())),
        "{}",
        shown.body
    );
    assert!(shown.body.contains("It does not open Bambu Connect"));

    // The placed file is the checked file, byte for byte, inside the folder.
    let placed = PathBuf::from(detail["path"].as_str().unwrap());
    assert_eq!(placed.parent(), Some(folder.as_path()));
    assert_eq!(sha256(&placed), sha);
    assert_eq!(
        std::fs::read(&placed).unwrap(),
        std::fs::read(sliced_file(fx.core.data_dir(), &sha)).unwrap()
    );

    // The opener was asked once, for exactly the stored link, which names that file.
    let link = detail["link"].as_str().unwrap();
    assert_eq!(fx.opener.asked(), [link.to_owned()]);
    let encoded = placed.to_str().unwrap().replace('/', "%2F");
    assert!(
        link.starts_with(&format!("bambu-connect://import-file?path={encoded}&name=")),
        "{link}"
    );
    assert!(link.ends_with("&version=1.0.0"), "{link}");
    assert_eq!(detail["opened"], false);
    assert_eq!(detail["opener"], "stand-in opener: nothing was opened");
    assert_eq!(detail["windows_path"], Value::Null);
    let message = detail["message"].as_str().unwrap();
    assert!(
        message.contains("Bambu Connect was not opened"),
        "{message}"
    );
    assert!(message.contains("press Print there"), "{message}");

    // A delivered job is not delivered again.
    let again = refused(
        shell,
        "print_confirm",
        json!({"jobId": job_id, "delivery": CONNECT()}),
    )
    .await;
    assert_eq!(serde_json::to_value(&again).unwrap()["kind"], "print");
    assert_eq!(fx.opener.asked().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_changed_after_the_check_never_reaches_the_opener() {
    let fx = fixture().await;
    let shell = &fx.shell;
    let (project_id, job) = pending_job(shell).await;
    let job_id = job["id"].as_str().unwrap().to_owned();
    let sha = job["slice_sha256"].as_str().unwrap().to_owned();
    let sliced = sliced_file(fx.core.data_dir(), &sha);
    let original = std::fs::read(&sliced).unwrap();

    // Changed between the check and the confirmation: the core checks the file again in
    // the review it makes for the prompt, which refuses it before anything is asked.
    let mut changed = original.clone();
    changed.extend_from_slice(b"tampered");
    std::fs::write(&sliced, &changed).unwrap();
    let error = refused(
        shell,
        "print_confirm",
        json!({"jobId": job_id, "delivery": CONNECT()}),
    )
    .await;
    assert_eq!(serde_json::to_value(&error).unwrap()["kind"], "print");
    assert!(fx.prompt.shown.lock().unwrap().is_empty());
    assert!(fx.opener.asked().is_empty());

    // A file that is not a sliced archive at all fails the check: nothing is shown,
    // nothing placed, nothing opened.
    std::fs::write(&sliced, b"not a sliced file").unwrap();
    refused(
        shell,
        "print_confirm",
        json!({"jobId": job_id, "delivery": CONNECT()}),
    )
    .await;
    assert!(fx.prompt.shown.lock().unwrap().is_empty());
    assert!(fx.opener.asked().is_empty());
    let handoff = fx.core.data_dir().join("handoff");
    let placed = std::fs::read_dir(&handoff)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(placed, 0, "nothing was placed");

    // The core alone: a job that was never reviewed is not confirmed, whatever it is
    // shown.
    std::fs::write(&sliced, &original).unwrap();
    let jobs = call(shell, "print_jobs", json!({"projectId": project_id})).await;
    assert!(
        jobs.as_array()
            .unwrap()
            .iter()
            .all(|job| job["status"] != "handed_off"),
        "{jobs}"
    );
    assert!(fx.opener.asked().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn under_wsl_a_hand_off_needs_a_windows_drive_folder() {
    let fx = fixture().await;
    let shell = &fx.shell;
    fx.core.set_handoff_host(HostKind::Wsl);
    let (_project, job) = pending_job(shell).await;
    let job_id = job["id"].as_str().unwrap().to_owned();

    // The way is listed, not available, and says why.
    let printers = call(shell, "printers_list", json!({})).await;
    let connect = printers[0]["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|delivery| delivery["kind"] == "bambu_connect")
        .unwrap()
        .clone();
    assert_eq!(connect["enabled"], false);
    let reason = connect["disabled_reason"].as_str().unwrap();
    assert!(
        reason.contains("Windows drive") && reason.contains("Settings"),
        "{reason}"
    );

    // No folder chosen: refused before any prompt, with what to do.
    let error = refused(
        shell,
        "print_confirm",
        json!({"jobId": job_id, "delivery": CONNECT()}),
    )
    .await;
    let error = serde_json::to_value(&error).unwrap();
    assert_eq!(error["code"], "handoff_folder", "{error}");
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains("Windows drive") && message.contains("in Settings"),
        "{message}"
    );

    // A folder that is not on a Windows drive: refused the same way.
    let elsewhere = tempfile::tempdir().unwrap();
    let mut settings = fx.core.settings_get().await.unwrap();
    settings.handoff_folder = Some(elsewhere.path().to_path_buf());
    fx.core.settings_set(settings).await.unwrap();
    let error = refused(
        shell,
        "print_review",
        json!({"jobId": job_id, "delivery": CONNECT()}),
    )
    .await;
    let error = serde_json::to_value(&error).unwrap();
    assert_eq!(error["code"], "handoff_folder", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("/mnt/<drive letter>/"),
        "{error}"
    );
    assert!(fx.prompt.shown.lock().unwrap().is_empty());
    assert!(fx.opener.asked().is_empty());
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}
