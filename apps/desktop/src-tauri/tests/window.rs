//! What the window is allowed to do, read from the files that decide it: the capability,
//! the Tauri configuration, the manifest and the build guard. These tests fail when one of
//! them is loosened.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test stops at the first surprise"
)]

use std::collections::BTreeSet;
use std::path::Path;

use hotend_desktop::{app, ipc};
use serde_json::Value;

include!("../build/command_names.rs");
include!("../build/guard.rs");

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn json(relative: &str) -> Value {
    serde_json::from_str(&read(relative)).unwrap()
}

/// Commands of the table the window is not granted: the interface has no screen that
/// calls them. Granting one again goes with the screen that uses it.
const NOT_GRANTED: &[&str] = &[
    "versions_list",
    "questions_list",
    "measure_run",
    "checks_run",
    "views_render",
    "audit_log",
];

/// The interface's own source files (src/), without the generated bindings.
fn interface_sources() -> String {
    fn walk(dir: &Path, out: &mut String) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path
                .extension()
                .is_some_and(|ext| ext == "ts" || ext == "tsx")
                && path.file_name().is_some_and(|name| name != "bindings.ts")
            {
                out.push_str(&std::fs::read_to_string(&path).unwrap());
                out.push('\n');
            }
        }
    }
    let mut out = String::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../src"),
        &mut out,
    );
    out
}

#[test]
fn a_command_is_granted_exactly_when_the_interface_calls_it() {
    let sources = interface_sources();
    assert!(
        sources.contains(".project_get("),
        "the interface sources were not found"
    );
    for name in ipc::command_names() {
        // `client.name(` in the screens; `await name(` where tauri.ts unpacks the commands.
        let called =
            sources.contains(&format!(".{name}(")) || sources.contains(&format!(" {name}("));
        let granted = !NOT_GRANTED.contains(&name);
        assert_eq!(
            called, granted,
            "`{name}`: called by the interface {called}, granted to the window {granted}"
        );
    }
}

#[test]
fn the_capability_grants_exactly_the_commands() {
    // Three lists must be one: the command table, the names the build script turns into
    // permissions, and the permissions the capability grants.
    let table: Vec<&str> = ipc::command_names();
    assert_eq!(
        table.len(),
        table.iter().collect::<BTreeSet<_>>().len(),
        "a command is listed twice"
    );
    assert_eq!(
        COMMAND_NAMES,
        table.as_slice(),
        "build/command_names.rs and the table in ipc.rs differ"
    );

    let capability = json("capabilities/main.json");
    let granted: Vec<&str> = capability["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|permission| {
            permission
                .as_str()
                .expect("a permission is a plain name, without a scope")
        })
        .collect();
    // Every command but those the interface never calls (the webview gets
    // only what it needs). They stay methods of the facade and commands of the CLI.
    let expected: Vec<String> = table
        .iter()
        .filter(|name| !NOT_GRANTED.contains(name))
        .map(|name| format!("allow-{}", name.replace('_', "-")))
        .collect();
    assert_eq!(granted, expected);
    // A permission of a plugin or of Tauri's core is written `<plugin>:<name>`. There is
    // none: no shell, no filesystem, no dialog, no opener, no window or event API.
    assert!(
        granted.iter().all(|permission| !permission.contains(':')),
        "{granted:?}"
    );
}

#[test]
fn the_capability_is_for_the_main_window_only() {
    let capability = json("capabilities/main.json");
    assert_eq!(capability["windows"], serde_json::json!([app::MAIN_WINDOW]));
    assert_eq!(app::MAIN_WINDOW, "main");
    for absent in ["webviews", "remote", "platforms"] {
        assert!(
            capability.get(absent).is_none(),
            "the capability has `{absent}`"
        );
    }
    // It is the only capability, and the configuration names it.
    let files: Vec<_> =
        std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
    assert_eq!(files, ["main.json"]);
    let config = json("tauri.conf.json");
    assert_eq!(
        config["app"]["security"]["capabilities"],
        serde_json::json!(["main"])
    );
}

#[test]
fn the_configuration_keeps_the_window_closed() {
    let config = json("tauri.conf.json");
    let app = &config["app"];
    // The window is made in Rust (app.rs), where its navigation is restricted; none is
    // declared here that would skip that.
    assert_eq!(app["windows"], serde_json::json!([]));
    assert_eq!(app["withGlobalTauri"], false);
    let security = &app["security"];
    assert_eq!(security["freezePrototype"], true);
    assert_eq!(security["assetProtocol"]["enable"], false);
    assert!(
        security
            .get("dangerousDisableAssetCspModification")
            .is_none()
    );
    assert!(
        security.get("devCsp").is_none(),
        "the development server sends the window's policy itself (csp.ts, devPolicy)"
    );

    let csp = security["csp"]
        .as_object()
        .expect("the policy is written directive by directive");
    assert_eq!(csp["default-src"], "'none'");
    assert_eq!(csp["script-src"], "'self'");
    assert_eq!(csp["connect-src"], "ipc: http://ipc.localhost");
    for (directive, sources) in csp {
        let sources = sources.as_str().unwrap();
        assert!(!sources.contains("unsafe-eval"), "{directive}");
        assert!(
            !sources.contains("https:") && !sources.contains("ws:") && !sources.contains('*'),
            "{directive}"
        );
        if directive != "style-src-attr" {
            assert!(!sources.contains("unsafe-inline"), "{directive}");
        }
    }
}

#[test]
fn the_manifest_has_no_devtools_no_shell_and_no_bridge_by_default() {
    let manifest = read("Cargo.toml");
    let code: String = manifest
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    // Tauri's inspector exists in a release build only with the `devtools` feature.
    assert!(!code.contains("devtools"));
    for plugin in [
        "tauri-plugin-shell",
        "tauri-plugin-fs",
        "tauri-plugin-http",
        "tauri-plugin-process",
    ] {
        assert!(!code.contains(plugin), "{plugin} is a dependency");
    }
    assert!(code.contains("default = []"), "no feature is on by default");
    assert!(
        code.contains("required-features = [\"dev-bridge\"]"),
        "hotend-bridge needs the feature"
    );

    // One plugin is registered: the dialogs the shell itself draws. Its commands are not
    // granted to the window (see the capability).
    let app = read("src/app.rs");
    let registered: Vec<&str> = app
        .lines()
        .filter(|line| line.contains(".plugin("))
        .map(str::trim)
        .collect();
    assert_eq!(registered, [".plugin(tauri_plugin_dialog::init())"]);
    assert!(!app.contains("open_devtools"));
}

#[test]
fn a_release_build_cannot_contain_the_bridge() {
    // The build script stops with this error before anything of the crate is compiled.
    assert!(
        bridge_allowed("release", true)
            .unwrap_err()
            .contains("never")
    );
    assert!(bridge_allowed("bench", true).is_err());
    assert!(bridge_allowed("debug", true).is_ok());
    assert!(bridge_allowed("release", false).is_ok());
    assert!(bridge_allowed("debug", false).is_ok());
    let build = read("build.rs");
    assert!(build.contains("bridge_allowed("), "build.rs asks the guard");
    // And a second lock in the crate itself, for a profile that is optimised under
    // another name.
    assert!(
        read("src/lib.rs").contains("#[cfg(all(feature = \"dev-bridge\", not(debug_assertions)))]")
    );
}

#[test]
fn every_icon_of_the_configuration_exists_for_every_platform() {
    // tauri-build stops a Windows build without an .ico, and a macOS bundle needs an
    // .icns.
    let config = json("tauri.conf.json");
    let icons: Vec<&str> = config["bundle"]["icon"]
        .as_array()
        .unwrap()
        .iter()
        .map(|icon| icon.as_str().unwrap())
        .collect();
    let bytes = |relative: &str| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    };
    for icon in &icons {
        let content = bytes(icon);
        let magic: &[u8] = match icon.rsplit('.').next().unwrap() {
            "png" => b"\x89PNG\r\n\x1a\n",
            "ico" => &[0, 0, 1, 0],
            "icns" => b"icns",
            other => panic!("{icon}: an icon of an unknown kind `{other}`"),
        };
        assert!(
            content.starts_with(magic),
            "{icon} is not what its name says"
        );
    }
    for kind in [".ico", ".icns", ".png"] {
        assert!(
            icons.iter().any(|icon| icon.ends_with(kind)),
            "no {kind} icon in {icons:?}"
        );
    }
}

#[test]
fn the_bundle_puts_python_and_the_launcher_where_the_application_looks() {
    // The bundle configuration is its own file: its sources exist only after the
    // packaging steps, and tauri-build would stop every other build without them.
    let dev = json("tauri.conf.json");
    assert_eq!(dev["bundle"]["active"], false);
    assert!(dev["bundle"].get("externalBin").is_none());
    assert!(dev["bundle"].get("resources").is_none());

    let bundle = json("tauri.bundle.conf.json");
    assert_eq!(bundle["bundle"]["active"], true);
    // Tauri puts an external program beside the main executable under its plain name,
    // which is where the sandbox looks for its launcher.
    assert_eq!(
        bundle["bundle"]["externalBin"],
        serde_json::json!(["binaries/hotend-sandbox"])
    );
    // The Python environment lands in the folder the core names.
    let resources = bundle["bundle"]["resources"].as_object().unwrap();
    let targets: Vec<&str> = resources
        .values()
        .map(|target| target.as_str().unwrap())
        .collect();
    assert_eq!(targets, ["python/"]);
    let python = hotend_core::PythonRuntime::packaged_interpreter(Path::new("/resources"));
    assert!(python.starts_with("/resources/python"), "{python:?}");
    // And the application asks for it.
    let app = read("src/app.rs");
    assert!(app.contains("PythonRuntime::packaged_interpreter("));
    assert!(app.contains("resource_dir()"));
}
