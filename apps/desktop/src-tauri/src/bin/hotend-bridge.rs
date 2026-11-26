//! `hotend-bridge`: the development bridge as a program, for building the interface in a
//! browser and for the Playwright tests. Never shipped: it needs the `dev-bridge` feature,
//! which does not build in a release profile.
//!
//! It starts headless (no window, no display) on a scripted core in a temporary data
//! directory, prints one JSON line with its address and token on standard output, and
//! serves until it is interrupted (Ctrl-C or SIGTERM).

use std::path::PathBuf;
use std::process::ExitCode;

use hotend_core::ApiError;
use hotend_desktop::bridge;

const USAGE: &str = "\
hotend-bridge: Hotend's development bridge (never part of a release)

Usage: hotend-bridge [options]
  --port <n>                   listen on 127.0.0.1:<n> (default: a free port)
  --data-dir <dir>             keep the data there (default: a temporary folder, removed at exit)
  --script <builtin:name|file> the conversation the agent replays (default: builtin:cable_box_demo)
  --time-scale <n>             simulated printer seconds per second (default: 3600)
  --answer-dialogs-for-tests   answer every print confirmation with \"confirm\" and every
                               folder picker with a folder in the data directory. Tests only:
                               without it the bridge refuses to confirm a print.

Prints {\"hotend_bridge\":1,\"url\":...,\"token\":...} on standard output when it is ready.
The agent replays a written conversation (no model is asked, no key is read), the slicer is
the stand-in and the printer is simulated.";

struct Options {
    port: u16,
    data_dir: Option<PathBuf>,
    script: String,
    time_scale: f64,
    answer_dialogs: bool,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        port: 0,
        data_dir: None,
        script: bridge::DEFAULT_SCRIPT.to_owned(),
        time_scale: 3600.0,
        answer_dialogs: false,
    };
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--port" => {
                options.port = value("--port")?
                    .parse()
                    .map_err(|_| "--port needs a number from 0 to 65535".to_owned())?;
            }
            "--data-dir" => options.data_dir = Some(PathBuf::from(value("--data-dir")?)),
            "--script" => options.script = value("--script")?,
            "--time-scale" => {
                options.time_scale = value("--time-scale")?
                    .parse()
                    .ok()
                    .filter(|scale: &f64| scale.is_finite() && *scale > 0.0)
                    .ok_or_else(|| "--time-scale needs a positive number".to_owned())?;
            }
            "--answer-dialogs-for-tests" => options.answer_dialogs = true,
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(options)
}

/// Resolves when the process is asked to stop: Ctrl-C or SIGTERM.
async fn stop_signal() {
    #[cfg(unix)]
    let terminated = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminated = std::future::pending::<()>();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = terminated => {}
    }
}

async fn run(options: Options) -> Result<(), String> {
    // Kept until the end of `run`: dropping it removes the temporary folder.
    let mut temporary = None;
    let data_dir = match options.data_dir {
        Some(dir) => {
            std::fs::create_dir_all(&dir)
                .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
            dir
        }
        None => {
            let dir = tempfile::Builder::new()
                .prefix("hotend-bridge-")
                .tempdir()
                .map_err(|error| format!("cannot create a temporary folder: {error}"))?;
            let path = dir.path().to_path_buf();
            temporary = Some(dir);
            path
        }
    };
    let data_dir = data_dir
        .canonicalize()
        .map_err(|error| format!("cannot use {}: {error}", data_dir.display()))?;
    let core = bridge::open_scripted_core(&data_dir, &options.script, options.time_scale)
        .await
        .map_err(|message| ApiError::Internal {
            message,
            next_step: "Run `hotend doctor`; it says what is missing on this machine.".to_owned(),
        });
    if let Err(error) = &core {
        // The bridge still serves: every command answers with this error, as the window
        // would show it.
        eprintln!("hotend-bridge: the core did not open: {}", error.message());
    }
    let listener = bridge::bind(options.port).await?;
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    let token = bridge::new_token()?;
    let ready = serde_json::json!({
        "hotend_bridge": 1,
        "url": format!("http://{address}"),
        "token": token,
        "data_dir": data_dir,
        "answers_dialogs": options.answer_dialogs,
    });
    println!("{ready}");
    eprintln!(
        "hotend-bridge: serving a scripted core on http://{address} (development only){}",
        if options.answer_dialogs {
            "; TEST SWITCH ON: print confirmations are answered automatically"
        } else {
            "; print confirmations are refused"
        }
    );
    let shell = bridge::shell(core, options.answer_dialogs);
    let served = bridge::serve(listener, shell, token, stop_signal()).await;
    drop(temporary);
    served
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let options = match parse(args.into_iter()) {
        Ok(options) => options,
        Err(problem) => {
            eprintln!("hotend-bridge: {problem}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hotend-bridge: cannot start: {error}");
            return ExitCode::from(2);
        }
    };
    match runtime.block_on(run(options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("hotend-bridge: {problem}");
            ExitCode::from(2)
        }
    }
}
