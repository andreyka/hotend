//! The workloads the sandbox exists for, run for real: build123d on OpenCascade under the
//! engine profile and the agent's Python stack under the agent profile.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::Stdio;

use common::*;
use hotend_sandbox::Profile;
use hotend_sandbox::error::EXIT_LIMIT;

#[test]
fn build123d_runs_under_the_engine_profile_and_writes_step() {
    if !sandbox_available("build123d_runs_under_the_engine_profile_and_writes_step") {
        return;
    }
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let policy = policy(Profile::Engine, job.path(), namespaces);
        let code = "from build123d import *; b=Box(10,20,30); print(b.volume); \
                    export_step(b, 'box.step'); export_stl(b, 'box.stl')";
        let mut command = hotend_sandbox::command(&policy, python(), ["-I", "-c", code]).unwrap();
        let run = run({
            command.stdin(Stdio::null());
            command
        });
        assert_eq!(run.code, Some(0), "{run}");
        assert_eq!(run.stdout.trim(), "6000.0", "{run}");
        let step = std::fs::read_to_string(job.path().join("box.step")).unwrap();
        assert!(
            step.starts_with("ISO-10303-21;"),
            "not a STEP file: {step:.60}"
        );
        assert!(
            step.contains("MANIFOLD_SOLID_BREP"),
            "no solid in the STEP file"
        );
        assert!(std::fs::metadata(job.path().join("box.stl")).unwrap().len() > 100);
    }
}

#[test]
fn agent_stack_runs_under_the_agent_profile() {
    if !sandbox_available("agent_stack_runs_under_the_agent_profile") {
        return;
    }
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let policy = policy(Profile::Agent, job.path(), namespaces);
        // An asyncio loop that reads a request from the stdin pipe and answers on stdout,
        // with a Pydantic AI agent on its scripted test model in between.
        let script = r#"
import asyncio, json
from pydantic_ai import Agent
from pydantic_ai.models.test import TestModel

async def main():
    loop = asyncio.get_running_loop()
    reader = asyncio.StreamReader()
    await loop.connect_read_pipe(lambda: asyncio.StreamReaderProtocol(reader), sys.stdin)
    request = json.loads(await reader.readline())
    result = await Agent(TestModel()).run(request["text"])
    print(json.dumps({"id": request["id"], "output": result.output}), flush=True)

asyncio.run(main())
"#;
        let mut command = python_command(&policy, script);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        std::io::Write::write_all(
            &mut child.stdin.take().unwrap(),
            b"{\"id\": 7, \"text\": \"hello\"}\n",
        )
        .unwrap();
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(reply["id"], 7, "{reply}");
        assert!(reply["output"].is_string(), "{reply}");
    }
}

/// The product runs as an ordinary user; this machine's tests may run as root. When they
/// do, repeat the essentials as an unprivileged user, where the kernel also enforces the
/// process cap inside the user namespace.
#[test]
fn an_unprivileged_user_gets_the_same_confinement() {
    if !sandbox_available("an_unprivileged_user_gets_the_same_confinement") {
        return;
    }
    // SAFETY: getuid cannot fail.
    if unsafe { libc_getuid() } != 0 {
        eprintln!(
            "SKIPPED an_unprivileged_user_gets_the_same_confinement: \
             already running unprivileged, so every other test covers this"
        );
        return;
    }
    const NOBODY: u32 = 65534;
    for namespaces in namespace_modes() {
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let secret = home.path().join("secret.txt");
        std::fs::write(&secret, "PRIVATE").unwrap();
        std::os::unix::fs::chown(&secret, Some(NOBODY), Some(NOBODY)).unwrap();
        std::os::unix::fs::chown(home.path(), Some(NOBODY), Some(NOBODY)).unwrap();

        let job = tempfile::tempdir().unwrap();
        std::os::unix::fs::chown(job.path(), Some(NOBODY), Some(NOBODY)).unwrap();
        let mut policy = policy(Profile::Engine, job.path(), namespaces);
        policy.limits.max_processes = 64;
        policy.limits.wall_seconds = 60;

        let script = format!(
            r#"
assert os.getuid() == {NOBODY}, os.getuid()
secret = {secret:?}
attempt("read_secret", lambda: open(secret).read())
attempt("write_outside", lambda: open(os.path.dirname(secret) + "/planted", "w"))
attempt("tcp_socket", lambda: socket.socket(socket.AF_INET, socket.SOCK_STREAM))
attempt("unshare_user", lambda: os.unshare(os.CLONE_NEWUSER))
os.chmod(secret, 0o666)
with open("result.txt", "w") as f:
    f.write("ok")
print("inside=ALLOWED", flush=True)
while True:
    try:
        os.fork()
    except OSError:
        time.sleep(0.01)
"#,
            secret = secret.to_str().unwrap()
        );
        let mut command = python_command(&policy, &script);
        command.uid(NOBODY).gid(NOBODY);
        let run = run(command);
        assert_eq!(run.code, Some(EXIT_LIMIT), "{run}");
        assert!(run.stderr.contains("process limit of 64 reached"), "{run}");
        for name in ["read_secret", "write_outside", "tcp_socket", "unshare_user"] {
            assert!(
                run.stdout.contains(&format!("{name}=DENIED")),
                "{name}: {run}"
            );
        }
        assert!(run.stdout.contains("inside=ALLOWED"), "{run}");
        assert_eq!(
            std::fs::read_to_string(job.path().join("result.txt")).unwrap(),
            "ok"
        );
        let mode = std::fs::metadata(&secret).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "the secret's mode was changed");
        assert!(!home.path().join("planted").exists());
    }
}

unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid has no preconditions.
    unsafe { getuid() }
}
