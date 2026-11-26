//! `HOTEND_UNSAFE_NO_SANDBOX=1`, end to end. Alone in this file because it changes the
//! environment of the test process.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use hotend_sandbox::Profile;

#[test]
fn unsafe_override_removes_the_sandbox_in_debug_builds_only() {
    if !sandbox_available("unsafe_override_removes_the_sandbox_in_debug_builds_only") {
        return;
    }
    let job = tempfile::tempdir().unwrap();
    let policy = policy(Profile::Engine, job.path(), true);
    // SAFETY: this file has a single test, so no other thread reads the environment.
    unsafe {
        std::env::set_var(hotend_sandbox::UNSAFE_ENV, "1");
        std::env::set_var("ANTHROPIC_API_KEY", "not-a-real-key");
    }
    let run = run_python(
        &policy,
        r#"
attempt("socket", lambda: socket.socket(socket.AF_INET, socket.SOCK_STREAM))
print("key visible:", "ANTHROPIC_API_KEY" in os.environ)
print("cwd:", os.getcwd())
"#,
    );
    assert_eq!(run.code, Some(0), "{run}");
    // Even unconfined, the child gets the clean environment and the job folder.
    assert!(run.stdout.contains("key visible: False"), "{run}");
    assert!(
        run.stdout.contains(&format!(
            "cwd: {}",
            job.path().canonicalize().unwrap().display()
        )),
        "{run}"
    );
    if cfg!(debug_assertions) {
        assert!(
            run.stdout.contains("socket=ALLOWED"),
            "the override was not honoured: {run}"
        );
    } else {
        assert!(
            run.stdout.contains("socket=DENIED"),
            "release build ran unconfined: {run}"
        );
    }
}
