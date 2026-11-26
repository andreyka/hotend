//! `HOTEND_UNSAFE_NO_SANDBOX=1` is visible in the audit log. Alone in this file because it
//! changes the environment of the test process.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use hotend_core::CoreConfig;
use hotend_core::gateway::RunSession;
use serde_json::json;

#[tokio::test]
async fn a_job_that_ran_without_the_sandbox_says_so_in_the_audit_log() {
    let test = "a_job_that_ran_without_the_sandbox_says_so_in_the_audit_log";
    let Some(t) = common::open_core(test, CoreConfig::default()).await else {
        return;
    };
    let project = t.core.project_create("unsandboxed").await.unwrap();
    let call = async |run: &str| {
        let session = RunSession::new(run, &project.id);
        let listed = t
            .core
            .gateway()
            .call(&session, "list_cards", json!({}))
            .await;
        assert!(listed.error.is_none(), "{listed:?}");
        let audit = t.core.audit_log(Some(&project.id), 20).await.unwrap();
        audit
            .into_iter()
            .find(|a| a.kind == "job" && a.run_id.as_deref() == Some(run))
            .expect("the job must be audited")
    };

    let confined = call("confined").await;
    assert_eq!(confined.detail["sandbox"]["mode"], "on", "{confined:?}");
    assert_eq!(confined.detail["sandbox"]["launcher_overridden"], false);
    assert!(confined.detail["sandbox"]["launcher"].is_string());

    // SAFETY: this file has a single test, and no job is running: nothing else reads the
    // environment now.
    unsafe {
        std::env::set_var("HOTEND_UNSAFE_NO_SANDBOX", "1");
    }
    let job = call("override").await;
    // Only a debug build honours the override; a release build stays confined.
    let expected = if cfg!(debug_assertions) { "off" } else { "on" };
    assert_eq!(job.detail["sandbox"]["mode"], expected, "{job:?}");
}
