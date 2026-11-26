//! `hotend-confirm-dialog`: shows the real confirmation dialog of `print_confirm`, for
//! looking at it and for trying both of its buttons. A development tool like
//! `hotend-bridge` (it needs the `dev-bridge` feature, which does not build in a release
//! profile). Nothing can be printed: the printer is the simulated one.
//!
//!     cargo run -p hotend-desktop --features dev-bridge --bin hotend-confirm-dialog -- \
//!         [--name <project name>] [--asks <number>]
//!
//! It builds the cable box of the engine's tests in a temporary folder, slices it with the
//! stand-in slicer, proposes a print and calls the `print_confirm` command exactly as the
//! window does. The command opens the native dialog; the tool prints what the command
//! answered (`{"confirmed", "status"}`, one line) and exits. With `--asks`, a dialog that
//! was not confirmed is asked again, up to that many times, one line per answer: the job
//! is still `pending` then. `tests/confirm_dialog.sh` drives it with key presses.
//!
//!     ... --bin hotend-confirm-dialog -- --text <file>
//!
//! shows the same dialog with the text of a file as its body and no print job behind it,
//! for looking at how a long or odd text is laid out; it prints `{"confirmed"}`.

use std::path::Path;
use std::sync::Arc;

use hotend_desktop::app::NativePrompt;
use hotend_desktop::bridge;
use hotend_desktop::confirm::{Answer, ConfirmationPrompt, ConfirmationText};
use hotend_desktop::grants::NoPickers;
use hotend_desktop::ipc;
use hotend_desktop::links::LinkOpener;
use hotend_desktop::shell::{Desktop, Shell};
use serde_json::{Value, json};

struct OpensNothing;

impl LinkOpener for OpensNothing {
    fn open(&self, _url: &url::Url) -> Result<(), String> {
        Ok(())
    }
}

async fn call(shell: &Shell, command: &str, args: Value) -> Result<Value, String> {
    ipc::dispatch(shell, command, args)
        .await
        .map_err(|error| format!("{command}: {error}"))
}

/// What the command line asked for.
struct Options {
    name: String,
    asks: u32,
    text: Option<std::path::PathBuf>,
}

fn options() -> Result<Options, String> {
    let mut options = Options {
        name: "Cable box".to_owned(),
        asks: 1,
        text: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--name" => options.name = value()?,
            "--text" => options.text = Some(value()?.into()),
            "--asks" => {
                options.asks = value()?
                    .parse()
                    .ok()
                    .filter(|asks| (1..=20).contains(asks))
                    .ok_or("--asks takes a number from 1 to 20")?;
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(options)
}

/// The dialog alone, with the text of a file as its body.
async fn show_text(app: tauri::AppHandle, file: &Path) -> Result<(), String> {
    let body = std::fs::read_to_string(file)
        .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
    let text = ConfirmationText {
        title: "Start this print?".to_owned(),
        body: body.trim_end().to_owned(),
        confirm_label: "Start print".to_owned(),
        cancel_label: "Cancel".to_owned(),
    };
    eprintln!("hotend-confirm-dialog: the dialog is open; press one of its buttons");
    let answer = tokio::task::spawn_blocking(move || NativePrompt::new(app).ask(&text))
        .await
        .map_err(|error| error.to_string())?;
    println!("{}", json!({"confirmed": answer == Answer::Confirm}));
    Ok(())
}

async fn run(app: tauri::AppHandle, options: Options) -> Result<(), String> {
    if let Some(file) = &options.text {
        return show_text(app, file).await;
    }
    let model =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../python/tests/models/cable_box.py");
    let code = std::fs::read_to_string(&model)
        .map_err(|error| format!("cannot read {}: {error}", model.display()))?;
    let data = tempfile::tempdir().map_err(|error| error.to_string())?;
    let core = bridge::open_scripted_core(data.path(), bridge::DEFAULT_SCRIPT, 3600.0).await?;
    let shell = Shell::new(
        Ok(core),
        Desktop {
            prompt: Arc::new(NativePrompt::new(app)),
            can_confirm: true,
            pickers: Arc::new(NoPickers),
            links: Arc::new(OpensNothing),
        },
    );
    let project = call(&shell, "project_create", json!({"name": options.name})).await?;
    let id = &project["id"];
    eprintln!("hotend-confirm-dialog: building and slicing the cable box (about 20 s)");
    call(
        &shell,
        "model_build",
        json!({"projectId": id, "code": code, "params": {}, "note": "dialog"}),
    )
    .await?;
    call(
        &shell,
        "slice_run",
        json!({"projectId": id, "profile": "demo-0.20mm", "plate": null}),
    )
    .await?;
    let job = call(
        &shell,
        "print_propose",
        json!({"projectId": id, "note": "dialog"}),
    )
    .await?;
    for _ in 0..options.asks {
        eprintln!("hotend-confirm-dialog: the dialog is open; press one of its buttons");
        let outcome = call(
            &shell,
            "print_confirm",
            json!({"jobId": job["id"], "delivery": {"kind": "simulated_printer"}}),
        )
        .await?;
        println!(
            "{}",
            json!({"confirmed": outcome["confirmed"], "status": outcome["job"]["status"]})
        );
        if outcome["confirmed"] == true {
            break;
        }
    }
    Ok(())
}

fn main() {
    let options = match options() {
        Ok(options) => options,
        Err(problem) => {
            eprintln!("hotend-confirm-dialog: {problem}");
            std::process::exit(2);
        }
    };
    let started = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match run(handle.clone(), options).await {
                    Ok(()) => handle.exit(0),
                    Err(problem) => {
                        eprintln!("hotend-confirm-dialog: {problem}");
                        handle.exit(1);
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!());
    if let Err(error) = started {
        eprintln!("hotend-confirm-dialog: cannot start: {error}");
        std::process::exit(1);
    }
}
