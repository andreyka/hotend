//! `hotend key` and `hotend printer access-code` with the file store
//! (`HOTEND_SECRETS=file`): keys are stored in, reported from and removed from a file in a
//! temporary folder, a key in the environment wins for reading, and a file others could
//! read is refused with the command that fixes it. No key is ever printed. Nothing here
//! needs the engine, and nothing outside the temporary folder is read or written.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Output, Stdio};

const TAVILY: &str = "tvly-filetest-0a1b2c3d4e5f";
const CODE: &str = "c0de1234";

/// A `hotend` command whose whole configuration points into `tmp`.
fn hotend(tmp: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hotend"));
    command.arg("--data-dir").arg(tmp.join("data")).args(args);
    for name in [
        "HOTEND_DATA_DIR",
        "HOTEND_CONFIG_DIR",
        "HOTEND_BAMBU_STUDIO",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "OPENROUTER_API_KEY",
        "KEENABLE_API_KEY",
        "TAVILY_API_KEY",
        "BAMBU_LAN_ACCESS_CODE",
    ] {
        command.env_remove(name);
    }
    command
        .env("HOME", tmp.join("home"))
        .env("XDG_CONFIG_HOME", tmp.join("home/.config"))
        .env("HOTEND_SECRETS", "file")
        .env("HOTEND_SECRETS_FILE", tmp.join("keys/secrets.json"));
    command
}

fn run(mut command: Command, input: Option<&str>) -> Output {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("cannot run hotend");
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let all = text(&output);
    assert!(!all.contains(TAVILY) && !all.contains(CODE), "{all}");
    output
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn keys_are_stored_reported_and_removed_in_the_secrets_file() {
    let tmp = tempfile::tempdir().unwrap();
    let tmp = tmp.path();
    let file = tmp.join("keys/secrets.json");

    let stored = run(
        hotend(tmp, &["key", "set", "tavily"]),
        Some(&format!("{TAVILY}\n")),
    );
    assert_eq!(stored.status.code(), Some(0), "{}", text(&stored));
    assert!(text(&stored).contains("secrets file"), "{}", text(&stored));
    assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::metadata(file.parent().unwrap()).unwrap().mode() & 0o777,
        0o700
    );

    let status = run(hotend(tmp, &["key", "status", "--json"]), None);
    assert_eq!(status.status.code(), Some(0), "{}", text(&status));
    let json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(json["store"], "file");
    let tavily = json["search_services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["service"] == "tavily")
        .unwrap();
    assert_eq!(tavily["key"]["state"], "set", "{json}");
    let words = text(&run(hotend(tmp, &["key", "status"]), None));
    assert!(
        words.contains("secrets file entry search-key.tavily"),
        "{words}"
    );

    // A key in the environment wins for reading, and the status says so.
    let mut with_env = hotend(tmp, &["key", "status"]);
    with_env.env("TAVILY_API_KEY", "tvly-from-environment-0000");
    let words = text(&run(with_env, None));
    assert!(
        words.contains("TAVILY_API_KEY (environment; wins over the file)"),
        "{words}"
    );

    // The printer's access code goes to the same file.
    let code = run(
        hotend(tmp, &["printer", "access-code", "set"]),
        Some(&format!("{CODE}\n")),
    );
    assert_eq!(code.status.code(), Some(0), "{}", text(&code));
    let where_code = text(&run(
        hotend(tmp, &["printer", "access-code", "status"]),
        None,
    ));
    assert!(
        where_code.contains("stored in the key store"),
        "{where_code}"
    );

    let removed = run(hotend(tmp, &["key", "delete", "tavily"]), None);
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed));
    let json: serde_json::Value =
        serde_json::from_slice(&run(hotend(tmp, &["key", "status", "--json"]), None).stdout)
            .unwrap();
    let tavily = json["search_services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["service"] == "tavily")
        .unwrap();
    assert_eq!(tavily["key"]["state"], "not_set", "{json}");
    assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
}

#[test]
fn a_secrets_file_others_can_read_is_refused_with_the_fix_and_left_as_it_is() {
    let tmp = tempfile::tempdir().unwrap();
    let tmp = tmp.path();
    let file = tmp.join("keys/secrets.json");
    let stored = run(
        hotend(tmp, &["key", "set", "tavily"]),
        Some(&format!("{TAVILY}\n")),
    );
    assert_eq!(stored.status.code(), Some(0), "{}", text(&stored));
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

    let refused = run(
        hotend(tmp, &["key", "set", "tavily"]),
        Some(&format!("{TAVILY}\n")),
    );
    assert_ne!(refused.status.code(), Some(0), "{}", text(&refused));
    assert!(text(&refused).contains("chmod 600"), "{}", text(&refused));
    let status = text(&run(hotend(tmp, &["key", "status"]), None));
    assert!(status.contains("chmod 600"), "{status}");
    assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o644);
}

#[test]
fn deleting_a_key_the_environment_still_supplies_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let tmp = tmp.path();
    let stored = run(
        hotend(tmp, &["key", "set", "anthropic"]),
        Some(&format!("{TAVILY}\n")),
    );
    assert_eq!(stored.status.code(), Some(0), "{}", text(&stored));
    let mut delete = hotend(tmp, &["key", "delete", "anthropic"]);
    delete.env("ANTHROPIC_API_KEY", "sk-ant-from-environment-0000");
    let removed = run(delete, None);
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed));
    let words = text(&removed);
    assert!(
        words.contains("removed the anthropic key from the secrets file"),
        "{words}"
    );
    assert!(
        words.contains("still read from ANTHROPIC_API_KEY"),
        "{words}"
    );
    assert!(!words.contains("sk-ant-from-environment"), "{words}");
    // Without the variable, the plain message.
    let again = text(&run(hotend(tmp, &["key", "delete", "anthropic"]), None));
    assert!(!again.contains("still read"), "{again}");
}

#[test]
fn the_help_names_the_file_store() {
    let tmp = tempfile::tempdir().unwrap();
    let help = run(hotend(tmp.path(), &["key", "set", "--help"]), None);
    assert!(text(&help).contains("file"), "{}", text(&help));
    assert!(text(&help).contains("HOTEND_SECRETS"), "{}", text(&help));
}
