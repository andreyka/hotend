//! Helpers shared by the integration tests.
//!
//! The tests drive the real launcher through the library API. Children are Python scripts run
//! by the repository's interpreter, with its environment and base installation as the only
//! extra read-only paths.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use hotend_sandbox::{Policy, Profile};

/// Defines `attempt(name, fn)`, which prints `name=ALLOWED`, or `name=DENIED:...` when the
/// operating system refused. Any other exception is a bug in the test script and prints
/// `name=ERROR:...`, which no assertion accepts.
pub const PRELUDE: &str = r#"
import ctypes, errno, os, signal, socket, sys, time
libc = ctypes.CDLL(None, use_errno=True)
def attempt(name, fn):
    try:
        fn()
        print(f"{name}=ALLOWED", flush=True)
    except OSError as e:
        print(f"{name}=DENIED:{type(e).__name__}:{e.errno}", flush=True)
    except BaseException as e:
        print(f"{name}=ERROR:{type(e).__name__}:{e}", flush=True)
def syscall(*args):
    ctypes.set_errno(0)
    result = libc.syscall(*args)
    if result < 0:
        raise OSError(ctypes.get_errno(), os.strerror(ctypes.get_errno()))
    return result
"#;

/// The interpreter used for test children: `HOTEND_TEST_PYTHON`, or the repository's
/// environment.
pub fn python() -> PathBuf {
    if let Some(path) = std::env::var_os("HOTEND_TEST_PYTHON") {
        return PathBuf::from(path);
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python/.venv/bin/python");
    assert!(
        path.exists(),
        "no Python at {}; run scripts/setup-wsl.sh or set HOTEND_TEST_PYTHON",
        path.display()
    );
    path
}

/// The interpreter's environment and its real installation prefix. A uv environment's
/// `python` is a symlink to a base interpreter elsewhere, which must be readable too.
pub fn python_read_only_paths() -> Vec<PathBuf> {
    static PATHS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    PATHS
        .get_or_init(|| {
            let output = Command::new(python())
                .args([
                    "-I",
                    "-c",
                    "import sys; print(sys.prefix); print(sys.base_prefix)",
                ])
                .output()
                .expect("cannot run the test Python");
            assert!(output.status.success(), "the test Python does not start");
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(PathBuf::from)
                .collect()
        })
        .clone()
}

/// The machine's probe report, taken once.
pub fn report() -> &'static hotend_sandbox::ProbeReport {
    static REPORT: OnceLock<hotend_sandbox::ProbeReport> = OnceLock::new();
    REPORT.get_or_init(|| hotend_sandbox::probe().expect("probe failed"))
}

/// False, with a printed reason, when this machine cannot run the sandbox at all.
///
/// A machine like that passes the suite without having tested anything. Where that must
/// not go unnoticed (continuous integration), set `HOTEND_TEST_REQUIRE_SANDBOX=1`: the
/// tests then fail instead of skipping.
pub fn sandbox_available(test: &str) -> bool {
    let report = report();
    if !report.usable {
        let reason = report.reason.as_deref().unwrap_or("unknown");
        assert!(
            std::env::var_os("HOTEND_TEST_REQUIRE_SANDBOX").is_none_or(|value| value != "1"),
            "{test}: HOTEND_TEST_REQUIRE_SANDBOX=1, but the sandbox cannot run here: {reason}"
        );
        eprintln!("SKIPPED {test}: this machine lacks a required mechanism: {reason}");
    }
    report.usable
}

/// The layer combinations to test: with the namespace layer (when the kernel has it) and
/// with the mandatory layers alone.
pub fn namespace_modes() -> Vec<bool> {
    if report().namespaces {
        vec![true, false]
    } else {
        eprintln!(
            "NOTE: namespace layer unavailable ({}); testing Landlock and seccomp alone",
            report().namespaces_detail.as_deref().unwrap_or("unknown")
        );
        vec![false]
    }
}

/// A policy for `job` that can run the test Python.
pub fn policy(profile: Profile, job: &Path, namespaces: bool) -> Policy {
    let mut policy = Policy::new(profile, job);
    policy.read_only = python_read_only_paths();
    policy.namespaces = namespaces;
    policy
}

pub struct Run {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

impl Run {
    fn from_output(output: Output, started: Instant) -> Self {
        Self {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            elapsed: started.elapsed(),
        }
    }

    /// Asserts that the script ran to its end and that every named attempt was denied.
    pub fn assert_denied(&self, names: &[&str]) {
        assert_eq!(self.code, Some(0), "script failed: {self}");
        for name in names {
            assert!(
                self.stdout
                    .lines()
                    .any(|line| line.starts_with(&format!("{name}=DENIED"))),
                "`{name}` was not denied: {self}"
            );
        }
    }

    pub fn assert_allowed(&self, names: &[&str]) {
        assert_eq!(self.code, Some(0), "script failed: {self}");
        for name in names {
            assert!(
                self.stdout
                    .lines()
                    .any(|line| line == format!("{name}=ALLOWED")),
                "`{name}` was not allowed: {self}"
            );
        }
    }
}

impl std::fmt::Display for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exit code {:?} after {:?}\n--- stdout\n{}\n--- stderr\n{}",
            self.code, self.elapsed, self.stdout, self.stderr
        )
    }
}

/// The sandboxed command for a Python script given as source text.
pub fn python_command(policy: &Policy, script: &str) -> Command {
    let code = format!("{PRELUDE}\n{script}");
    let mut command = hotend_sandbox::command(policy, python(), ["-I", "-c", code.as_str()])
        .expect("cannot build the sandboxed command");
    command.stdin(Stdio::null());
    command
}

pub fn run(mut command: Command) -> Run {
    let started = Instant::now();
    let output = command.output().expect("cannot start the launcher");
    let run = Run::from_output(output, started);
    // `HOTEND_TEST_SHOW=1 cargo test -- --nocapture` shows what every child printed.
    if std::env::var_os("HOTEND_TEST_SHOW").is_some() {
        eprintln!("{run}");
    }
    run
}

pub fn run_python(policy: &Policy, script: &str) -> Run {
    run(python_command(policy, script))
}

/// Ids of live processes whose command line mentions `marker`.
pub fn processes_mentioning(marker: &str) -> Vec<u32> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if String::from_utf8_lossy(&cmdline).contains(marker) {
            // Zombies keep their command line until reaped but hold no resources.
            let state = std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
            let zombie = state
                .rsplit(')')
                .next()
                .is_some_and(|rest| rest.trim_start().starts_with('Z'));
            if !zombie && pid != std::process::id() {
                found.push(pid);
            }
        }
    }
    found
}

/// Asserts that no process mentioning `marker` is left, allowing a moment for kills to land.
pub fn assert_no_processes_left(marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = processes_mentioning(marker);
        if left.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "processes survived the sandbox: {left:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Waits until `path` exists.
pub fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---- Other users and what they run -----------------------------------------------------

/// Whether the tests run as root, and can therefore run a job as another user.
pub fn is_root() -> bool {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() == 0 }
}

/// Makes `command` run as the unprivileged user `uid` (the tests must run as root).
pub fn run_as(command: &mut Command, uid: u32) {
    use std::os::unix::process::CommandExt;
    command.uid(uid).gid(uid);
}

/// Gives the job folder to `uid`, so a job that runs as that user can write to it.
pub fn give_to(path: &Path, uid: u32) {
    std::os::unix::fs::chown(path, Some(uid), Some(uid)).unwrap();
}

/// The probe report as the launcher prints it for `uid`, or for the tests' own user.
pub fn probe_as(uid: Option<u32>) -> hotend_sandbox::ProbeReport {
    let Some(uid) = uid else {
        return report().clone();
    };
    let mut command = Command::new(env!("CARGO_BIN_EXE_hotend-sandbox"));
    command.arg("probe").stdin(Stdio::null());
    run_as(&mut command, uid);
    let output = command.output().expect("cannot run the probe");
    assert!(output.status.success(), "probe failed: {output:?}");
    serde_json::from_slice(&output.stdout).expect("the probe did not print its report")
}

/// One process from `/proc`.
struct Process {
    pid: i32,
    parent: i32,
    group: i32,
    name: String,
    zombie: bool,
    threads: u64,
}

/// Every process the tests can see, from one read of its `stat` file each.
fn processes() -> Vec<Process> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `pid (name) state ppid pgrp ...`; the thread count is field 20.
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let fields: Vec<&str> = stat[close + 1..].split_ascii_whitespace().collect();
        let number = |index: usize| fields.get(index).and_then(|n| n.parse::<i64>().ok());
        let (Some(parent), Some(group)) = (number(1), number(2)) else {
            continue;
        };
        found.push(Process {
            pid,
            parent: parent as i32,
            group: group as i32,
            name: stat[open + 1..close].to_owned(),
            zombie: fields.first() == Some(&"Z"),
            threads: number(17).unwrap_or(1).max(1) as u64,
        });
    }
    found
}

/// The process group of the job the launcher `launcher` runs: the id of the job's first
/// process, which is the launcher's only child besides its watchdog.
pub fn job_group(launcher: u32) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let first = processes()
            .into_iter()
            .find(|p| p.parent == launcher as i32 && p.name != "hotend-sbx-wd");
        if let Some(first) = first {
            return first.pid;
        }
        assert!(Instant::now() < deadline, "launcher {launcher} has no job");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Live tasks (processes and threads) in the process group `group`: what the launcher
/// counts against `max_processes`. With namespaces the init process of the job's PID
/// namespace is one of them.
pub fn group_tasks(group: i32) -> u64 {
    processes()
        .into_iter()
        .filter(|p| p.group == group && !p.zombie)
        .map(|p| p.threads)
        .sum()
}

/// Everything the kernel counts for the user `uid`: live tasks and processes that have
/// exited but were not collected yet.
pub fn user_tasks(uid: u32) -> u64 {
    use std::os::unix::fs::MetadataExt;
    processes()
        .into_iter()
        .filter(|p| {
            std::fs::metadata(format!("/proc/{}", p.pid)).is_ok_and(|meta| meta.uid() == uid)
        })
        .map(|p| if p.zombie { 1 } else { p.threads })
        .sum()
}

/// The soft and hard process limit (`RLIMIT_NPROC`) of the process `pid`, from
/// `/proc/<pid>/limits`; `u64::MAX` stands for "unlimited".
pub fn process_limit(pid: i32) -> Option<(u64, u64)> {
    let limits = std::fs::read_to_string(format!("/proc/{pid}/limits")).ok()?;
    let line = limits
        .lines()
        .find_map(|line| line.strip_prefix("Max processes"))?;
    let mut numbers = line
        .split_ascii_whitespace()
        .map(|field| field.parse().unwrap_or(u64::MAX));
    Some((numbers.next()?, numbers.next()?))
}

/// The process limit the tests themselves run under (`ulimit -u`).
pub fn own_process_limit() -> u64 {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: passes a valid rlimit struct for the kernel to fill.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NPROC, &mut limit) },
        0
    );
    limit.rlim_cur
}

/// Waits until the user `uid` runs nothing, so that a test starts from a known count.
/// Killed processes of an earlier job can wait a second or two for the machine's first
/// process to collect them.
pub fn wait_until_idle(uid: u32) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while user_tasks(uid) != 0 {
        assert!(
            Instant::now() < deadline,
            "user {uid} still runs {} tasks",
            user_tasks(uid)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A process outside the sandbox that keeps forking, as any other program of the user
/// would. It counts the forks that worked and the ones the kernel refused, in total and
/// while the file `window` exists.
pub struct Sibling {
    child: std::process::Child,
    dir: tempfile::TempDir,
}

/// What a [`Sibling`] saw: forks that worked and forks that were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Forks {
    pub worked: u64,
    pub refused: u64,
    pub worked_in_window: u64,
}

impl Sibling {
    /// Starts the process as `uid` (or as the tests' user) and waits for its first fork.
    pub fn start(uid: Option<u32>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            dir.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let script = r#"
import os, sys, time
stop, window, ready = sys.argv[1:4]
worked = refused = in_window = 0
while not os.path.exists(stop):
    try:
        pid = os.fork()
        if pid == 0:
            os._exit(0)
        os.waitpid(pid, 0)
        worked += 1
        in_window += os.path.exists(window)
    except OSError:
        refused += 1
    if worked == 1:
        open(ready, "w").close()
    time.sleep(0.002)
print(worked, refused, in_window)
"#;
        let mut command = Command::new(python());
        command
            .args(["-I", "-c", script])
            .arg(dir.path().join("stop"))
            .arg(dir.path().join("window"))
            .arg(dir.path().join("ready"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        if let Some(uid) = uid {
            give_to(dir.path(), uid);
            run_as(&mut command, uid);
        }
        let child = command.spawn().expect("cannot start the sibling process");
        wait_for(&dir.path().join("ready"));
        Self { child, dir }
    }

    /// Marks the start of the interval that `worked_in_window` counts.
    pub fn open_window(&self) {
        std::fs::write(self.dir.path().join("window"), "").unwrap();
    }

    pub fn finish(self) -> Forks {
        std::fs::write(self.dir.path().join("stop"), "").unwrap();
        let output = self.child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        let numbers: Vec<u64> = text
            .split_ascii_whitespace()
            .map(|n| n.parse().unwrap())
            .collect();
        assert_eq!(numbers.len(), 3, "the sibling process printed {text:?}");
        Forks {
            worked: numbers[0],
            refused: numbers[1],
            worked_in_window: numbers[2],
        }
    }
}

/// Samples a number on another thread until told to stop and keeps the largest value.
pub struct Peak {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: std::thread::JoinHandle<u64>,
}

impl Peak {
    pub fn watch(sample: impl Fn() -> u64 + Send + 'static) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut peak = 0;
            while !stopped.load(std::sync::atomic::Ordering::Relaxed) {
                peak = peak.max(sample());
            }
            peak.max(sample())
        });
        Self { stop, thread }
    }

    pub fn finish(self) -> u64 {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.thread.join().unwrap()
    }
}
