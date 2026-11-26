//! The development bridge: the window's commands over loopback HTTP, so that the interface
//! can be built in a normal browser and tested by Playwright without a Tauri window.
//!
//! It exists only with the `dev-bridge` feature, which cannot be built in a release
//! profile. Guarantees while it runs:
//!
//! - It listens on 127.0.0.1 only, and answers only requests that carry the random token
//!   printed at start (`Authorization: Bearer <token>`), whose `Host` is the loopback
//!   address it listens on (so a web page cannot reach it by DNS rebinding) and whose
//!   `Origin`, when a browser sends one, is a loopback page.
//! - A command is run by [`crate::ipc::dispatch`]: the same table and the same [`Shell`]
//!   methods as the Tauri commands, so the bridge can do nothing the window cannot.
//! - It has no window, so it has nobody to ask: `print_confirm`,
//!   `print_project_confirm`, the certificate pin and the search agreement are refused before
//!   anything is reviewed, unless the bridge was started with `--answer-dialogs-for-tests`,
//!   which answers every confirmation (a print, a project, a certificate pin, a search
//!   agreement) with "confirm" and every picker with a folder inside the bridge's own data
//!   directory. That switch is for end-to-end tests of the simulated print and of the project
//!   hand-over; the only printer the core has is the simulated one, and its opener is the
//!   stand-in, which opens nothing.
//! - The core it serves is a scripted one ([`open_scripted_core`]): the agent replays a
//!   written conversation, no model is asked, no key is read, the slicer is the stand-in.
//!
//! Wire format: `POST /invoke/<command>` with the argument object as JSON answers
//! `{"ok": true, "data": ...}` or `{"ok": false, "error": ApiError}`; `GET /events` is a
//! server-sent event stream with one `ShellEvent` as JSON per event; `GET /health` answers
//! without a token and says only that this is the bridge.

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::header::{AUTHORIZATION, HOST, ORIGIN};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;
use hotend_core::{
    AgentCommand, ApiError, Core, CoreConfig, MemorySecretStore, PrinterOptions, ProviderKind,
    ProviderSettings, PythonRuntime, SecretBackend, Settings,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use url::Url;

use crate::confirm::{Answer, ConfirmationPrompt, ConfirmationText, NoPrompt};
use crate::grants::{NoPickers, Pickers};
use crate::ipc::{self, DispatchError};
use crate::links::LinkOpener;
use crate::shell::{Desktop, Shell};

/// The agent script the bridge replays unless told otherwise.
pub const DEFAULT_SCRIPT: &str = "builtin:cable_box_demo";
/// The provider slot of a scripted core: the one kind that takes no key. No request is
/// ever sent to it.
const PLACEHOLDER_PROVIDER: ProviderKind = ProviderKind::Ollama;

/// Opens a core for development and tests in `data_dir`: the agent replays `script`
/// (`builtin:<name>` or a file), keys live in an empty in-memory store, the slicer is the
/// stand-in and the simulated printer's clock runs `time_scale` times faster than real time.
pub async fn open_scripted_core(
    data_dir: &Path,
    script: &str,
    time_scale: f64,
) -> Result<Core, String> {
    let runtime = PythonRuntime::discover()
        .await
        .map_err(|error| error.to_string())?;
    let mut agent = AgentCommand::python_module(&runtime);
    let source = if script.starts_with("builtin:") {
        script.to_owned()
    } else {
        let path = std::fs::canonicalize(script)
            .map_err(|error| format!("cannot read the script {script}: {error}"))?;
        // The sandboxed agent reads the file, so its folder joins the read-only view.
        if let Some(folder) = path.parent() {
            agent.read_only.push(folder.to_path_buf());
        }
        path.display().to_string()
    };
    agent.args.extend(["--script".to_owned(), source]);
    let core = Core::open(CoreConfig {
        data_dir: Some(data_dir.to_path_buf()),
        secrets: SecretBackend::Custom(Arc::new(MemorySecretStore::default())),
        runtime: Some(runtime),
        agent: Some(agent),
        printer: PrinterOptions {
            time_scale,
            ..PrinterOptions::default()
        },
        ..CoreConfig::default()
    })
    .await
    .map_err(|error| error.to_string())?;
    core.settings_set(Settings {
        provider: Some(ProviderSettings {
            kind: PLACEHOLDER_PROVIDER,
            model: "scripted".to_owned(),
        }),
        // The scripted scenario has the textured plate on its simulated printer; a slice
        // made without a chosen plate cannot become a print job.
        plate_type: Some("textured-pei".to_owned()),
        ..Settings::default()
    })
    .await
    .map_err(|error| error.to_string())?;
    hotend_slicer::install(&core, hotend_slicer::Programs::StandIn)
        .map_err(|error| error.to_string())?;
    Ok(core)
}

/// Answers every confirmation with "confirm", and says so on standard error. Only the
/// `--answer-dialogs-for-tests` switch installs it.
struct AnswersForTests;

impl ConfirmationPrompt for AnswersForTests {
    fn ask(&self, text: &ConfirmationText) -> Answer {
        eprintln!(
            "hotend-bridge: TEST SWITCH: answering \"{}\" with \"{}\" for nobody:\n{}",
            text.title, text.confirm_label, text.body
        );
        Answer::Confirm
    }
}

/// Pickers for tests: a new folder inside the bridge's data directory for every folder
/// asked for, and the stand-in slicer for a program.
struct PickersForTests {
    root: PathBuf,
    next: AtomicU32,
}

impl Pickers for PickersForTests {
    fn pick_folder(&self, _title: &str) -> Option<PathBuf> {
        let folder = self
            .root
            .join(format!("{}", self.next.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&folder).ok()?;
        Some(folder)
    }

    fn pick_file(&self, _title: &str) -> Option<PathBuf> {
        hotend_slicer::fake::locate()
    }
}

/// Opens nothing: the bridge has no desktop. The address is written to standard error.
struct LogsLinks;

impl LinkOpener for LogsLinks {
    fn open(&self, url: &Url) -> Result<(), String> {
        eprintln!("hotend-bridge: would open {url} in the system browser");
        Ok(())
    }
}

/// The shell the bridge serves. With `answer_dialogs` it confirms prints and project
/// hand-overs, pins certificates, agrees to search and picks folders by itself (tests only);
/// without, it refuses `print_confirm`, `print_project_confirm`, the certificate pin and the
/// search agreement, and every picker cancels.
pub fn shell(core: Result<Core, ApiError>, answer_dialogs: bool) -> Shell {
    // The bridge never opens Bambu Connect: its opener is the stand-in, which records the
    // link (in the file `HOTEND_STAND_IN_OPENER` names, when it names one) and opens
    // nothing. The hand-off path is made for this host, as in the application.
    if let Ok(core) = &core {
        core.set_handoff_host(hotend_core::HostKind::detect());
        core.set_link_opener(Arc::new(hotend_core::StandInOpener::from_env()));
    }
    let picked = core
        .as_ref()
        .map(|core| core.data_dir().join("picked"))
        .unwrap_or_default();
    let desktop = if answer_dialogs {
        Desktop {
            prompt: Arc::new(AnswersForTests),
            can_confirm: true,
            pickers: Arc::new(PickersForTests {
                root: picked,
                next: AtomicU32::new(1),
            }),
            links: Arc::new(LogsLinks),
        }
    } else {
        Desktop {
            prompt: Arc::new(NoPrompt),
            can_confirm: false,
            pickers: Arc::new(NoPickers),
            links: Arc::new(LogsLinks),
        }
    };
    Shell::new(core, desktop)
}

/// A new random token: 32 bytes from the operating system, as hex.
pub fn new_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| format!("no random bytes: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// A listener on the loopback address. `port` 0 lets the system choose.
pub async fn bind(port: u16) -> Result<TcpListener, String> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .await
        .map_err(|error| format!("cannot listen on 127.0.0.1:{port}: {error}"))
}

struct Bridge {
    shell: Shell,
    token: String,
    port: u16,
}

fn same_bytes(a: &[u8], b: &[u8]) -> bool {
    // Compared without stopping at the first difference.
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn is_loopback_origin(origin: &str) -> bool {
    Url::parse(origin).is_ok_and(|url| {
        url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
    })
}

/// Why a request was turned away before any command ran.
type Refusal = (StatusCode, &'static str);

impl Bridge {
    /// The `Host` must be this listener and a browser's `Origin` a loopback page.
    fn check_caller(&self, headers: &HeaderMap) -> Result<(), Refusal> {
        let host = headers.get(HOST).and_then(|value| value.to_str().ok());
        let expected = [
            format!("127.0.0.1:{}", self.port),
            format!("localhost:{}", self.port),
        ];
        if !host.is_some_and(|host| expected.iter().any(|own| own == host)) {
            return Err((StatusCode::FORBIDDEN, "this is not the bridge's address"));
        }
        if let Some(origin) = headers.get(ORIGIN)
            && !origin.to_str().is_ok_and(is_loopback_origin)
        {
            return Err((StatusCode::FORBIDDEN, "this page may not use the bridge"));
        }
        Ok(())
    }

    fn check_token(&self, headers: &HeaderMap) -> Result<(), Refusal> {
        let given = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        if given.is_some_and(|given| same_bytes(given.as_bytes(), self.token.as_bytes())) {
            Ok(())
        } else {
            Err((
                StatusCode::UNAUTHORIZED,
                "the bridge token is missing or wrong",
            ))
        }
    }
}

/// Lets a loopback page read the answer (the interface's development server is another
/// origin than the bridge).
fn with_cors(headers: &HeaderMap, mut response: Response) -> Response {
    if let Some(origin) = headers.get(ORIGIN)
        && origin.to_str().is_ok_and(is_loopback_origin)
    {
        let out = response.headers_mut();
        out.insert("access-control-allow-origin", origin.clone());
        out.insert("vary", HeaderValue::from_static("Origin"));
        out.insert(
            "access-control-allow-headers",
            HeaderValue::from_static("authorization, content-type"),
        );
        out.insert(
            "access-control-allow-methods",
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
        out.insert("access-control-max-age", HeaderValue::from_static("600"));
    }
    response
}

async fn preflight(State(bridge): State<Arc<Bridge>>, headers: HeaderMap) -> Response {
    match bridge.check_caller(&headers) {
        Ok(()) => with_cors(&headers, StatusCode::NO_CONTENT.into_response()),
        Err(refused) => refused.into_response(),
    }
}

async fn health(State(bridge): State<Arc<Bridge>>, headers: HeaderMap) -> Response {
    match bridge.check_caller(&headers) {
        Ok(()) => with_cors(
            &headers,
            axum::Json(json!({"hotend_bridge": 1})).into_response(),
        ),
        Err(refused) => refused.into_response(),
    }
}

async fn invoke(
    State(bridge): State<Arc<Bridge>>,
    UrlPath(command): UrlPath<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(refused) = bridge.check_caller(&headers) {
        return refused.into_response();
    }
    if let Err(refused) = bridge.check_token(&headers) {
        return with_cors(&headers, refused.into_response());
    }
    let args: Value = if body.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&body) {
            Ok(args) => args,
            Err(error) => {
                let refused = (
                    StatusCode::BAD_REQUEST,
                    format!("the arguments are not JSON: {error}"),
                );
                return with_cors(&headers, refused.into_response());
            }
        }
    };
    let response = match ipc::dispatch(&bridge.shell, &command, args).await {
        Ok(data) => axum::Json(json!({"ok": true, "data": data})).into_response(),
        Err(DispatchError::Failed(error)) => {
            axum::Json(json!({"ok": false, "error": error})).into_response()
        }
        Err(error @ DispatchError::UnknownCommand(_)) => {
            (StatusCode::NOT_FOUND, error.to_string()).into_response()
        }
        Err(error @ DispatchError::BadArguments { .. }) => {
            (StatusCode::BAD_REQUEST, error.to_string()).into_response()
        }
    };
    with_cors(&headers, response)
}

fn event_stream(
    events: tokio::sync::broadcast::Receiver<hotend_core::CoreEvent>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    // `forward` pushes into a channel; the stream ends when the client goes away.
    let (send, receive) = tokio::sync::mpsc::channel::<String>(256);
    tokio::spawn(async move {
        let closed = send.clone();
        tokio::select! {
            () = crate::events::forward(events, move |event| {
                // A full buffer drops the event for this reader, as a slow window would.
                match serde_json::to_string(&event) {
                    Ok(json) => !matches!(
                        send.try_send(json),
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_))
                    ),
                    Err(_) => true,
                }
            }) => {}
            () = closed.closed() => {}
        }
    });
    futures_util::stream::unfold(receive, |mut receive| async move {
        let json = receive.recv().await?;
        Some((Ok(Event::default().data(json)), receive))
    })
}

async fn events(State(bridge): State<Arc<Bridge>>, headers: HeaderMap) -> Response {
    if let Err(refused) = bridge.check_caller(&headers) {
        return refused.into_response();
    }
    if let Err(refused) = bridge.check_token(&headers) {
        return with_cors(&headers, refused.into_response());
    }
    let response = match bridge.shell.core() {
        Ok(core) => Sse::new(event_stream(core.events()))
            .keep_alive(KeepAlive::default())
            .into_response(),
        Err(error) => axum::Json(json!({"ok": false, "error": error})).into_response(),
    };
    with_cors(&headers, response)
}

/// Serves `shell` on `listener` until `stop` resolves. Every request needs `token`.
/// Stopping does not wait for open connections: an event stream never ends by itself.
pub async fn serve(
    listener: TcpListener,
    shell: Shell,
    token: String,
    stop: impl Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    if !address.ip().is_loopback() {
        return Err(format!(
            "the bridge only serves a loopback address, not {address}"
        ));
    }
    let bridge = Arc::new(Bridge {
        shell,
        token,
        port: address.port(),
    });
    let app = Router::new()
        .route("/health", get(health).options(preflight))
        .route("/invoke/{command}", post(invoke).options(preflight))
        .route("/events", get(events).options(preflight))
        .with_state(bridge);
    tokio::select! {
        served = axum::serve(listener, app).into_future() => {
            served.map_err(|error| error.to_string())
        }
        () = stop => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_pages_count_as_callers() {
        for own in [
            "http://localhost:1420",
            "http://127.0.0.1:4173",
            "http://localhost",
        ] {
            assert!(is_loopback_origin(own), "{own}");
        }
        for other in [
            "https://example.com",
            "http://127.0.0.1.example.com",
            "http://localhost.example.com:1420",
            "https://localhost:1420",
            "null",
            "",
        ] {
            assert!(!is_loopback_origin(other), "{other}");
        }
    }

    #[test]
    fn tokens_are_long_random_and_compared_whole() {
        #![allow(clippy::unwrap_used)]
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(same_bytes(a.as_bytes(), a.as_bytes()));
        assert!(!same_bytes(a.as_bytes(), b.as_bytes()));
        assert!(!same_bytes(a.as_bytes(), &a.as_bytes()[..63]));
        assert!(!same_bytes(b"", a.as_bytes()));
    }
}
