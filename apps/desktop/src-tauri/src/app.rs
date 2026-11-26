//! The application: one window, the native dialogs, and the core behind the commands.
//!
//! Guarantees:
//!
//! - The window loads the bundled interface and nothing else: a navigation to any other
//!   address is refused ([`is_app_address`]), and the page cannot open another window.
//! - The confirmation, the folder and file pickers and the system browser are driven from
//!   Rust. Bambu Connect's import link is opened by the core after a confirmed hand-off,
//!   through the opener for this platform that is given to it here
//!   ([`hotend_core::platform_opener`]: `rundll32 url.dll,FileProtocolHandler` for the
//!   link and explorer.exe for a project, through WSL or on Windows, `open` on macOS; the
//!   stand-in when `HOTEND_STAND_IN_OPENER` is set). The dialog plugin is registered because
//!   its Rust side needs it, but the window's capability grants none of its commands, so the
//!   page cannot open or answer a dialog.
//! - Developer tools exist in debug builds only: the `devtools` feature of `tauri` is not
//!   enabled.
//! - An installed (release) application runs the Python environment it was installed
//!   with, the folder `python` under its resource folder, unless `HOTEND_PYTHON` names
//!   another ([`packaged_python`]). The sandbox launcher is the `hotend-sandbox` beside
//!   the executable. `tauri.bundle.conf.json` is the bundle configuration with that layout.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hotend_core::secrets::without_secret_env;
use hotend_core::{ApiError, Core, CoreConfig, CoreError, PythonRuntime, SecretBackend};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;
use url::Url;

use crate::confirm::{Answer, ConfirmationPrompt, ConfirmationText};
use crate::grants::Pickers;
use crate::ipc::{self, Subscription};
use crate::links::LinkOpener;
use crate::shell::{Desktop, Shell};

/// The label of the one window. The capability names it.
pub const MAIN_WINDOW: &str = "main";

/// Whether `url` is the application's own interface: the bundled files, or, in a debug
/// build, the development server of `tauri.conf.json`.
///
/// The bundled files are at `tauri://localhost` on Linux and macOS and at
/// `http://tauri.localhost` on Windows and Android. Each form counts only where it is the
/// application: on Linux and macOS `http://tauri.localhost` is an ordinary address of
/// this computer, answered by whatever program listens on port 80.
pub fn is_app_address(url: &Url, dev_server: Option<&Url>) -> bool {
    let bundled = is_bundled(url.scheme(), url.host_str(), url.port());
    let dev = dev_server.is_some_and(|dev| {
        url.scheme() == dev.scheme()
            && url.host_str() == dev.host_str()
            && url.port_or_known_default() == dev.port_or_known_default()
    });
    bundled || dev
}

#[cfg(any(windows, target_os = "android"))]
fn is_bundled(scheme: &str, host: Option<&str>, port: Option<u16>) -> bool {
    matches!(
        (scheme, host, port),
        ("http" | "https", Some("tauri.localhost"), None)
    )
}

#[cfg(not(any(windows, target_os = "android")))]
fn is_bundled(scheme: &str, host: Option<&str>, port: Option<u16>) -> bool {
    matches!((scheme, host, port), ("tauri", Some("localhost"), None))
}

/// The confirmation as a system dialog ([`crate::dialog`]), parented to the window when
/// there is one. It is drawn by the operating system's toolkit from Rust: nothing in the
/// web view can draw over it, read it or press its buttons.
pub struct NativePrompt {
    app: tauri::AppHandle,
}

impl NativePrompt {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

impl ConfirmationPrompt for NativePrompt {
    fn ask(&self, text: &ConfirmationText) -> Answer {
        crate::dialog::ask(&self.app, text)
    }
}

struct NativePickers {
    app: tauri::AppHandle,
}

impl NativePickers {
    fn dialog(&self, title: &str) -> tauri_plugin_dialog::FileDialogBuilder<tauri::Wry> {
        let mut dialog = self.app.dialog().file().set_title(title);
        if let Some(window) = self.app.get_webview_window(MAIN_WINDOW) {
            dialog = dialog.set_parent(&window);
        }
        dialog
    }
}

impl Pickers for NativePickers {
    fn pick_folder(&self, title: &str) -> Option<PathBuf> {
        self.dialog(title)
            .blocking_pick_folder()
            .and_then(|path| path.into_path().ok())
    }

    fn pick_file(&self, title: &str) -> Option<PathBuf> {
        self.dialog(title)
            .blocking_pick_file()
            .and_then(|path| path.into_path().ok())
    }
}

/// The person's own browser, through the operating system's opener.
struct SystemBrowser;

impl LinkOpener for SystemBrowser {
    fn open(&self, url: &Url) -> Result<(), String> {
        let mut problem = "this system has no program that opens a link".to_owned();
        for mut command in browser_commands(url) {
            match start(&mut command, OPENER_FAILS_WITHIN) {
                Ok(()) => return Ok(()),
                Err(error) => problem = error,
            }
        }
        Err(problem)
    }
}

/// The commands that show `url` in the person's browser, the first that works is enough:
/// the operating system's openers as the `open` crate lists them, without standard streams
/// and with Hotend's environment less the variables that may hold a secret
/// ([`hotend_core::secrets::SECRET_ENV`]).
fn browser_commands(url: &Url) -> Vec<Command> {
    let mut commands = open::commands(url.as_str());
    for command in &mut commands {
        without_secret_env(command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Ctrl-C in the terminal Hotend was started from does not reach the browser.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(command, 0);
    }
    commands
}

/// How long an opener may take to say that it failed; one that still runs is taken to have
/// opened the link.
const OPENER_FAILS_WITHIN: Duration = Duration::from_millis(250);

/// Starts an opener and reaps it on a thread of its own. An opener that cannot be started,
/// or that ends with a failure within `fails_within`, is an error. On Windows no exit status
/// is read: Explorer's says nothing.
fn start(command: &mut Command, fails_within: Duration) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .spawn()
        .map_err(|error| format!("`{program}` could not be started: {error}"))?;
    if cfg!(unix) {
        let deadline = Instant::now() + fails_within;
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => return Err(format!("`{program}` ended with {status}")),
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The interpreter this application was installed with, when that is the one to use: in
/// a release build, unless `HOTEND_PYTHON` names another. `None` leaves the choice to the
/// core (the variable, or the checkout's environment in a debug build).
fn packaged_python(
    debug_build: bool,
    overridden: bool,
    resources: Option<PathBuf>,
) -> Option<PathBuf> {
    if debug_build || overridden {
        return None;
    }
    resources.map(|folder| PythonRuntime::packaged_interpreter(&folder))
}

/// Opens the core with the product configuration and the installed slicers. `python` is
/// the interpreter of [`packaged_python`].
async fn open_core(python: Option<PathBuf>) -> Result<Core, ApiError> {
    let runtime = match python {
        Some(python) => Some(
            PythonRuntime::from_interpreter(python, None)
                .await
                .map_err(|error| ApiError::from(&CoreError::from(error)))?,
        ),
        None => None,
    };
    let core = Core::open(CoreConfig {
        secrets: SecretBackend::from_env(),
        runtime,
        ..CoreConfig::default()
    })
    .await?;
    // The hand-off is made for this host (under WSL: a Windows drive folder and the
    // Windows path), and its link is opened by this platform's opener.
    let host = hotend_core::HostKind::detect();
    core.set_handoff_host(host);
    core.set_link_opener(hotend_core::platform_opener(host));
    hotend_slicer::install(&core, hotend_slicer::Programs::Installed).map_err(|error| {
        ApiError::Internal {
            message: format!("the slicer integration could not start: {error}"),
            next_step: "Run the doctor report; it lists the slicers and what is wrong.".to_owned(),
        }
    })?;
    Ok(core)
}

/// Runs the application until its window is closed.
pub fn run() {
    let specta = ipc::specta_builder::<tauri::Wry>();
    let context = tauri::generate_context!();
    let dev_server = if cfg!(debug_assertions) {
        context.config().build.dev_url.clone()
    } else {
        None
    };
    let outcome = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(specta.invoke_handler())
        .setup(move |app| {
            let handle = app.handle().clone();
            // A core that cannot open is not the end of the window: every command then
            // answers with the reason, and the doctor report says what to fix.
            let python = packaged_python(
                cfg!(debug_assertions),
                std::env::var_os(hotend_core::runtime::PYTHON_ENV).is_some_and(|v| !v.is_empty()),
                app.path().resource_dir().ok(),
            );
            let core = tauri::async_runtime::block_on(open_core(python));
            app.manage(Arc::new(Shell::new(
                core,
                Desktop {
                    prompt: Arc::new(NativePrompt::new(handle.clone())),
                    can_confirm: true,
                    pickers: Arc::new(NativePickers {
                        app: handle.clone(),
                    }),
                    links: Arc::new(SystemBrowser),
                },
            )));
            app.manage(Subscription::default());
            WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::App("index.html".into()))
                .title("Hotend")
                .inner_size(1280.0, 800.0)
                .min_inner_size(1100.0, 700.0)
                .on_navigation(move |url| is_app_address(url, dev_server.as_ref()))
                .on_new_window(|_url, _features| tauri::webview::NewWindowResponse::Deny)
                .build()?;
            Ok(())
        })
        .run(context);
    if let Err(error) = outcome {
        eprintln!("hotend-desktop: the application could not start: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    /// The browser gets the link and none of the variables that may hold a secret (built,
    /// never started).
    #[test]
    fn the_browser_does_not_inherit_a_secret() {
        let link = url("https://console.anthropic.com/settings/keys");
        let commands = browser_commands(&link);
        assert!(!commands.is_empty());
        for command in &commands {
            let removed: Vec<_> = command
                .get_envs()
                .filter(|(_, value)| value.is_none())
                .filter_map(|(name, _)| name.to_str())
                .collect();
            for name in hotend_core::secrets::SECRET_ENV {
                assert!(removed.contains(&name), "{name}: {command:?}");
            }
            assert_eq!(removed.len(), hotend_core::secrets::SECRET_ENV.len());
        }
        #[cfg(target_os = "linux")]
        assert!(commands.iter().any(|command| {
            command.get_program() == "xdg-open" && command.get_args().eq([link.as_str()])
        }));
    }

    /// An opener that is not there or fails at once is an error, so the next one is tried.
    #[cfg(unix)]
    #[test]
    fn an_opener_that_fails_at_once_is_an_error() {
        let long = Duration::from_secs(30);
        assert!(start(&mut Command::new("/no/such/opener"), long).is_err());
        assert!(start(&mut Command::new("false"), long).is_err());
        assert!(start(&mut Command::new("true"), long).is_ok());
        assert!(start(Command::new("sleep").arg("1"), Duration::from_millis(50)).is_ok());
    }

    #[test]
    fn an_installed_application_runs_the_python_it_was_installed_with() {
        let resources = PathBuf::from("/usr/lib/Hotend");
        let python = packaged_python(false, false, Some(resources.clone())).unwrap();
        assert!(python.starts_with("/usr/lib/Hotend/python"), "{python:?}");
        // The variable wins, and a debug build uses the checkout: both are the core's rule.
        assert_eq!(packaged_python(false, true, Some(resources.clone())), None);
        assert_eq!(packaged_python(true, false, Some(resources)), None);
        // No resource folder: the core then says that nothing was configured.
        assert_eq!(packaged_python(false, false, None), None);
    }

    /// The layout an installer produces, with the checkout's interpreter standing in for
    /// the bundled one: the core gets a runtime from it.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_installed_layout_gives_the_core_a_runtime() {
        let checkout = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../python/.venv/bin/python");
        let real = checkout.canonicalize().unwrap();
        let resources = tempfile::tempdir().unwrap();
        let python = PythonRuntime::packaged_interpreter(resources.path());
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &python).unwrap();
        let runtime = PythonRuntime::from_interpreter(python.clone(), None)
            .await
            .unwrap();
        assert_eq!(runtime.python, python);
        assert!(!runtime.read_only.is_empty());

        // An installation without its environment is a typed error, not a panic.
        let empty = tempfile::tempdir().unwrap();
        let missing = PythonRuntime::packaged_interpreter(empty.path());
        let error = PythonRuntime::from_interpreter(missing, None)
            .await
            .map_err(|error| ApiError::from(&CoreError::from(error)))
            .unwrap_err();
        assert!(matches!(error, ApiError::Internal { .. }), "{error:?}");
    }

    #[test]
    fn the_window_stays_on_the_application() {
        let dev = url("http://localhost:1420");
        // The bundled interface has one address per platform, and only that one counts:
        // the other platform's form is, here, an address anything may be listening on.
        let (own, foreign): (&[&str], &[&str]) = if cfg!(any(windows, target_os = "android")) {
            (
                &[
                    "http://tauri.localhost/index.html",
                    "https://tauri.localhost/",
                ],
                &[
                    "tauri://localhost",
                    "tauri://localhost/hotend/p/1/design?v=2",
                ],
            )
        } else {
            (
                &[
                    "tauri://localhost",
                    "tauri://localhost/hotend/p/1/design?v=2",
                ],
                &[
                    "http://tauri.localhost/index.html",
                    "https://tauri.localhost/",
                    "http://tauri.localhost/",
                ],
            )
        };
        for own in own {
            assert!(is_app_address(&url(own), None), "{own}");
        }
        for other in foreign {
            assert!(!is_app_address(&url(other), None), "{other}");
            assert!(!is_app_address(&url(other), Some(&dev)), "{other}");
        }
        for other in [
            "https://example.com/",
            "http://localhost:1420/",
            "http://localhost/",
            "http://127.0.0.1/",
            "http://tauri.localhost:8080/",
            "http://tauri.localhost.example.com/",
            "tauri://localhost:80/",
            "tauri://evil/",
            "file:///etc/passwd",
            "data:text/html,<script>1</script>",
            "about:blank",
            "javascript:alert(1)",
        ] {
            assert!(!is_app_address(&url(other), None), "{other}");
        }
        // The development server counts in a debug build only, and only on its own port.
        assert!(is_app_address(
            &url("http://localhost:1420/settings/ai"),
            Some(&dev)
        ));
        assert!(!is_app_address(&url("http://localhost:1421/"), Some(&dev)));
        assert!(!is_app_address(&url("https://localhost:1420/"), Some(&dev)));
        assert!(!is_app_address(
            &url("http://example.com:1420/"),
            Some(&dev)
        ));
    }
}
