//! The command layer on a real core, called the way the window calls it: by command name
//! with JSON arguments (`ipc::dispatch`, the same table the Tauri commands are generated
//! from). What it proves about `print_confirm`:
//!
//! - nothing the window can send confirms a print: the command takes a job and a delivery
//!   and nothing else, and it answers only after the native prompt did;
//! - "Cancel" in the prompt leaves the job pending;
//! - only "confirm" in the prompt starts the job, with what the prompt showed;
//! - while the prompt is open the window can neither replace the review nor ask twice;
//! - a shell that cannot draw the prompt refuses;
//! - the answer carries no capability and no token.
//!
//! It proves the same of `print_project_confirm`, which hands over an editable Bambu Studio
//! project: no argument or other command gets around the prompt, and without a prompt (or
//! with another one open) nothing is reviewed or handed over.
//!
//! It also proves that a path the person did not pick is refused (a hand-off folder in the
//! settings included), that no command gives a provider key back, and that links are
//! opened by name only.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test stops at the first surprise"
)]

mod support;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use hotend_core::{ApiError, Core};
use hotend_desktop::confirm::{Answer, ConfirmationPrompt, ConfirmationText};
use hotend_desktop::grants::Pickers;
use hotend_desktop::ipc::{self, DispatchError};
use hotend_desktop::links::LinkOpener;
use hotend_desktop::shell::{Desktop, Shell};
use serde_json::{Value, json};
use url::Url;

const SECRET: &str = "sk-ant-test-0123456789-THIS-MUST-NEVER-COME-BACK";

/// The native prompt of a test: records what it was asked and answers what the test
/// queued. With a gate it first waits, like a dialog that is still open.
#[derive(Default)]
struct ScriptedPrompt {
    asked: Mutex<Vec<ConfirmationText>>,
    answers: Mutex<Vec<Answer>>,
    gate: Mutex<Option<(Sender<()>, Receiver<()>)>>,
}

impl ScriptedPrompt {
    fn will_answer(&self, answer: Answer) {
        self.answers.lock().unwrap().push(answer);
    }

    fn times_asked(&self) -> usize {
        self.asked.lock().unwrap().len()
    }

    fn last(&self) -> ConfirmationText {
        self.asked
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("the prompt was shown")
    }

    /// Makes the next prompt stay open. Returns how the test learns that it opened and
    /// how it closes it.
    fn hold_open(&self) -> (Receiver<()>, Sender<()>) {
        let (opened_tx, opened_rx) = channel();
        let (close_tx, close_rx) = channel();
        *self.gate.lock().unwrap() = Some((opened_tx, close_rx));
        (opened_rx, close_tx)
    }
}

impl ConfirmationPrompt for ScriptedPrompt {
    fn ask(&self, text: &ConfirmationText) -> Answer {
        self.asked.lock().unwrap().push(text.clone());
        let gate = self.gate.lock().unwrap().take();
        if let Some((opened, close)) = gate {
            opened.send(()).unwrap();
            close.recv().unwrap();
        }
        let mut answers = self.answers.lock().unwrap();
        assert!(
            !answers.is_empty(),
            "the test queued an answer for every prompt"
        );
        answers.remove(0)
    }
}

/// The folder picker of a test: answers with the folder the test set.
#[derive(Default)]
struct ScriptedPickers(Mutex<Option<PathBuf>>, Mutex<Vec<String>>);

impl Pickers for ScriptedPickers {
    fn pick_folder(&self, title: &str) -> Option<PathBuf> {
        self.1.lock().unwrap().push(title.to_owned());
        self.0.lock().unwrap().clone()
    }

    fn pick_file(&self, _title: &str) -> Option<PathBuf> {
        None
    }
}

#[derive(Default)]
struct RecordedLinks(Mutex<Vec<String>>);

impl LinkOpener for RecordedLinks {
    fn open(&self, url: &Url) -> Result<(), String> {
        self.0.lock().unwrap().push(url.to_string());
        Ok(())
    }
}

struct Fixture {
    shell: Shell,
    core: Core,
    prompt: Arc<ScriptedPrompt>,
    pickers: Arc<ScriptedPickers>,
    links: Arc<RecordedLinks>,
    _data: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let data = tempfile::tempdir().unwrap();
    let core = support::open_core(data.path()).await;
    let prompt = Arc::new(ScriptedPrompt::default());
    let pickers = Arc::new(ScriptedPickers::default());
    let links = Arc::new(RecordedLinks::default());
    let shell = Shell::new(
        Ok(core.clone()),
        Desktop {
            prompt: prompt.clone(),
            can_confirm: true,
            pickers: pickers.clone(),
            links: links.clone(),
        },
    );
    Fixture {
        shell,
        core,
        prompt,
        pickers,
        links,
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
        other => panic!("{command} should have been refused by the shell, got {other:?}"),
    }
}

fn kind(error: &ApiError) -> String {
    serde_json::to_value(error).unwrap()["kind"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Every key of every object in `value`, however deep.
fn keys(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                into.push(key.clone());
                keys(inner, into);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| keys(item, into)),
        _ => {}
    }
}

/// A project with the cable box built, sliced and proposed for printing. Returns the
/// project id and the pending job.
async fn pending_job(shell: &Shell) -> (String, Value) {
    let project = call(shell, "project_create", json!({"name": "Cable box"})).await;
    let project_id = project["id"].as_str().unwrap().to_owned();
    let built = call(
        shell,
        "model_build",
        json!({"projectId": project_id, "code": support::cable_box_code(), "params": {}, "note": "test"}),
    )
    .await;
    assert_eq!(built["version"]["number"], 1, "{built}");
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
        json!({"projectId": project_id, "note": "from the test"}),
    )
    .await;
    assert_eq!(job["status"], "pending");
    (project_id, job)
}

async fn status(shell: &Shell, job_id: &str) -> String {
    call(shell, "print_status", json!({"jobId": job_id})).await["status"]
        .as_str()
        .unwrap()
        .to_owned()
}

const SIMULATED: fn() -> Value = || json!({"kind": "simulated_printer"});

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_native_prompt_confirms_a_print() {
    let fx = fixture().await;
    let shell = &fx.shell;
    let (_project, job) = pending_job(shell).await;
    let job_id = job["id"].as_str().unwrap().to_owned();
    let sha256 = job["slice_sha256"].as_str().unwrap().to_owned();

    // 1. The command takes the job and the delivery, nothing else. Whatever else the
    //    window sends is refused before anything happens: no prompt, no review.
    for extra in [
        json!({"jobId": job_id, "delivery": SIMULATED(), "sha256Shown": sha256}),
        json!({"jobId": job_id, "delivery": SIMULATED(), "authority": {}}),
        json!({"jobId": job_id, "delivery": SIMULATED(), "confirmed": true}),
        json!({"jobId": job_id, "delivery": SIMULATED(), "token": "anything"}),
        json!({"jobId": job_id}),
        json!([job_id, SIMULATED()]),
    ] {
        match ipc::dispatch(shell, "print_confirm", extra.clone()).await {
            Err(DispatchError::BadArguments { command, .. }) => {
                assert_eq!(command, "print_confirm")
            }
            other => panic!("{extra} should be refused as bad arguments, got {other:?}"),
        }
    }
    assert_eq!(fx.prompt.times_asked(), 0);
    assert_eq!(status(shell, &job_id).await, "pending");

    // 2. There is one command that confirms a print and one that confirms handing over an
    //    editable project (both through the same prompt), and no command under another
    //    name.
    let confirming: Vec<&str> = ipc::command_names()
        .into_iter()
        .filter(|name| {
            name.contains("confirm")
                || name.contains("authority")
                || name.contains("deliver")
                || name.contains("hand_over")
        })
        .collect();
    assert_eq!(confirming, ["print_confirm", "print_project_confirm"]);

    // 3. "Cancel" in the prompt: the job stays pending and the answer says so.
    fx.prompt.will_answer(Answer::Cancel);
    let outcome = call(
        shell,
        "print_confirm",
        json!({"jobId": job_id, "delivery": SIMULATED()}),
    )
    .await;
    assert_eq!(outcome["confirmed"], false);
    assert_eq!(outcome["job"]["status"], "pending");
    assert_eq!(fx.prompt.times_asked(), 1);
    assert_eq!(status(shell, &job_id).await, "pending");

    // What the prompt showed was computed by the core from the sliced file: it names the
    // simulated printer, the project and the file by its hash.
    let shown = fx.prompt.last();
    assert_eq!(shown.title, "Start this print?");
    assert_eq!(shown.confirm_label, "Start print");
    assert!(
        shown
            .body
            .contains("SIMULATED PRINTER. Nothing will be printed."),
        "{}",
        shown.body
    );
    assert!(shown.body.contains("Cable box"), "{}", shown.body);
    // The dialog's own lines end with the file's check code; below them, under one heading,
    // comes the printer layer's review text of the checked file, and nothing after it.
    let (own, checked) = shown
        .body
        .split_once(&format!(
            "\n\n{}\n",
            hotend_desktop::confirm::CHECKED_HEADING
        ))
        .expect("the review text under its heading");
    assert!(
        own.ends_with(&format!(
            "(check code {}…{})",
            &sha256[..6],
            &sha256[sha256.len() - 4..]
        )),
        "{}",
        shown.body
    );
    assert!(
        checked.starts_with("Print file \"plate_1.gcode.3mf\", plate 1,"),
        "{}",
        shown.body
    );
    assert!(!checked.contains(hotend_desktop::confirm::CHECKED_HEADING));

    // The dialog and the review the window shows are one job in the same words. The window
    // writes the review's seconds to the nearest minute and, of a stand-in file, "About";
    // it names the nozzle and draws the spool's colour, and it replaces the file check's
    // "no moves" warning by its own sentence about the placeholder.
    let review = call(
        shell,
        "print_review",
        json!({"jobId": job_id, "delivery": SIMULATED()}),
    )
    .await;
    // The stand-in's file claims nothing it does not do: it states no layers and the time
    // it states is what its moves take (none). Its rough estimates are in the slice record
    // the core keeps for that very file, and those are what the dialog and the window show.
    assert!(review["summary"]["layers"].is_null(), "{review}");
    let slice = call(
        shell,
        "slice_get",
        json!({"projectId": review["job"]["project_id"], "version": review["job"]["version"]}),
    )
    .await;
    assert_eq!(slice["summary"]["sha256"], review["summary"]["sha256"]);
    let seconds = slice["summary"]["time_s"].as_f64().unwrap().round() as u64;
    assert!(seconds >= 60, "{slice}");
    let minutes = (seconds + 30) / 60;
    let time = match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    };
    let layers = slice["summary"]["layers"].as_u64().unwrap();
    let grams = slice["summary"]["filament"][0]["grams"].as_f64().unwrap();
    assert_eq!(review["spools"][0]["loaded"], "PLA Basic #FFFFFFFF");
    assert_eq!(review["warnings"][0]["code"], "no_moves", "{review}");
    let lines: Vec<&str> = shown.body.lines().collect();
    let own_end = lines
        .iter()
        .position(|line| *line == hotend_desktop::confirm::CHECKED_HEADING)
        .unwrap()
        - 1;
    assert_eq!(
        lines[4..own_end],
        [
            &format!("Time: about {time}, {layers} layers"),
            &format!("Filament: PLA, {grams:.1} g, main nozzle, AMS 1 slot 1 (PLA Basic, white)"),
            "",
            "This file is a placeholder from the stand-in slicer. It can only run on the \
             simulated printer.",
            "Warning: Hotend assumes a 0.4 mm nozzle: the nozzle size was not set in the \
             settings. Check that the printer has a 0.4 mm nozzle before you confirm",
            "",
            "These numbers are rough estimates from the stand-in slicer.",
            lines[own_end - 1],
        ],
        "{}",
        shown.body
    );

    // The text that was checked is in the dialog word for word and in one piece: the core
    // confirms for what the dialog displayed, and refuses a dialog without it. It names
    // the build plate the settings chose and the whole hash of the file.
    let review = call(
        shell,
        "print_review",
        json!({"jobId": job_id, "delivery": SIMULATED()}),
    )
    .await;
    let checked = review["review_text"].as_str().unwrap();
    assert!(shown.body.contains(checked), "{}", shown.body);
    assert!(
        checked.contains("Build plate: Textured PEI Plate, as chosen for this job")
            && checked.contains(&format!("SHA-256 of the file: {sha256}.")),
        "{checked}"
    );

    // 4. While the prompt is open the window can neither swap the review behind it nor
    //    open a second prompt: both are `busy`. Closing it with "Cancel" changes nothing.
    let (opened, close) = fx.prompt.hold_open();
    fx.prompt.will_answer(Answer::Cancel);
    let open_prompt = {
        let args = json!({"jobId": job_id, "delivery": SIMULATED()});
        async { call(shell, "print_confirm", args).await }
    };
    let meanwhile = async {
        tokio::task::spawn_blocking(move || opened.recv().unwrap())
            .await
            .unwrap();
        let review = refused(
            shell,
            "print_review",
            json!({"jobId": job_id, "delivery": {"kind": "bambu_connect"}}),
        )
        .await;
        assert_eq!(kind(&review), "busy");
        let second = refused(
            shell,
            "print_confirm",
            json!({"jobId": job_id, "delivery": SIMULATED()}),
        )
        .await;
        assert_eq!(kind(&second), "busy");
        assert_eq!(status(shell, &job_id).await, "pending");
        close.send(()).unwrap();
    };
    let (outcome, ()) = tokio::join!(open_prompt, meanwhile);
    assert_eq!(outcome["confirmed"], false);
    assert_eq!(fx.prompt.times_asked(), 2);
    assert_eq!(status(shell, &job_id).await, "pending");

    // 5. A shell that cannot draw the prompt (the development bridge without its test
    //    switch) refuses, and its prompt is never asked.
    let unused = Arc::new(ScriptedPrompt::default());
    let headless = Shell::new(
        Ok(fx.core.clone()),
        Desktop {
            prompt: unused.clone(),
            can_confirm: false,
            pickers: fx.pickers.clone(),
            links: fx.links.clone(),
        },
    );
    let error = refused(
        &headless,
        "print_confirm",
        json!({"jobId": job_id, "delivery": SIMULATED()}),
    )
    .await;
    assert_eq!(kind(&error), "not_configured");
    assert_eq!(unused.times_asked(), 0);
    assert_eq!(status(shell, &job_id).await, "pending");

    // 6. A review alone starts nothing, however often the window asks for one.
    for _ in 0..2 {
        let review = call(
            shell,
            "print_review",
            json!({"jobId": job_id, "delivery": SIMULATED()}),
        )
        .await;
        assert_eq!(review["summary"]["sha256"], sha256);
    }
    assert_eq!(status(shell, &job_id).await, "pending");
    assert_eq!(fx.prompt.times_asked(), 2);

    // 7. "Confirm" in the prompt: the job leaves `pending`. The answer holds the job and
    //    whether it was confirmed, and no capability or token in any field.
    fx.prompt.will_answer(Answer::Confirm);
    let outcome = call(
        shell,
        "print_confirm",
        json!({"jobId": job_id, "delivery": SIMULATED()}),
    )
    .await;
    assert_eq!(outcome["confirmed"], true);
    assert_ne!(outcome["job"]["status"], "pending");
    assert_eq!(outcome["job"]["id"], job_id);
    let mut top: Vec<&String> = outcome.as_object().unwrap().keys().collect();
    top.sort();
    assert_eq!(top, ["confirmed", "job"]);
    let mut all = Vec::new();
    keys(&outcome, &mut all);
    for key in &all {
        let key = key.to_lowercase();
        assert!(
            !["token", "authority", "capability", "secret"]
                .iter()
                .any(|word| key.contains(word)),
            "the answer has a field `{key}`"
        );
    }
    assert_eq!(fx.prompt.times_asked(), 3);
    assert_ne!(status(shell, &job_id).await, "pending");

    // 8. A job that is no longer pending is refused before any prompt is shown.
    let error = refused(
        shell,
        "print_confirm",
        json!({"jobId": job_id, "delivery": SIMULATED()}),
    )
    .await;
    assert_eq!(kind(&error), "print", "{error:?}");
    assert_eq!(fx.prompt.times_asked(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paths_keys_and_links_are_the_shells_not_the_windows() {
    let fx = fixture().await;
    let shell = &fx.shell;
    let project = call(shell, "project_create", json!({"name": "Cable box"})).await;
    let project_id = project["id"].as_str().unwrap();
    let built = call(
        shell,
        "model_build",
        json!({"projectId": project_id, "code": support::cable_box_code(), "params": {}, "note": "test"}),
    )
    .await;
    assert_eq!(built["version"]["number"], 1, "{built}");

    // A folder the person did not pick is refused, wherever the window names one.
    let outside = tempfile::tempdir().unwrap();
    let folder = outside.path().join("exports");
    std::fs::create_dir_all(&folder).unwrap();
    let export = json!({
        "projectId": project_id, "version": null, "formats": ["step"], "parts": null,
        "destination": folder,
    });
    let error = refused(shell, "export_run", export.clone()).await;
    assert_eq!(kind(&error), "invalid_input");
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);

    // The picker cancelled: nothing is granted.
    let picked = call(shell, "folder_pick", json!({"title": "Export to"})).await;
    assert_eq!(picked, Value::Null);
    assert_eq!(
        kind(&refused(shell, "export_run", export.clone()).await),
        "invalid_input"
    );

    // The person picked the folder in the system's picker: now it is accepted, and only
    // that folder, not one inside or above it.
    *fx.pickers.0.lock().unwrap() = Some(folder.clone());
    let picked = call(shell, "folder_pick", json!({"title": "Export to"})).await;
    assert_eq!(picked, json!(folder));
    let files = call(shell, "export_run", export).await;
    assert_eq!(files.as_array().unwrap().len(), 1, "{files}");
    assert!(std::fs::read_dir(&folder).unwrap().count() >= 1);
    for other in [outside.path().to_path_buf(), folder.join("deeper")] {
        let error = refused(
            shell,
            "export_run",
            json!({
                "projectId": project_id, "version": null, "formats": ["step"], "parts": null,
                "destination": other,
            }),
        )
        .await;
        assert_eq!(kind(&error), "invalid_input");
    }

    // The folder was picked to write files into. It is not accepted as a slicer program,
    // by the command that chooses one or through the settings.
    let error = refused(
        shell,
        "slicer_choose",
        json!({"kind": "bambu-studio", "executable": folder}),
    )
    .await;
    assert_eq!(kind(&error), "invalid_input", "{error:?}");
    let mut settings = call(shell, "settings_get", json!({})).await;
    settings["slicer_executables"]["bambu-studio"] = json!(folder);
    let error = refused(shell, "settings_set", json!({"settings": settings})).await;
    assert_eq!(kind(&error), "invalid_input", "{error:?}");

    // The person's consent to web research is not the window's to give: a page that sends
    // it, or other research limits, changes nothing; and settings saved without the field
    // keep what the core has.
    let before = call(shell, "settings_get", json!({})).await;
    assert!(
        before["research"]["acknowledged_at_ms"].is_null(),
        "{before}"
    );
    let mut forged = before.clone();
    forged["research"]["acknowledged_at_ms"] = json!(1_700_000_000_000_i64);
    forged["research"]["searches_per_turn"] = json!(1);
    call(shell, "settings_set", json!({"settings": forged})).await;
    let after = call(shell, "settings_get", json!({})).await;
    assert_eq!(after["research"], before["research"], "{after}");
    let mut without = before.clone();
    without.as_object_mut().unwrap().remove("research");
    call(shell, "settings_set", json!({"settings": without})).await;
    let after = call(shell, "settings_get", json!({})).await;
    assert_eq!(after["research"], before["research"], "{after}");

    // The words on the picker are Hotend's: a title the interface does not have is not
    // shown, one it has is.
    call(
        shell,
        "folder_pick",
        json!({"title": "Choose a folder for the exported files"}),
    )
    .await;
    assert_eq!(
        *fx.pickers.1.lock().unwrap(),
        [
            "Choose a folder",
            "Choose a folder",
            "Choose a folder for the exported files"
        ]
    );

    // A provider key goes in and never comes back: not from the command that stores it,
    // not from any command that reads state.
    let stored = call(
        shell,
        "provider_key_set",
        json!({"kind": "anthropic", "key": SECRET}),
    )
    .await;
    assert_eq!(stored, Value::Null);
    assert_eq!(
        call(shell, "provider_key_status", json!({"kind": "anthropic"})).await,
        json!(true)
    );
    assert!(
        !ipc::command_names()
            .iter()
            .any(|name| name.starts_with("provider_key")
                && ![
                    "provider_key_set",
                    "provider_key_delete",
                    "provider_key_status"
                ]
                .contains(name)),
        "no command reads a key"
    );
    let reads = [
        ("settings_get", json!({})),
        ("projects_list", json!({})),
        ("project_get", json!({"projectId": project_id})),
        ("versions_list", json!({"projectId": project_id})),
        ("printers_list", json!({})),
        ("slicers_list", json!({})),
        ("calibration_get", json!({})),
        ("print_jobs", json!({"projectId": project_id})),
        ("audit_log", json!({"projectId": null, "limit": 200})),
        ("doctor_report", json!({"skipSelfTests": true})),
        ("provider_key_status", json!({"kind": "anthropic"})),
    ];
    for (command, args) in reads {
        let answer = call(shell, command, args).await.to_string();
        assert!(!answer.contains(SECRET), "{command} gave the key back");
        assert!(
            !answer.contains(&SECRET[..16]),
            "{command} gave part of the key back"
        );
    }
    assert_eq!(
        call(shell, "provider_key_delete", json!({"kind": "anthropic"})).await,
        Value::Null
    );
    assert_eq!(
        call(shell, "provider_key_status", json!({"kind": "anthropic"})).await,
        json!(false)
    );

    // Links are opened by name. The window cannot hand the shell an address.
    assert_eq!(
        call(shell, "open_link", json!({"link": "anthropic_keys"})).await,
        Value::Null
    );
    assert_eq!(
        *fx.links.0.lock().unwrap(),
        ["https://console.anthropic.com/settings/keys"]
    );
    for args in [
        json!({"link": "https://example.com/"}),
        json!({"link": "anthropic_keys", "url": "https://example.com/"}),
        json!({"url": "https://example.com/"}),
    ] {
        assert!(
            matches!(
                ipc::dispatch(shell, "open_link", args.clone()).await,
                Err(DispatchError::BadArguments { .. })
            ),
            "{args}"
        );
    }
    assert_eq!(fx.links.0.lock().unwrap().len(), 1);

    // Nothing but the table is a command.
    for name in [
        "print_confirm_now",
        "shell_execute",
        "plugin:shell|execute",
        "fs_read",
        "",
    ] {
        assert!(
            matches!(
                ipc::dispatch(shell, name, json!({})).await,
                Err(DispatchError::UnknownCommand(_))
            ),
            "{name}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settings_set_refuses_a_hand_off_folder_the_person_did_not_pick() {
    let fx = fixture().await;
    let shell = &fx.shell;
    let chosen = tempfile::tempdir().unwrap();
    let folder = chosen.path().to_path_buf();
    let mut settings = call(shell, "settings_get", json!({})).await;
    assert_eq!(settings["handoff_folder"], Value::Null, "{settings}");

    // Named by the page alone: refused, and the settings keep no folder.
    settings["handoff_folder"] = json!(folder);
    let error = refused(shell, "settings_set", json!({"settings": settings.clone()})).await;
    assert_eq!(kind(&error), "invalid_input", "{error:?}");
    assert_eq!(fx.core.settings_get().await.unwrap().handoff_folder, None);

    // Picked in the folder dialog under another title (an export's), it is a folder for
    // files, not the hand-off folder: refused.
    *fx.pickers.0.lock().unwrap() = Some(folder.clone());
    let picked = call(
        shell,
        "folder_pick",
        json!({"title": "Choose a folder for the exported files"}),
    )
    .await;
    assert_eq!(picked, json!(folder));
    let error = refused(shell, "settings_set", json!({"settings": settings.clone()})).await;
    assert_eq!(kind(&error), "invalid_input", "{error:?}");
    assert!(
        format!("{error:?}").contains("hand-off folder"),
        "{error:?}"
    );
    assert_eq!(fx.core.settings_get().await.unwrap().handoff_folder, None);

    // Only the folder dialog under the hand-off picker's own title grants it.
    *fx.pickers.0.lock().unwrap() = Some(folder.clone());
    let picked = call(
        shell,
        "folder_pick",
        json!({"title": "Choose the Bambu Connect hand-off folder"}),
    )
    .await;
    assert_eq!(picked, json!(folder));
    assert_eq!(
        fx.pickers.1.lock().unwrap().last().map(String::as_str),
        Some("Choose the Bambu Connect hand-off folder")
    );
    call(shell, "settings_set", json!({"settings": settings.clone()})).await;
    assert_eq!(
        fx.core.settings_get().await.unwrap().handoff_folder,
        Some(folder.clone())
    );

    // The settings written back as they are keep the folder, also from a shell of another
    // session that never picked it.
    let other_session = Shell::new(
        Ok(fx.core.clone()),
        Desktop {
            prompt: fx.prompt.clone(),
            can_confirm: true,
            pickers: fx.pickers.clone(),
            links: fx.links.clone(),
        },
    );
    let current = call(&other_session, "settings_get", json!({})).await;
    call(&other_session, "settings_set", json!({"settings": current})).await;

    // Another folder the person did not pick: refused, the chosen one stays.
    let elsewhere = tempfile::tempdir().unwrap();
    let mut changed = settings.clone();
    changed["handoff_folder"] = json!(elsewhere.path());
    let error = refused(shell, "settings_set", json!({"settings": changed})).await;
    assert_eq!(kind(&error), "invalid_input", "{error:?}");
    assert_eq!(
        fx.core.settings_get().await.unwrap().handoff_folder,
        Some(folder.clone())
    );

    // Clearing it needs no picker.
    let mut cleared = settings.clone();
    cleared["handoff_folder"] = Value::Null;
    call(&other_session, "settings_set", json!({"settings": cleared})).await;
    assert_eq!(fx.core.settings_get().await.unwrap().handoff_folder, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_snapshot_is_exported_only_into_a_folder_picked_for_it() {
    let fx = fixture().await;
    let shell = &fx.shell;
    let project = call(shell, "project_create", json!({"name": "Cable box"})).await;
    let project_id = project["id"].as_str().unwrap();
    let built = call(
        shell,
        "model_build",
        json!({"projectId": project_id, "code": support::cable_box_code(), "params": {}, "note": "test"}),
    )
    .await;
    assert_eq!(built["version"]["number"], 1, "{built}");

    // Naming, listing: the current version by default.
    assert_eq!(
        call(shell, "snapshots_list", json!({"projectId": project_id})).await,
        json!([])
    );
    let snapshot = call(
        shell,
        "snapshot_create",
        json!({"projectId": project_id, "name": "First box", "version": null, "note": ""}),
    )
    .await;
    assert_eq!(snapshot["name"], "First box");
    assert_eq!(snapshot["version"], 1);
    let listed = call(shell, "snapshots_list", json!({"projectId": project_id})).await;
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    // A command takes its own arguments and nothing else.
    for extra in [
        json!({"projectId": project_id, "name": "First box", "folder": "/tmp", "granted": true}),
        json!({"projectId": project_id, "name": "First box"}),
        json!([project_id, "First box", "/tmp"]),
    ] {
        assert!(
            matches!(
                ipc::dispatch(shell, "snapshot_export", extra.clone()).await,
                Err(DispatchError::BadArguments { .. })
            ),
            "{extra}"
        );
    }

    // A folder nobody picked is refused, and nothing is written into it.
    let outside = tempfile::tempdir().unwrap();
    let folder = outside.path().join("snapshots");
    std::fs::create_dir_all(&folder).unwrap();
    let export = json!({"projectId": project_id, "name": "First box", "folder": folder});
    let error = refused(shell, "snapshot_export", export.clone()).await;
    assert_eq!(kind(&error), "invalid_input", "{error:?}");
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);

    // Picked under another title (an export's, the hand-off folder's): a folder for other
    // files, not for snapshot files. Refused.
    *fx.pickers.0.lock().unwrap() = Some(folder.clone());
    for title in [
        "Choose a folder for the exported files",
        "Choose the Bambu Connect hand-off folder",
        "Save snapshot files ",
        "Choose a folder",
    ] {
        let picked = call(shell, "folder_pick", json!({"title": title})).await;
        assert_eq!(picked, json!(folder));
        let error = refused(shell, "snapshot_export", export.clone()).await;
        assert_eq!(kind(&error), "invalid_input", "{title}: {error:?}");
        assert!(
            format!("{error:?}").contains("was not chosen as the folder for snapshot files"),
            "{error:?}"
        );
    }
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);

    // Picked under the snapshot export picker's own title: accepted, that folder only.
    let picked = call(
        shell,
        "folder_pick",
        json!({"title": "Save snapshot files"}),
    )
    .await;
    assert_eq!(picked, json!(folder));
    assert_eq!(
        fx.pickers.1.lock().unwrap().last().map(String::as_str),
        Some("Save snapshot files")
    );
    let exported = call(shell, "snapshot_export", export).await;
    let written = std::path::PathBuf::from(exported["folder"].as_str().unwrap());
    assert!(written.starts_with(&folder), "{exported}");
    let kinds: Vec<&str> = exported["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"manifest"), "{exported}");
    assert!(exported["printed"].is_null(), "{exported}");
    let deeper = folder.join("deeper");
    std::fs::create_dir_all(&deeper).unwrap();
    for other in [outside.path().to_path_buf(), deeper] {
        let error = refused(
            shell,
            "snapshot_export",
            json!({"projectId": project_id, "name": "First box", "folder": other}),
        )
        .await;
        assert_eq!(kind(&error), "invalid_input", "{error:?}");
    }
    // A folder picked for snapshot files only is no folder for an export.
    let only_snapshots = outside.path().join("only-snapshots");
    std::fs::create_dir_all(&only_snapshots).unwrap();
    *fx.pickers.0.lock().unwrap() = Some(only_snapshots.clone());
    call(
        shell,
        "folder_pick",
        json!({"title": "Save snapshot files"}),
    )
    .await;
    let error = refused(
        shell,
        "export_run",
        json!({
            "projectId": project_id, "version": null, "formats": ["step"], "parts": null,
            "destination": only_snapshots,
        }),
    )
    .await;
    assert_eq!(kind(&error), "invalid_input", "{error:?}");
    assert_eq!(std::fs::read_dir(&only_snapshots).unwrap().count(), 0);

    // Going back to it and removing the name.
    call(
        shell,
        "parameter_set",
        json!({"projectId": project_id, "name": "wall", "value": 2.5}),
    )
    .await;
    let restored = call(
        shell,
        "snapshot_restore",
        json!({"projectId": project_id, "name": "First box"}),
    )
    .await;
    assert_eq!(restored["number"], 1, "{restored}");
    let removed = call(
        shell,
        "snapshot_delete",
        json!({"projectId": project_id, "name": "First box"}),
    )
    .await;
    assert_eq!(removed["name"], "First box");
    assert_eq!(
        call(shell, "snapshots_list", json!({"projectId": project_id})).await,
        json!([])
    );
    let error = refused(
        shell,
        "snapshot_restore",
        json!({"projectId": project_id, "name": "First box"}),
    )
    .await;
    assert_eq!(kind(&error), "not_found", "{error:?}");
}

/// The printer's access code of the tests. Never a real one.
const ACCESS_CODE: &str = "Zq12345678";

/// Whether the environment of this test run gives an access code (it takes precedence
/// over a stored one).
fn access_code_in_environment() -> bool {
    std::env::var_os(hotend_core::printer_lan::ACCESS_CODE_ENV).is_some()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_printer_status_commands_never_give_the_code_back_and_never_take_a_fingerprint() {
    // A configuration folder of its own: no certificate authorities, so nothing is ever
    // connected to without a confirmed certificate. No printer address is ever set while a
    // certificate check could reach the network.
    let data = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let core = support::open_core_with_config(data.path(), config.path()).await;
    let prompt = Arc::new(ScriptedPrompt::default());
    let shell = Shell::new(
        Ok(core.clone()),
        Desktop {
            prompt: prompt.clone(),
            can_confirm: true,
            pickers: Arc::new(ScriptedPickers::default()),
            links: Arc::new(RecordedLinks::default()),
        },
    );
    let shell = &shell;

    let setup = call(shell, "printer_lan_get", json!({})).await;
    assert_eq!(setup["address"], Value::Null, "{setup}");
    assert_eq!(setup["trust"], "unconfirmed");
    assert_eq!(setup["watching"], false);
    if !access_code_in_environment() {
        assert_eq!(setup["access_code"], "missing");
        assert_eq!(setup["access_code_present"], false);
    }

    // The window cannot pin a certificate: no command takes a fingerprint, and the core's
    // confirmation is no command at all.
    for name in ["printer_certificate_confirm", "printer_certificate_probe"] {
        assert!(
            matches!(
                ipc::dispatch(shell, name, json!({"sha256": "ab".repeat(32)})).await,
                Err(DispatchError::UnknownCommand(_))
            ),
            "{name}"
        );
    }
    let certificate: Vec<&str> = ipc::command_names()
        .into_iter()
        .filter(|name| name.contains("certificate") || name.contains("pin"))
        .collect();
    assert_eq!(
        certificate,
        ["printer_certificate_check", "printer_certificate_forget"]
    );
    for extra in [
        json!({"sha256": "ab".repeat(32)}),
        json!({"confirmed": true}),
        json!({"fingerprint": "ab"}),
    ] {
        assert!(
            matches!(
                ipc::dispatch(shell, "printer_certificate_check", extra.clone()).await,
                Err(DispatchError::BadArguments { .. })
            ),
            "{extra}"
        );
    }
    // Without an address there is nothing to look at: refused before any dialog.
    let error = refused(shell, "printer_certificate_check", json!({})).await;
    assert_eq!(kind(&error), "not_configured", "{error:?}");
    assert_eq!(prompt.times_asked(), 0);
    // Without an address nothing is followed.
    let error = refused(shell, "printer_watch_start", json!({})).await;
    assert_eq!(kind(&error), "not_configured", "{error:?}");

    // The address: a private-range IPv4 address and a serial, nothing else.
    for (ip, serial) in [
        ("8.8.8.8", "01P00A000000001"),
        ("printer.local", "01P00A000000001"),
        ("192.168.1.50", "x"),
    ] {
        let error = refused(
            shell,
            "printer_lan_set",
            json!({"ip": ip, "serial": serial}),
        )
        .await;
        assert_eq!(kind(&error), "invalid_input", "{ip} {serial}: {error:?}");
    }
    let setup = call(
        shell,
        "printer_lan_set",
        json!({"ip": "192.168.1.50", "serial": "01P00A000000001"}),
    )
    .await;
    assert_eq!(
        setup["address"],
        json!({"ip": "192.168.1.50", "serial": "01P00A000000001"})
    );

    // A shell that cannot draw the dialog refuses before the printer is looked at.
    let unused = Arc::new(ScriptedPrompt::default());
    let headless = Shell::new(
        Ok(core.clone()),
        Desktop {
            prompt: unused.clone(),
            can_confirm: false,
            pickers: Arc::new(ScriptedPickers::default()),
            links: Arc::new(RecordedLinks::default()),
        },
    );
    let error = refused(&headless, "printer_certificate_check", json!({})).await;
    assert_eq!(kind(&error), "not_configured", "{error:?}");
    assert_eq!(unused.times_asked(), 0);

    // Without a confirmed certificate nothing is connected to.
    let error = refused(shell, "printer_watch_start", json!({})).await;
    assert_eq!(kind(&error), "not_configured", "{error:?}");
    assert!(
        format!("{error:?}").contains("Nothing was connected to"),
        "{error:?}"
    );
    assert_eq!(
        call(shell, "printer_watch_latest", json!({})).await,
        Value::Null
    );
    assert_eq!(
        call(shell, "printer_watch_stop", json!({})).await,
        json!(false)
    );

    // The access code goes in and never comes back; the setup says only whether it is there.
    assert_eq!(
        call(shell, "printer_lan_key_set", json!({"code": ACCESS_CODE})).await,
        Value::Null
    );
    let setup = call(shell, "printer_lan_get", json!({})).await;
    assert_eq!(setup["access_code_present"], true, "{setup}");
    if !access_code_in_environment() {
        assert_eq!(setup["access_code"], "secret_store");
    }
    let reads = [
        ("printer_lan_get", json!({})),
        ("printer_watch_latest", json!({})),
        ("settings_get", json!({})),
        ("audit_log", json!({"projectId": null, "limit": 500})),
        ("doctor_report", json!({"skipSelfTests": true})),
    ];
    for (command, args) in reads {
        let answer = call(shell, command, args).await.to_string();
        assert!(
            !answer.contains(ACCESS_CODE),
            "{command} gave the code back"
        );
    }
    assert!(
        !ipc::command_names()
            .iter()
            .any(|name| name.starts_with("printer_lan_key")
                && !["printer_lan_key_set", "printer_lan_key_delete"].contains(name)),
        "no command reads the code"
    );
    assert_eq!(
        call(shell, "printer_lan_key_delete", json!({})).await,
        Value::Null
    );
    if !access_code_in_environment() {
        let setup = call(shell, "printer_lan_get", json!({})).await;
        assert_eq!(setup["access_code_present"], false, "{setup}");
    }

    // Forgetting and clearing.
    assert_eq!(
        call(shell, "printer_certificate_forget", json!({})).await,
        Value::Null
    );
    assert_eq!(
        call(shell, "printer_lan_clear", json!({})).await,
        Value::Null
    );
    let setup = call(shell, "printer_lan_get", json!({})).await;
    assert_eq!(setup["address"], Value::Null, "{setup}");
    assert_eq!(setup["pin"], Value::Null, "{setup}");
    assert_eq!(prompt.times_asked(), 0);
}

/// A printer of the tests: its first look returns a fixed certificate and it records every
/// pin. Nothing is connected to.
struct LookedAtPrinter {
    looks: Mutex<u32>,
    pinned: Mutex<Vec<String>>,
    subject: String,
    /// When set, every look records whether this lock was free while it ran.
    watched_lock: Option<Arc<hotend_desktop::confirm::ReviewLock>>,
    lock_free_during_look: Mutex<Vec<bool>>,
}

const FINGERPRINT: &str = "5e1f0c2d9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d";

impl LookedAtPrinter {
    fn new(subject: &str) -> Self {
        Self {
            looks: Mutex::new(0),
            pinned: Mutex::new(Vec::new()),
            subject: subject.to_owned(),
            watched_lock: None,
            lock_free_during_look: Mutex::new(Vec::new()),
        }
    }
}

impl hotend_desktop::confirm::CertificatePinning for LookedAtPrinter {
    async fn probe(&self) -> Result<hotend_core::PrinterCertificate, ApiError> {
        *self.looks.lock().unwrap() += 1;
        if let Some(lock) = &self.watched_lock {
            let free = lock.try_hold().is_ok();
            self.lock_free_during_look.lock().unwrap().push(free);
        }
        Ok(hotend_core::PrinterCertificate {
            ip: "192.168.1.50".to_owned(),
            serial: "01P00A000000001".to_owned(),
            sha256: FINGERPRINT.to_owned(),
            subject: self.subject.clone(),
            issuer: "CN=BBL CA".to_owned(),
            note: hotend_core::printer_lan::SUBJECT_NOTE.to_owned(),
        })
    }

    async fn pin(&self, sha256: &str) -> Result<(), ApiError> {
        self.pinned.lock().unwrap().push(sha256.to_owned());
        Ok(())
    }

    async fn setup(&self) -> Result<hotend_core::PrinterLanSetup, ApiError> {
        let pinned = !self.pinned.lock().unwrap().is_empty();
        Ok(hotend_core::PrinterLanSetup {
            address: None,
            trust: if pinned { "pinned" } else { "unconfirmed" }.to_owned(),
            ca_file: None,
            pin: None,
            access_code: "missing".to_owned(),
            access_code_store: hotend_core::SecretStoreKind::Memory,
            watching: false,
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_certificate_is_pinned_only_after_the_dialogs_yes_and_only_as_it_was_seen() {
    use hotend_desktop::confirm::{ConfirmationPrompt, ReviewLock, confirm_certificate};

    let printer = LookedAtPrinter::new("CN=01P00A000000001");
    let scripted = Arc::new(ScriptedPrompt::default());
    let prompt: Arc<dyn ConfirmationPrompt> = scripted.clone();
    let lock = ReviewLock::default();

    // Nobody to ask: refused before the printer is looked at.
    let error = confirm_certificate(&printer, &prompt, false, &lock)
        .await
        .unwrap_err();
    assert_eq!(kind(&error), "not_configured");
    assert_eq!(*printer.looks.lock().unwrap(), 0);
    assert_eq!(scripted.times_asked(), 0);

    // "Cancel": nothing is stored.
    scripted.will_answer(Answer::Cancel);
    let outcome = confirm_certificate(&printer, &prompt, true, &lock)
        .await
        .unwrap();
    assert!(!outcome.confirmed);
    assert_eq!(outcome.setup.setup.trust, "unconfirmed");
    assert!(printer.pinned.lock().unwrap().is_empty());
    // The dialog showed what the look found: address, serial, the whole fingerprint,
    // subject, issuer and the core's note, as written.
    let shown = scripted.last();
    assert_eq!(shown.title, "Trust this printer's certificate?");
    assert_eq!(shown.confirm_label, "Trust this certificate");
    assert_eq!(shown.cancel_label, "Cancel");
    for line in [
        "Printer: 192.168.1.50",
        "Serial: 01P00A000000001",
        &format!("Fingerprint: {FINGERPRINT}"),
        "Subject: CN=01P00A000000001",
        "Issuer: CN=BBL CA",
    ] {
        assert!(
            shown.body.lines().any(|shown| shown == line),
            "{line}\n{}",
            shown.body
        );
    }
    assert!(shown.body.ends_with(hotend_core::printer_lan::SUBJECT_NOTE));

    // While a dialog is open no second one opens.
    let (opened, close) = scripted.hold_open();
    scripted.will_answer(Answer::Cancel);
    let open = confirm_certificate(&printer, &prompt, true, &lock);
    let meanwhile = async {
        tokio::task::spawn_blocking(move || opened.recv().unwrap())
            .await
            .unwrap();
        let busy = confirm_certificate(&printer, &prompt, true, &lock)
            .await
            .unwrap_err();
        assert_eq!(kind(&busy), "busy");
        close.send(()).unwrap();
    };
    let (outcome, ()) = tokio::join!(open, meanwhile);
    assert!(!outcome.unwrap().confirmed);
    assert!(printer.pinned.lock().unwrap().is_empty());

    // The confirming button: pinned, with the fingerprint of that look and nothing else.
    scripted.will_answer(Answer::Confirm);
    let outcome = confirm_certificate(&printer, &prompt, true, &lock)
        .await
        .unwrap();
    assert!(outcome.confirmed);
    assert_eq!(outcome.setup.setup.trust, "pinned");
    assert_eq!(*printer.pinned.lock().unwrap(), [FINGERPRINT]);
    assert_eq!(*printer.looks.lock().unwrap(), 3);
    assert_eq!(scripted.times_asked(), 3);
    let answer = serde_json::to_value(&outcome).unwrap();
    let mut top: Vec<&String> = answer.as_object().unwrap().keys().collect();
    top.sort();
    assert_eq!(top, ["confirmed", "setup"]);
}

/// The look at the printer (a network handshake) does not hold the confirmations' lock: a
/// print's confirmation is not refused as busy while no dialog is showing yet. The dialog
/// itself still holds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_look_at_the_printer_does_not_hold_other_confirmations() {
    use hotend_desktop::confirm::{ConfirmationPrompt, ReviewLock, confirm_certificate};

    let lock = Arc::new(ReviewLock::default());
    let mut printer = LookedAtPrinter::new("CN=01P00A000000001");
    printer.watched_lock = Some(Arc::clone(&lock));
    let scripted = Arc::new(ScriptedPrompt::default());
    let prompt: Arc<dyn ConfirmationPrompt> = scripted.clone();

    scripted.will_answer(Answer::Cancel);
    let outcome = confirm_certificate(&printer, &prompt, true, &lock)
        .await
        .unwrap();
    assert!(!outcome.confirmed);
    assert_eq!(*printer.lock_free_during_look.lock().unwrap(), [true]);
    assert_eq!(scripted.times_asked(), 1);

    // While another dialog is open it is refused at once, before the printer is looked at.
    let held = lock.try_hold().unwrap();
    let busy = confirm_certificate(&printer, &prompt, true, &lock)
        .await
        .unwrap_err();
    assert_eq!(kind(&busy), "busy");
    drop(held);
    assert_eq!(*printer.looks.lock().unwrap(), 1);
    assert_eq!(scripted.times_asked(), 1);
}

const SEARCH_KEY: &str = "tvly-test-0123456789-THIS-MUST-NEVER-COME-BACK";

/// The research option of `source` in `options`.
fn option<'a>(options: &'a Value, source: &str) -> &'a Value {
    options
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["source"] == source)
        .unwrap_or_else(|| panic!("no option {source} in {options}"))
}

/// The window learns before anything is typed whether a key can be stored, from answers it
/// already asks for (`research_source_options`, `printer_lan_get`); with the file store the
/// search key and the printer's access code are stored in the file and never come back.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_window_is_told_where_keys_go_and_the_file_store_takes_them() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config/hotend/secrets.json");
    let core = support::open_core_with_secrets(
        &dir.path().join("data"),
        &dir.path().join("config"),
        Arc::new(hotend_core::FileSecretStore::at(&file)),
    )
    .await;
    let shell = Shell::new(
        Ok(core.clone()),
        Desktop {
            prompt: Arc::new(ScriptedPrompt::default()),
            can_confirm: true,
            pickers: Arc::new(ScriptedPickers::default()),
            links: Arc::new(RecordedLinks::default()),
        },
    );
    let shell = &shell;
    let tavily_in_environment = std::env::var_os("TAVILY_API_KEY").is_some();

    let options = call(shell, "research_source_options", json!({})).await;
    assert_eq!(option(&options, "tavily")["key_store"], "file", "{options}");
    assert_eq!(
        option(&options, "tavily")["key_from_environment"],
        tavily_in_environment
    );
    let setup = call(shell, "printer_lan_get", json!({})).await;
    assert_eq!(setup["access_code_store"], "file", "{setup}");

    assert_eq!(
        call(
            shell,
            "search_key_set",
            json!({"service": "tavily", "key": SEARCH_KEY})
        )
        .await,
        Value::Null
    );
    assert_eq!(
        call(shell, "printer_lan_key_set", json!({"code": ACCESS_CODE})).await,
        Value::Null
    );
    assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
    let options = call(shell, "research_source_options", json!({})).await;
    assert_eq!(option(&options, "tavily")["key_present"], true, "{options}");
    let setup = call(shell, "printer_lan_get", json!({})).await;
    assert_eq!(setup["access_code_present"], true, "{setup}");
    if !access_code_in_environment() {
        assert_eq!(setup["access_code"], "secret_store", "{setup}");
    }
    for answer in [options.to_string(), setup.to_string()] {
        assert!(!answer.contains(SEARCH_KEY) && !answer.contains(ACCESS_CODE));
    }
    // A second core on the same file (another Hotend process) reads what was stored.
    let again = Arc::new(hotend_core::FileSecretStore::at(&file));
    if !tavily_in_environment {
        use hotend_core::SecretStore;
        assert!(again.is_set("search-key.tavily").unwrap());
    }

    // The health answer the key row reads names the providers whose key the environment
    // supplies over the file (as this run's environment has them), never a key.
    let report = call(shell, "doctor_report", json!({"skipSelfTests": true})).await;
    let listed = report["keys_from_environment"].as_array().unwrap().clone();
    assert_eq!(
        listed.contains(&json!("anthropic")),
        hotend_core::secrets::environment_has("provider-key.anthropic"),
        "{listed:?}"
    );
    assert!(!report.to_string().contains(SEARCH_KEY));

    // The read-only environment store says so before anything is typed, and refuses.
    let env_core = support::open_core_with_secrets(
        &dir.path().join("data-env"),
        &dir.path().join("config"),
        Arc::new(hotend_core::secrets::EnvSecretStore::default()),
    )
    .await;
    let env_shell = Shell::new(
        Ok(env_core.clone()),
        Desktop {
            prompt: Arc::new(ScriptedPrompt::default()),
            can_confirm: true,
            pickers: Arc::new(ScriptedPickers::default()),
            links: Arc::new(RecordedLinks::default()),
        },
    );
    let options = call(&env_shell, "research_source_options", json!({})).await;
    assert_eq!(option(&options, "keenable")["key_store"], "environment");
    let setup = call(&env_shell, "printer_lan_get", json!({})).await;
    assert_eq!(setup["access_code_store"], "environment", "{setup}");
    let error = refused(
        &env_shell,
        "search_key_set",
        json!({"service": "tavily", "key": SEARCH_KEY}),
    )
    .await;
    assert!(format!("{error:?}").contains("read-only"), "{error:?}");
    assert!(!format!("{error:?}").contains(SEARCH_KEY));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn research_is_agreed_to_only_in_the_native_dialog_and_only_for_the_chosen_source() {
    let fx = fixture().await;
    let shell = &fx.shell;
    let agreed_at = |settings: &Value| settings["research"]["acknowledged_at_ms"].clone();

    let options = call(shell, "research_source_options", json!({})).await;
    assert_eq!(options.as_array().unwrap().len(), 4, "{options}");
    assert_eq!(option(&options, "built_in")["chosen"], true);
    for source in ["built_in", "keenable", "tavily", "off"] {
        assert_eq!(option(&options, source)["agreed"], false, "{options}");
    }

    // The command takes the source and nothing else: the window cannot say "agreed".
    for extra in [
        json!({"source": "keenable", "agreed": true}),
        json!({"source": "keenable", "text": "anything"}),
        json!({}),
        json!(["keenable"]),
    ] {
        assert!(
            matches!(
                ipc::dispatch(shell, "research_consent_ask", extra.clone()).await,
                Err(DispatchError::BadArguments { .. })
            ),
            "{extra}"
        );
    }
    // The core's own agreement is no command of the window.
    for name in ["research_consent", "research_consent_for", "research_set"] {
        assert!(
            matches!(
                ipc::dispatch(shell, name, json!({"agreed": true})).await,
                Err(DispatchError::UnknownCommand(_))
            ),
            "{name}"
        );
    }

    let settings = call(shell, "research_source_set", json!({"source": "keenable"})).await;
    assert_eq!(settings["research"]["source"], "keenable", "{settings}");

    // Asking for another source than the chosen one, or for `off`: refused before any
    // dialog, nothing agreed.
    for source in ["tavily", "built_in", "off"] {
        let error = refused(shell, "research_consent_ask", json!({"source": source})).await;
        assert_eq!(kind(&error), "invalid_input", "{source}: {error:?}");
    }
    assert_eq!(fx.prompt.times_asked(), 0);
    assert_eq!(
        agreed_at(&call(shell, "settings_get", json!({})).await),
        Value::Null
    );

    // "Cancel": nothing is recorded. The dialog showed the core's text for that source,
    // naming who receives the queries: the words `hotend research on` prints.
    fx.prompt.will_answer(Answer::Cancel);
    let answer = call(shell, "research_consent_ask", json!({"source": "keenable"})).await;
    assert_eq!(answer["agreed"], false, "{answer}");
    assert_eq!(option(&answer["options"], "keenable")["agreed"], false);
    assert_eq!(
        agreed_at(&call(shell, "settings_get", json!({})).await),
        Value::Null
    );
    let shown = fx.prompt.last();
    assert_eq!(shown.title, "Agree to web research through Keenable?");
    assert_eq!(shown.confirm_label, "Agree");
    assert_eq!(shown.cancel_label, "Cancel");
    assert_eq!(
        shown.body,
        hotend_core::research::consent_explanation(hotend_core::SearchSource::Keenable, false)
    );
    let host = option(&options, "keenable")["host"].as_str().unwrap();
    assert!(
        !host.is_empty() && shown.body.contains(host),
        "{}",
        shown.body
    );

    // While the dialog is open no second one opens.
    let (opened, close) = fx.prompt.hold_open();
    fx.prompt.will_answer(Answer::Cancel);
    let open = async { call(shell, "research_consent_ask", json!({"source": "keenable"})).await };
    let meanwhile = async {
        tokio::task::spawn_blocking(move || opened.recv().unwrap())
            .await
            .unwrap();
        let busy = refused(shell, "research_consent_ask", json!({"source": "keenable"})).await;
        assert_eq!(kind(&busy), "busy");
        close.send(()).unwrap();
    };
    let (answer, ()) = tokio::join!(open, meanwhile);
    assert_eq!(answer["agreed"], false);

    // A shell that cannot draw the dialog refuses, and its prompt is never asked.
    let unused = Arc::new(ScriptedPrompt::default());
    let headless = Shell::new(
        Ok(fx.core.clone()),
        Desktop {
            prompt: unused.clone(),
            can_confirm: false,
            pickers: fx.pickers.clone(),
            links: fx.links.clone(),
        },
    );
    let error = refused(
        &headless,
        "research_consent_ask",
        json!({"source": "keenable"}),
    )
    .await;
    assert_eq!(kind(&error), "not_configured");
    assert_eq!(unused.times_asked(), 0);

    // The confirming button: agreed, for the chosen source.
    fx.prompt.will_answer(Answer::Confirm);
    let answer = call(shell, "research_consent_ask", json!({"source": "keenable"})).await;
    assert_eq!(answer["agreed"], true, "{answer}");
    assert_eq!(option(&answer["options"], "keenable")["agreed"], true);
    assert_eq!(option(&answer["options"], "tavily")["agreed"], false);
    let settings = call(shell, "settings_get", json!({})).await;
    assert!(agreed_at(&settings).is_number(), "{settings}");
    assert_eq!(settings["research"]["acknowledged_source"], "keenable");
    assert_eq!(fx.prompt.times_asked(), 3);

    // Another source is not covered by it.
    call(shell, "research_source_set", json!({"source": "tavily"})).await;
    let options = call(shell, "research_source_options", json!({})).await;
    assert_eq!(option(&options, "tavily")["agreed"], false, "{options}");

    // Withdrawing ends any agreement.
    call(shell, "research_source_set", json!({"source": "keenable"})).await;
    assert_eq!(
        call(shell, "research_consent_withdraw", json!({})).await,
        Value::Null
    );
    let options = call(shell, "research_source_options", json!({})).await;
    for source in ["built_in", "keenable", "tavily", "off"] {
        assert_eq!(option(&options, source)["agreed"], false, "{options}");
    }
    assert_eq!(
        agreed_at(&call(shell, "settings_get", json!({})).await),
        Value::Null
    );

    // A search key goes in and never comes back; its status is a boolean.
    assert_eq!(
        call(
            shell,
            "search_key_set",
            json!({"service": "tavily", "key": SEARCH_KEY})
        )
        .await,
        Value::Null
    );
    assert_eq!(
        call(shell, "search_key_status", json!({"service": "tavily"})).await,
        json!(true)
    );
    assert_eq!(
        call(shell, "search_key_status", json!({"service": "keenable"})).await,
        json!(false)
    );
    let options = call(shell, "research_source_options", json!({})).await;
    assert_eq!(option(&options, "tavily")["key_present"], true);
    assert!(
        !ipc::command_names()
            .iter()
            .any(|name| name.starts_with("search_key")
                && !["search_key_set", "search_key_delete", "search_key_status"].contains(name)),
        "no command reads a key"
    );
    let reads = [
        ("research_source_options", json!({})),
        ("settings_get", json!({})),
        ("audit_log", json!({"projectId": null, "limit": 500})),
        ("doctor_report", json!({"skipSelfTests": true})),
        ("search_key_status", json!({"service": "tavily"})),
    ];
    for (command, args) in reads {
        let answer = call(shell, command, args).await.to_string();
        assert!(!answer.contains(SEARCH_KEY), "{command} gave the key back");
        assert!(
            !answer.contains(&SEARCH_KEY[..16]),
            "{command} gave part of the key back"
        );
    }
    assert_eq!(
        call(shell, "search_key_delete", json!({"service": "tavily"})).await,
        Value::Null
    );
    assert_eq!(
        call(shell, "search_key_status", json!({"service": "tavily"})).await,
        json!(false)
    );
}

const TO_STUDIO: fn() -> Value = || json!({"kind": "bambu_studio"});

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_editable_project_is_handed_over_only_through_the_native_prompt() {
    let fx = fixture().await;
    let shell = &fx.shell;
    let (project_id, job) = pending_job(shell).await;
    let job_id = job["id"].as_str().unwrap().to_owned();

    // The stand-in slicer makes no project, and there is none to list or hand over.
    let error = refused(
        shell,
        "print_project_make",
        json!({"projectId": project_id, "plate": null}),
    )
    .await;
    assert!(
        serde_json::to_string(&error)
            .unwrap()
            .contains("stand_in_slicer"),
        "{error:?}"
    );
    assert_eq!(
        call(shell, "print_projects", json!({"projectId": project_id})).await,
        json!([])
    );

    // 1. The command takes a project and a destination, nothing else.
    for extra in [
        json!({"projectExportId": "p1", "destination": TO_STUDIO(), "shown": "anything"}),
        json!({"projectExportId": "p1", "destination": TO_STUDIO(), "authority": {}}),
        json!({"projectExportId": "p1", "destination": TO_STUDIO(), "confirmed": true}),
        json!({"projectExportId": "p1"}),
        json!({"projectExportId": "p1", "destination": {"kind": "bambu_connect"}}),
        json!(["p1", TO_STUDIO()]),
    ] {
        match ipc::dispatch(shell, "print_project_confirm", extra.clone()).await {
            Err(DispatchError::BadArguments { command, .. }) => {
                assert_eq!(command, "print_project_confirm")
            }
            other => panic!("{extra} should be refused as bad arguments, got {other:?}"),
        }
    }

    // 2. A folder the person did not pick is refused before any review or prompt, by the
    //    review and by the confirmation.
    let outside = tempfile::tempdir().unwrap();
    let to_folder = json!({"kind": "folder", "folder": outside.path()});
    for command in ["print_project_review", "print_project_confirm"] {
        let error = refused(
            shell,
            command,
            json!({"projectExportId": "p1", "destination": to_folder}),
        )
        .await;
        assert_eq!(kind(&error), "invalid_input", "{command}: {error:?}");
    }

    // 3. A project the core cannot review is refused before the prompt.
    let error = refused(
        shell,
        "print_project_confirm",
        json!({"projectExportId": "no-such-project", "destination": TO_STUDIO()}),
    )
    .await;
    assert_eq!(kind(&error), "print", "{error:?}");
    assert_eq!(fx.prompt.times_asked(), 0);

    // 4. While a confirmation dialog is open, no project is reviewed or confirmed: the
    //    window cannot change what the open dialog's answer applies to.
    let (opened, close) = fx.prompt.hold_open();
    fx.prompt.will_answer(Answer::Cancel);
    let open_prompt = {
        let args = json!({"jobId": job_id, "delivery": SIMULATED()});
        async { call(shell, "print_confirm", args).await }
    };
    let meanwhile = async {
        tokio::task::spawn_blocking(move || opened.recv().unwrap())
            .await
            .unwrap();
        for command in ["print_project_review", "print_project_confirm"] {
            let busy = refused(
                shell,
                command,
                json!({"projectExportId": "p1", "destination": TO_STUDIO()}),
            )
            .await;
            assert_eq!(kind(&busy), "busy", "{command}");
        }
        close.send(()).unwrap();
    };
    let (outcome, ()) = tokio::join!(open_prompt, meanwhile);
    assert_eq!(outcome["confirmed"], false);
    assert_eq!(fx.prompt.times_asked(), 1);

    // 5. A shell that cannot draw the prompt refuses before anything is reviewed.
    let unused = Arc::new(ScriptedPrompt::default());
    let headless = Shell::new(
        Ok(fx.core.clone()),
        Desktop {
            prompt: unused.clone(),
            can_confirm: false,
            pickers: fx.pickers.clone(),
            links: fx.links.clone(),
        },
    );
    let error = refused(
        &headless,
        "print_project_confirm",
        json!({"projectExportId": "p1", "destination": TO_STUDIO()}),
    )
    .await;
    assert_eq!(kind(&error), "not_configured");
    assert_eq!(unused.times_asked(), 0);

    // Nothing was placed in the hand-off folder by any of this.
    let handoff = fx.core.data_dir().join("handoff");
    let placed = std::fs::read_dir(&handoff)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(placed, 0);
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}
