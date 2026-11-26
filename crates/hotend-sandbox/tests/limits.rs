//! Resource limits, the wall clock, tree cleanup and the launcher's exit codes.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::*;
use hotend_sandbox::error::EXIT_LIMIT;
use hotend_sandbox::{Policy, Profile};

const LAUNCHER: &str = env!("CARGO_BIN_EXE_hotend-sandbox");

/// Runs `script` from a file in the job folder, so that every process of the tree carries
/// the job path in its command line and stragglers can be found from outside.
fn run_script_file(policy: &Policy, script: &str) -> Run {
    run(script_file_command(policy, script))
}

fn script_file_command(policy: &Policy, script: &str) -> Command {
    let path = policy.job_dir.join("script.py");
    std::fs::write(&path, format!("{PRELUDE}\n{script}")).unwrap();
    let mut command =
        hotend_sandbox::command(policy, python(), ["-I".as_ref(), path.as_os_str()]).unwrap();
    command.stdin(Stdio::null());
    command
}

fn job_marker(policy: &Policy) -> String {
    policy.job_dir.to_str().unwrap().to_owned()
}

// ---- The process cap --------------------------------------------------------------------
//
// A fork bomb is run in every way this machine can run a job: as the tests' own user and,
// when the tests are root, as an unprivileged user as well, each with and without the
// namespace layer. Root is the case where only a cgroup can cap the count; an ordinary
// user without namespaces is the case where the cap is the user's total.
//
// The bombs cannot hurt the other tests or anything else on the machine: as root they sit
// in their own cgroup, and each test that runs one as an unprivileged user has a user id
// of its own, which no other test and no other program uses.

/// The process limit of the bombs.
const BOMB_LIMIT: u64 = 64;
/// What `nproc::OTHER_PROGRAMS_ALLOWANCE` is: how far past its limit the kernel lets a job
/// get where the cap is the user's total.
const ALLOWANCE: u64 = 1024;
/// A killed bomb is gone, with every process it made, within this time.
const GONE_WITHIN: Duration = Duration::from_secs(5);

/// Unused user ids (the ones just below `nobody`), one per test that runs a bomb.
const BOMB_USER: u32 = 65533;
const FROZEN_BOMB_USER: u32 = 65532;
const COUNTED_USER: u32 = 65531;
const DELEGATED_USER: u32 = 65530;
const SMALL_QUOTA_USER: u32 = 65529;
const ACCOUNT_USER: u32 = 65528;
const GROWING_USER: u32 = 65527;
const CROWDED_USER: u32 = 65526;

const BOMB: &str = r#"
open("started", "w").close()
while not os.path.exists("go"):
    time.sleep(0.002)
while True:
    try:
        os.fork()
    except OSError:
        time.sleep(0.01)
"#;

/// One way of running a job here.
#[derive(Debug, Clone, Copy)]
struct Mode {
    namespaces: bool,
    /// The unprivileged user the job runs as; `None` is the user the tests run as.
    uid: Option<u32>,
}

impl Mode {
    /// Every way this machine offers; `uid` is the user for the unprivileged runs.
    fn all(uid: u32) -> Vec<Self> {
        let mut modes = Vec::new();
        for namespaces in namespace_modes() {
            modes.push(Self {
                namespaces,
                uid: None,
            });
            if is_root() {
                modes.push(Self {
                    namespaces,
                    uid: Some(uid),
                });
            }
        }
        modes
    }

    /// The mechanism the probe promises for this mode.
    fn cap(self) -> hotend_sandbox::ProcessCapMode {
        let report = probe_as(self.uid);
        if self.namespaces && report.namespaces {
            report.process_cap.default
        } else {
            report.process_cap.without_namespaces
        }
    }

    fn name(self) -> String {
        format!(
            "{}, {}",
            match self.uid {
                Some(uid) => format!("uid {uid}"),
                None if is_root() => "root".to_owned(),
                None => "own user".to_owned(),
            },
            if self.namespaces {
                "namespaces"
            } else {
                "no namespaces"
            }
        )
    }
}

/// A bomb that waits for its release, with a sibling process of the same user beside it.
struct Bomb {
    job: tempfile::TempDir,
    marker: String,
    launcher: std::process::Child,
    /// The job's process group, whose tasks are what the limit counts.
    group: i32,
    sibling: Sibling,
}

impl Bomb {
    /// Starts the job and waits until its program runs. `quota` is the `ulimit -u` the
    /// launcher starts with (the sibling keeps the tests' own); `cgroup` is a cgroup the
    /// launcher moves itself into before it starts.
    fn start(mode: Mode, quota: Option<u64>, cgroup: Option<&std::path::Path>) -> Self {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::process::CommandExt;
        if let Some(uid) = mode.uid {
            wait_until_idle(uid);
        }
        let job = tempfile::tempdir().unwrap();
        let mut policy = policy(Profile::Engine, job.path(), mode.namespaces);
        policy.limits.max_processes = BOMB_LIMIT;
        policy.limits.wall_seconds = 60;
        let mut command = script_file_command(&policy, BOMB);
        command.stdout(Stdio::null()).stderr(Stdio::piped());
        if let Some(uid) = mode.uid {
            give_to(job.path(), uid);
            run_as(&mut command, uid);
        }
        let members = cgroup.map(|dir| {
            std::ffi::CString::new(dir.join("cgroup.procs").as_os_str().as_bytes()).unwrap()
        });
        // SAFETY: the closure only makes system calls, on values prepared before the fork.
        unsafe {
            command.pre_exec(move || {
                let check = |result: isize| match result {
                    -1 => Err(std::io::Error::last_os_error()),
                    _ => Ok(()),
                };
                if let Some(quota) = quota {
                    let limit = libc::rlimit {
                        rlim_cur: quota,
                        rlim_max: quota,
                    };
                    check(libc::setrlimit(libc::RLIMIT_NPROC, &limit) as isize)?;
                }
                if let Some(members) = &members {
                    // Writing 0 moves the writing process.
                    let fd = libc::open(members.as_ptr(), libc::O_WRONLY);
                    check(fd as isize)?;
                    check(libc::write(fd, c"0".as_ptr().cast(), 1))?;
                    libc::close(fd);
                }
                Ok(())
            });
        }
        let launcher = command.spawn().unwrap();
        wait_for(&job.path().join("started"));
        Self {
            marker: job_marker(&policy),
            group: job_group(launcher.id()),
            job,
            launcher,
            sibling: Sibling::start(mode.uid),
        }
    }

    fn release(&self) {
        self.sibling.open_window();
        std::fs::write(self.job.path().join("go"), "").unwrap();
    }

    fn signal(&self, signal: i32) {
        // SAFETY: signals the launcher this test started and has not yet waited for.
        assert_eq!(unsafe { libc::kill(self.launcher.id() as i32, signal) }, 0);
    }

    /// Waits for the launcher and for the last process of the job. Returns the launcher's
    /// exit code and stderr, the time until the launcher was gone and until everything was.
    fn end(self, since: Instant) -> (Option<i32>, String, Duration, Duration, Forks) {
        let launcher = self.launcher.id();
        // Reading stderr to its end waits for every process that holds it: the whole tree.
        let output = self.launcher.wait_with_output().unwrap();
        let killed = since.elapsed();
        assert_no_processes_left(&self.marker);
        let gone = since.elapsed();
        let forks = self.sibling.finish();
        assert_no_cgroup_left(launcher);
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            killed,
            gone,
            forks,
        )
    }
}

/// The launcher removes the cgroup it made for its job.
fn assert_no_cgroup_left(launcher: u32) {
    let Some(parent) = &report().process_cap.cgroup_parent else {
        return;
    };
    let prefix = format!("hotend-sandbox-{launcher}-");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(&prefix))
            .collect();
        if left.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "cgroups left in {parent}: {left:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The task count in the launcher's "process limit of N reached (M tasks)" line.
fn reported_tasks(stderr: &str) -> u64 {
    stderr
        .split_once(" reached (")
        .and_then(|(_, rest)| rest.split_once(" tasks)"))
        .and_then(|(tasks, _)| tasks.parse().ok())
        .unwrap_or(0)
}

/// The most live tasks the bomb `bomb` with [`BOMB_LIMIT`] may ever have under `cap`.
///
/// Where the cap is on the user's total, what the kernel guarantees for the job's own
/// tasks depends on what else the user runs:
///
/// - A user of the test's own (`mode.uid`), who runs nothing but the launcher, its
///   watchdog and the sibling: the limit plus the allowance. The sibling's short-lived
///   child may have been in the count the cap was last set from and gone a moment
///   later, which is the one task more.
/// - The user the tests themselves run as, when that is an ordinary user: other tests
///   and other programs start and end tasks of that user all the time, and each task
///   that ends leaves its place to the bomb. What holds for the job then is the
///   ceiling the launcher gave it (its hard limit), which keeps it out of the part of
///   the user's own limit that is reserved for the user's other programs.
fn bound(cap: &hotend_sandbox::ProcessCapMode, mode: Mode, bomb: &Bomb) -> u64 {
    match (cap.enforcement.as_str(), mode.uid) {
        ("kernel-exact", _) => BOMB_LIMIT,
        ("kernel-approximate", Some(_)) => BOMB_LIMIT + ALLOWANCE + 1,
        ("kernel-approximate", None) => {
            let (soft, ceiling) = process_limit(bomb.group).expect("the bomb has no limits");
            let own = own_process_limit();
            assert!(soft <= ceiling, "{}: {soft} > {ceiling}", mode.name());
            assert!(
                ceiling < own || own == u64::MAX,
                "{}: the job's ceiling of {ceiling} tasks leaves nothing of the user's \
                 own limit of {own}",
                mode.name()
            );
            ceiling
        }
        (other, _) => panic!("a job ran although its process cap is `{other}`"),
    }
}

#[test]
fn fork_bomb_is_stopped() {
    if !sandbox_available("fork_bomb_is_stopped") {
        return;
    }
    for mode in Mode::all(BOMB_USER) {
        let cap = mode.cap();
        let bomb = Bomb::start(mode, None, None);
        let bound = bound(&cap, mode, &bomb);
        let group = bomb.group;
        let peak = Peak::watch(move || group_tasks(group));
        let released = Instant::now();
        bomb.release();
        let (code, stderr, killed, gone, forks) = bomb.end(released);
        // The bomb lives for a few milliseconds, which the sampling can miss; the count
        // the supervisor killed it at is exact for that moment.
        let peak = peak.finish().max(reported_tasks(&stderr));
        eprintln!(
            "MEASURED fork bomb, limit {BOMB_LIMIT} | {} | {} ({}) | peak {peak} live tasks | \
             killed after {killed:.0?}, gone after {gone:.0?} | sibling forks: {} worked, {} \
             refused",
            mode.name(),
            cap.mechanism,
            cap.enforcement,
            forks.worked,
            forks.refused
        );
        assert_eq!(code, Some(EXIT_LIMIT), "{}: {stderr}", mode.name());
        assert!(
            stderr.contains("process limit of 64 reached"),
            "{}: {stderr}",
            mode.name()
        );
        assert!(
            peak <= bound,
            "{}: {peak} live tasks under a limit of {BOMB_LIMIT}, the kernel should stop \
             at {bound} ({})",
            mode.name(),
            cap.mechanism
        );
        assert!(gone < GONE_WITHIN, "{}: gone after {gone:?}", mode.name());
        assert_eq!(forks.refused, 0, "{}: {forks:?}", mode.name());
        assert!(forks.worked > 0, "{}: {forks:?}", mode.name());
    }
}

/// The cap is the kernel's, not the supervisor's: with the launcher stopped, so that
/// nothing counts and nothing kills, a bomb still stays within its bound for as long as it
/// runs, and another process of the same user forks as if nothing happened.
#[test]
fn a_fork_bomb_stays_capped_while_the_supervisor_is_stopped() {
    if !sandbox_available("a_fork_bomb_stays_capped_while_the_supervisor_is_stopped") {
        return;
    }
    for mode in Mode::all(FROZEN_BOMB_USER) {
        let cap = mode.cap();
        // Where the cap is the user's total, give the user a small `ulimit -u`: the job
        // must then stay out of its last eighth, which also keeps this test light. That
        // needs a user who runs nothing else, so only root can test it.
        let quota = (cap.enforcement == "kernel-approximate").then_some(320);
        if quota.is_some() && mode.uid.is_none() {
            eprintln!(
                "SKIPPED {}: testing a cap on the user's total needs root, to run the job \
                 as a user of its own",
                mode.name()
            );
            continue;
        }
        let limit = match quota {
            Some(quota) => quota - quota / 8,
            None => BOMB_LIMIT,
        };
        let bomb = Bomb::start(mode, quota, None);
        let group = bomb.group;
        let peak = Peak::watch(move || group_tasks(group));
        bomb.signal(libc::SIGSTOP);
        bomb.release();
        std::thread::sleep(Duration::from_millis(700));
        let frozen_peak = group_tasks(group);
        bomb.signal(libc::SIGCONT);
        let resumed = Instant::now();
        let (code, stderr, killed, gone, forks) = bomb.end(resumed);
        let peak = peak.finish();
        eprintln!(
            "MEASURED fork bomb, limit {BOMB_LIMIT}, supervisor stopped for 700 ms{} | {} | {} \
             ({}) | peak {peak} live tasks | killed {killed:.0?} after the supervisor resumed, \
             gone after {gone:.0?} | sibling forks meanwhile: {} worked, {} refused",
            quota.map_or(String::new(), |q| format!(", ulimit -u {q}")),
            mode.name(),
            cap.mechanism,
            cap.enforcement,
            forks.worked_in_window,
            forks.refused
        );
        assert!(
            peak <= limit,
            "{}: {peak} live tasks, the kernel should stop at {limit} ({})",
            mode.name(),
            cap.mechanism
        );
        // The bomb did run into its cap: it holds everything but the user's few tasks
        // outside the job (the launcher, its watchdog, the sibling and its child).
        assert!(
            frozen_peak + 8 >= limit,
            "{}: only {frozen_peak} tasks after 700 ms, expected about {limit}",
            mode.name()
        );
        assert_eq!(forks.refused, 0, "{}: {forks:?}", mode.name());
        assert!(forks.worked_in_window >= 10, "{}: {forks:?}", mode.name());
        assert_eq!(code, Some(EXIT_LIMIT), "{}: {stderr}", mode.name());
        assert!(
            stderr.contains("process limit of 64 reached"),
            "{}: {stderr}",
            mode.name()
        );
        assert!(gone < GONE_WITHIN, "{}: gone after {gone:?}", mode.name());
    }
}

/// The cgroup that caps a job is out of the job's reach: it can neither raise its own
/// cap nor move itself out, although the files belong to the user it runs as.
#[test]
fn a_job_cannot_change_or_leave_its_cgroup() {
    if !sandbox_available("a_job_cannot_change_or_leave_its_cgroup") {
        return;
    }
    let process_cap = &report().process_cap;
    let Some(parent) = &process_cap.cgroup_parent else {
        eprintln!("SKIPPED a_job_cannot_change_or_leave_its_cgroup: no job runs in a cgroup here");
        return;
    };
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let policy = policy(Profile::Engine, job.path(), namespaces);
        let run = run_python(
            &policy,
            &format!(
                r#"
parent = {parent:?}
own = [line.split(":", 2)[2].strip() for line in open("/proc/self/cgroup")]
own = [path for path in own if "hotend-sandbox-" in path]
print("cgroups", own, flush=True)
name = own[0].rsplit("/", 1)[1] if own else "missing"
attempt("list", lambda: os.listdir(parent))
attempt("raise_cap", lambda: open(f"{{parent}}/{{name}}/pids.max", "w").write("max"))
attempt("leave", lambda: open(f"{{parent}}/cgroup.procs", "w").write(str(os.getpid())))
"#
            ),
        );
        let mode = if namespaces {
            &process_cap.default
        } else {
            &process_cap.without_namespaces
        };
        if mode.mechanism == "cgroup-pids" {
            assert!(run.stdout.contains("hotend-sandbox-"), "{run}");
        }
        run.assert_denied(&["list", "raise_cap", "leave"]);
    }
}

/// An ordinary user who has been given a pids cgroup gets the exact cap from it, also
/// without namespaces, instead of the cap on the user's total.
#[test]
fn a_user_with_a_cgroup_of_its_own_gets_the_exact_cap_without_namespaces() {
    const TEST: &str = "a_user_with_a_cgroup_of_its_own_gets_the_exact_cap_without_namespaces";
    if !sandbox_available(TEST) {
        return;
    }
    let (true, Some(parent)) = (is_root(), &report().process_cap.cgroup_parent) else {
        eprintln!("SKIPPED {TEST}: needs root and a cgroup hierarchy to hand a cgroup to a user");
        return;
    };
    if report().process_cap.cgroup.as_deref() != Some("cgroup v1 pids hierarchy") {
        // Handing a cgroup v2 cgroup to a user takes more than the two changes of owner
        // below (its cgroup.subtree_control, and the controller passed on from above).
        eprintln!("SKIPPED {TEST}: written for a cgroup v1 pids hierarchy");
        return;
    }
    let delegated = std::path::Path::new(parent).join(format!("delegated-{}", std::process::id()));
    std::fs::create_dir(&delegated).unwrap();
    give_to(&delegated, DELEGATED_USER);
    give_to(&delegated.join("cgroup.procs"), DELEGATED_USER);

    let mode = Mode {
        namespaces: false,
        uid: Some(DELEGATED_USER),
    };
    let bomb = Bomb::start(mode, None, Some(&delegated));
    let group = bomb.group;
    let peak = Peak::watch(move || group_tasks(group));
    bomb.signal(libc::SIGSTOP);
    bomb.release();
    std::thread::sleep(Duration::from_millis(700));
    let frozen_peak = group_tasks(group);
    bomb.signal(libc::SIGCONT);
    let (code, stderr, killed, gone, forks) = bomb.end(Instant::now());
    let peak = peak.finish();
    eprintln!(
        "MEASURED fork bomb, limit {BOMB_LIMIT}, supervisor stopped for 700 ms | uid \
         {DELEGATED_USER} in a cgroup of its own, no namespaces | cgroup-pids | peak {peak} live \
         tasks | killed {killed:.0?} after the supervisor resumed, gone after {gone:.0?} | \
         sibling forks meanwhile: {} worked, {} refused",
        forks.worked_in_window, forks.refused
    );
    // The launcher made the job's cgroup inside the one it was given, and removed it.
    let left: Vec<_> = std::fs::read_dir(&delegated)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().starts_with("hotend-sandbox-"))
        .collect();
    std::fs::remove_dir(&delegated).unwrap();
    assert!(left.is_empty(), "{left:?}");
    assert_eq!((peak, frozen_peak), (BOMB_LIMIT, BOMB_LIMIT), "{stderr}");
    assert_eq!(code, Some(EXIT_LIMIT), "{stderr}");
    assert!(stderr.contains("process limit of 64 reached"), "{stderr}");
    assert!(gone < GONE_WITHIN, "gone after {gone:?}");
    assert_eq!(forks.refused, 0, "{forks:?}");
}

/// Without namespaces and without a cgroup the cap is `RLIMIT_NPROC`, set to what the
/// user runs when the job starts plus the job's limit plus the allowance. The program
/// cannot raise it.
#[test]
fn the_cap_on_the_users_total_is_the_count_at_the_start_plus_the_limit() {
    if !sandbox_available("the_cap_on_the_users_total_is_the_count_at_the_start_plus_the_limit") {
        return;
    }
    let mode = Mode {
        namespaces: false,
        uid: Some(COUNTED_USER),
    };
    if !is_root() || mode.cap().mechanism != "rlimit-on-user-total" {
        eprintln!(
            "SKIPPED the_cap_on_the_users_total_is_the_count_at_the_start_plus_the_limit: \
             needs root, to run the job as a user whose other processes the test knows"
        );
        return;
    }
    wait_until_idle(COUNTED_USER);
    // Five other processes of the user, each with two threads: threads count.
    let others: Vec<_> = (0..5)
        .map(|_| {
            let mut command = Command::new(python());
            command
                .args([
                    "-I",
                    "-c",
                    "import threading, time\n\
                     threading.Thread(target=time.sleep, args=(60,), daemon=True).start()\n\
                     print('up', flush=True)\ntime.sleep(60)",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped());
            run_as(&mut command, COUNTED_USER);
            let mut child = command.spawn().unwrap();
            let mut up = [0u8; 2];
            std::io::Read::read_exact(child.stdout.as_mut().unwrap(), &mut up).unwrap();
            child
        })
        .collect();
    assert_eq!(user_tasks(COUNTED_USER), 10);

    let job = tempfile::tempdir().unwrap();
    give_to(job.path(), COUNTED_USER);
    let mut policy = policy(Profile::Engine, job.path(), false);
    policy.limits.max_processes = BOMB_LIMIT;
    let mut command = python_command(
        &policy,
        r#"
import resource
soft, hard = resource.getrlimit(resource.RLIMIT_NPROC)
print(f"limit={soft},{hard}", flush=True)
x86 = os.uname().machine == "x86_64"
limit = (ctypes.c_ulong * 2)(hard, hard)
attempt("raise_hard_limit", lambda: resource.setrlimit(resource.RLIMIT_NPROC, (hard + 1, hard + 1)))
# The soft limit is the cap. It is the supervisor's to move, up to the hard limit.
attempt("raise_soft_limit", lambda: resource.setrlimit(resource.RLIMIT_NPROC, (hard, hard)))
attempt("prlimit64", lambda: syscall(302 if x86 else 261, 0, resource.RLIMIT_NPROC, limit, 0))
attempt("setrlimit", lambda: syscall(160 if x86 else 164, resource.RLIMIT_NPROC, limit))
print("after=%d,%d" % resource.getrlimit(resource.RLIMIT_NPROC), flush=True)
"#,
    );
    run_as(&mut command, COUNTED_USER);
    let run = run(command);
    for mut other in others {
        other.kill().unwrap();
        other.wait().unwrap();
    }
    assert_eq!(run.code, Some(0), "{run}");
    // The ten tasks above, the launcher and its watchdog. The program itself is the
    // first of the job's 64, so a job can have its limit plus the allowance, not one more.
    let expected = 10 + 2 + BOMB_LIMIT + ALLOWANCE;
    // The furthest the supervisor may move the cap: seven eighths of the user's limit.
    let own = own_process_limit();
    let ceiling = own - own / 8;
    assert!(
        run.stdout
            .contains(&format!("limit={expected},{ceiling}\n")),
        "expected a limit of {expected} below a ceiling of {ceiling}: {run}"
    );
    // Python reports a refused setrlimit as a ValueError.
    for refused in ["raise_hard_limit", "raise_soft_limit"] {
        assert!(
            run.stdout.contains(&format!("{refused}=ERROR:ValueError")),
            "{refused}: {run}"
        );
    }
    run.assert_denied(&["prlimit64", "setrlimit"]);
    assert!(
        run.stdout
            .contains(&format!("after={expected},{ceiling}\n")),
        "{run}"
    );
}

/// A long job keeps its allowance when the user's other programs grow: the supervisor
/// moves the cap on the user's total along with them, up and down again. Without that
/// a job would lose `fork` and thread creation once the user ran 1024 tasks more than
/// at its start.
#[test]
fn the_cap_on_the_users_total_follows_the_users_other_programs() {
    const TEST: &str = "the_cap_on_the_users_total_follows_the_users_other_programs";
    if !sandbox_available(TEST) {
        return;
    }
    let mode = Mode {
        namespaces: false,
        uid: Some(GROWING_USER),
    };
    if !is_root() || mode.cap().mechanism != "rlimit-on-user-total" {
        eprintln!(
            "SKIPPED {TEST}: needs root, to run the job as a user whose other processes the test knows"
        );
        return;
    }
    wait_until_idle(GROWING_USER);
    let job = tempfile::tempdir().unwrap();
    give_to(job.path(), GROWING_USER);
    let mut policy = policy(Profile::Agent, job.path(), false);
    policy.limits.max_processes = BOMB_LIMIT;
    policy.limits.wall_seconds = 120;
    let mut command = script_file_command(
        &policy,
        r#"
import resource, subprocess, threading
def step(name):
    open(name + ".ready", "w").close()
    while not os.path.exists(name + ".go"):
        time.sleep(0.01)
    print(f"{name}_limit={resource.getrlimit(resource.RLIMIT_NPROC)[0]}", flush=True)
    def thread():
        t = threading.Thread(target=lambda: None); t.start(); t.join()
    attempt(f"{name}_thread", thread)
    attempt(f"{name}_process", lambda: subprocess.run(["/bin/true"], check=True))
step("start")
step("grown")
step("shrunk")
"#,
    );
    run_as(&mut command, GROWING_USER);
    let launcher = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let go = |name: &str| {
        wait_for(&job.path().join(format!("{name}.ready")));
        std::fs::write(job.path().join(format!("{name}.go")), "").unwrap();
    };
    go("start");
    wait_for(&job.path().join("grown.ready"));

    // Another program of the user starts 1200 threads: more than the allowance.
    let mut command = Command::new(python());
    command
        .args([
            "-I",
            "-c",
            "import threading, time\nthreading.stack_size(128 * 1024)\n\
             for _ in range(1200):\n    \
             threading.Thread(target=time.sleep, args=(120,), daemon=True).start()\n\
             print('up', flush=True)\ntime.sleep(120)",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    run_as(&mut command, GROWING_USER);
    let mut other = command.spawn().unwrap();
    let mut up = [0u8; 2];
    std::io::Read::read_exact(other.stdout.as_mut().unwrap(), &mut up).unwrap();
    assert!(user_tasks(GROWING_USER) > ALLOWANCE);
    // Longer than the supervisor takes to notice (once a second).
    std::thread::sleep(Duration::from_millis(2500));
    go("grown");
    wait_for(&job.path().join("shrunk.ready"));
    other.kill().unwrap();
    other.wait().unwrap();
    std::thread::sleep(Duration::from_millis(2500));
    go("shrunk");

    let output = launcher.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stdout}\n{stderr}");
    let limit = |name: &str| -> u64 {
        stdout
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}_limit=")))
            .and_then(|limit| limit.parse().ok())
            .unwrap_or_else(|| panic!("no {name}_limit: {stdout}\n{stderr}"))
    };
    // The launcher and its watchdog are the user's other tasks at the start and the end.
    let quiet = 2 + BOMB_LIMIT + ALLOWANCE;
    assert_eq!(limit("start"), quiet, "{stdout}");
    assert_eq!(limit("grown"), quiet + 1201, "{stdout}");
    assert_eq!(limit("shrunk"), quiet, "{stdout}");
    for attempt in ["start", "grown", "shrunk"] {
        for kind in ["thread", "process"] {
            assert!(
                stdout.contains(&format!("{attempt}_{kind}=ALLOWED")),
                "{attempt}_{kind}: {stdout}\n{stderr}"
            );
        }
    }
    assert!(stderr.is_empty(), "{stderr}");
}

/// Where the launcher cannot make room (the user is at the ceiling of its own limit),
/// the job's forks fail, and the launcher says why on stderr instead of leaving an
/// unexplained EAGAIN.
#[test]
fn a_job_that_cannot_fork_for_lack_of_room_is_reported() {
    use std::os::unix::process::CommandExt;
    const TEST: &str = "a_job_that_cannot_fork_for_lack_of_room_is_reported";
    if !sandbox_available(TEST) {
        return;
    }
    let mode = Mode {
        namespaces: false,
        uid: Some(CROWDED_USER),
    };
    if !is_root() || mode.cap().mechanism != "rlimit-on-user-total" {
        eprintln!(
            "SKIPPED {TEST}: needs root, to run the job as a user whose other processes the test knows"
        );
        return;
    }
    wait_until_idle(CROWDED_USER);
    let job = tempfile::tempdir().unwrap();
    give_to(job.path(), CROWDED_USER);
    let mut policy = policy(Profile::Agent, job.path(), false);
    policy.limits.max_processes = BOMB_LIMIT;
    policy.limits.wall_seconds = 120;
    let mut command = script_file_command(
        &policy,
        r#"
open("ready", "w").close()
while not os.path.exists("go"):
    time.sleep(0.01)
def fork():
    pid = os.fork()
    if pid == 0:
        os._exit(0)
    os.waitpid(pid, 0)
attempt("fork", fork)
"#,
    );
    run_as(&mut command, CROWDED_USER);
    // The launcher's user may have 400 tasks; the job's ceiling is seven eighths, 350.
    // SAFETY: the closure only calls setrlimit, which is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: 400,
                rlim_max: 400,
            };
            match libc::setrlimit(libc::RLIMIT_NPROC, &limit) {
                0 => Ok(()),
                _ => Err(std::io::Error::last_os_error()),
            }
        });
    }
    let launcher = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(&job.path().join("ready"));
    // Another program of the user, under the tests' own limit, takes the user to 365.
    let mut command = Command::new(python());
    command
        .args([
            "-I",
            "-c",
            "import threading, time\nthreading.stack_size(128 * 1024)\n\
             for _ in range(360):\n    \
             threading.Thread(target=time.sleep, args=(120,), daemon=True).start()\n\
             print('up', flush=True)\ntime.sleep(120)",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    run_as(&mut command, CROWDED_USER);
    let mut other = command.spawn().unwrap();
    let mut up = [0u8; 2];
    std::io::Read::read_exact(other.stdout.as_mut().unwrap(), &mut up).unwrap();
    std::thread::sleep(Duration::from_millis(2500));
    std::fs::write(job.path().join("go"), "").unwrap();
    let output = launcher.wait_with_output().unwrap();
    other.kill().unwrap();
    other.wait().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("fork=DENIED:BlockingIOError:11"),
        "{stdout}"
    );
    assert!(
        stderr.contains("hotend-sandbox: the job cannot start another process or thread"),
        "{stderr}"
    );
    assert_eq!(
        stderr.matches("cannot start another process").count(),
        1,
        "{stderr}"
    );
}

/// Forks until the kernel refuses, then reports how many children it got.
const CHILDREN: &str = r#"
children = []
try:
    while len(children) < 200:
        pid = os.fork()
        if pid == 0:
            time.sleep(30)
            os._exit(0)
        children.append(pid)
except OSError:
    pass
print(f"children={len(children)}", flush=True)
for pid in children:
    os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
"#;

/// A user whose own `ulimit -u` is below the job's limit still gets a job, capped lower;
/// a user with nothing left gets none; a user who is close to the limit shares what is
/// free with the job.
#[test]
fn a_small_process_quota_of_the_user_is_respected() {
    use std::os::unix::process::CommandExt;
    const TEST: &str = "a_small_process_quota_of_the_user_is_respected";
    if !sandbox_available(TEST) {
        return;
    }
    if !is_root() {
        eprintln!("SKIPPED {TEST}: needs root, to run the job as a user who runs nothing else");
        return;
    }
    let run_with = |namespaces: bool, quota: u64| {
        wait_until_idle(SMALL_QUOTA_USER);
        let job = tempfile::tempdir().unwrap();
        give_to(job.path(), SMALL_QUOTA_USER);
        let mut policy = policy(Profile::Engine, job.path(), namespaces);
        policy.limits.max_processes = BOMB_LIMIT;
        policy.limits.wall_seconds = 30;
        let mut command = python_command(&policy, CHILDREN);
        run_as(&mut command, SMALL_QUOTA_USER);
        // SAFETY: the closure only calls setrlimit, which is async-signal-safe.
        unsafe {
            command.pre_exec(move || {
                let limit = libc::rlimit {
                    rlim_cur: quota,
                    rlim_max: quota,
                };
                match libc::setrlimit(libc::RLIMIT_NPROC, &limit) {
                    0 => Ok(()),
                    _ => Err(std::io::Error::last_os_error()),
                }
            });
        }
        run(command)
    };
    for namespaces in namespace_modes() {
        let cap = Mode {
            namespaces,
            uid: Some(SMALL_QUOTA_USER),
        }
        .cap();
        // 40 allowed: fewer than the job's 64.
        let run = run_with(namespaces, 40);
        assert_eq!(run.code, Some(0), "{run}");
        let children: u64 = run
            .stdout
            .trim()
            .strip_prefix("children=")
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("{run}"));
        match cap.mechanism.as_str() {
            // The user's own limit stays in force above the namespace: 40 less the
            // launcher, its watchdog, the namespace's init and the program.
            "rlimit-in-user-namespace" => assert_eq!(children, 36, "{run}"),
            // Seven eighths of 40 are 35, less the launcher, its watchdog and the program.
            "rlimit-on-user-total" => {
                assert_eq!(children, 32, "{run}");
                assert!(
                    run.stderr.contains("the job can start only 32 more"),
                    "{run}"
                );
            }
            other => panic!("unexpected mechanism {other} for an unprivileged user"),
        }
        // Nothing left for a job: the launcher, its watchdog and the program fill a quota
        // of 3 (and, without namespaces, reach into the last eighth of a quota of 4).
        let run = run_with(namespaces, if namespaces { 2 } else { 4 });
        assert_eq!(run.code, Some(125), "{run}");
        assert!(!run.stdout.contains("children="), "{run}");

        if cap.mechanism != "rlimit-on-user-total" {
            continue;
        }
        // A user who already runs inside the last eighth of the limit is not turned
        // away: the job gets half of what is free. 290 tasks of another program, the
        // launcher and its watchdog under a limit of 320 leave 28, so 14 for the job:
        // the program and 13 children.
        wait_until_idle(SMALL_QUOTA_USER);
        let mut command = Command::new(python());
        command
            .args([
                "-I",
                "-c",
                "import threading, time\nthreading.stack_size(128 * 1024)\n\
                 for _ in range(289):\n    \
                 threading.Thread(target=time.sleep, args=(60,), daemon=True).start()\n\
                 print('up', flush=True)\ntime.sleep(60)",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        run_as(&mut command, SMALL_QUOTA_USER);
        let mut other = command.spawn().unwrap();
        let mut up = [0u8; 2];
        std::io::Read::read_exact(other.stdout.as_mut().unwrap(), &mut up).unwrap();
        let job = tempfile::tempdir().unwrap();
        give_to(job.path(), SMALL_QUOTA_USER);
        let mut policy = policy(Profile::Engine, job.path(), namespaces);
        policy.limits.max_processes = BOMB_LIMIT;
        let mut command = python_command(&policy, CHILDREN);
        run_as(&mut command, SMALL_QUOTA_USER);
        // SAFETY: the closure only calls setrlimit, which is async-signal-safe.
        unsafe {
            command.pre_exec(|| {
                let limit = libc::rlimit {
                    rlim_cur: 320,
                    rlim_max: 320,
                };
                match libc::setrlimit(libc::RLIMIT_NPROC, &limit) {
                    0 => Ok(()),
                    _ => Err(std::io::Error::last_os_error()),
                }
            });
        }
        let run = common::run(command);
        other.kill().unwrap();
        other.wait().unwrap();
        assert_eq!(run.code, Some(0), "{run}");
        assert_eq!(run.stdout.trim(), "children=13", "{run}");
        assert!(
            run.stderr.contains("the job can start only 13 more"),
            "{run}"
        );
    }
}

/// What the kernel counts for a whole user account, and not for a process, is out of a
/// job's reach or capped for it: inotify instances (128 per user by default) and
/// queued signals (`RLIMIT_SIGPENDING`). A job that takes all it can get leaves every
/// other program of its user able to watch files and to queue signals.
#[test]
fn a_job_cannot_use_up_the_allowances_of_its_users_account() {
    const TEST: &str = "a_job_cannot_use_up_the_allowances_of_its_users_account";
    /// `child::PENDING_SIGNALS`.
    const PENDING_SIGNALS: u64 = 1024;
    if !sandbox_available(TEST) {
        return;
    }
    // What another program of the user can still do: `inotify=.. sigqueue=..`.
    let outside = |uid: Option<u32>| {
        let mut command = Command::new(python());
        command.args([
            "-I",
            "-c",
            r#"
import ctypes, os, signal
libc = ctypes.CDLL(None, use_errno=True)
libc.sigqueue.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGRTMIN})
print("inotify=%s sigqueue=%s" % (
    "OK" if libc.inotify_init1(0) >= 0 else ctypes.get_errno(),
    "OK" if libc.sigqueue(os.getpid(), signal.SIGRTMIN, None) == 0 else ctypes.get_errno()))
"#,
        ]);
        if let Some(uid) = uid {
            run_as(&mut command, uid);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    for mode in Mode::all(ACCOUNT_USER) {
        let before = outside(mode.uid);
        if mode.uid.is_some() {
            assert_eq!(before, "inotify=OK sigqueue=OK", "{}", mode.name());
        }
        let job = tempfile::tempdir().unwrap();
        let mut policy = policy(Profile::Engine, job.path(), mode.namespaces);
        policy.limits.wall_seconds = 60;
        let mut command = script_file_command(
            &policy,
            r#"
import resource
instances = 0
while instances < 4096 and libc.inotify_init1(0) >= 0:
    instances += 1
libc.sigqueue.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGRTMIN})
queued = 0
while queued < 1000000 and libc.sigqueue(os.getpid(), signal.SIGRTMIN, None) == 0:
    queued += 1
print(f"instances={instances} queued={queued}", flush=True)
print("pending=%d,%d" % resource.getrlimit(resource.RLIMIT_SIGPENDING), flush=True)
print("msgqueue=%d,%d" % resource.getrlimit(resource.RLIMIT_MSGQUEUE), flush=True)
attempt("raise_pending", lambda: resource.setrlimit(resource.RLIMIT_SIGPENDING, (2048, 2048)))
open("held", "w").close()
while not os.path.exists("go"):
    time.sleep(0.01)
"#,
        );
        if let Some(uid) = mode.uid {
            give_to(job.path(), uid);
            run_as(&mut command, uid);
        }
        let launcher = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_for(&job.path().join("held"));
        // The job holds everything it could get. Another program of the same user:
        let during = outside(mode.uid);
        std::fs::write(job.path().join("go"), "").unwrap();
        let output = launcher.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}: {stdout}\n{stderr}",
            mode.name()
        );
        assert_eq!(during, before, "{}: {stdout}", mode.name());
        let number = |name: &str| -> u64 {
            stdout
                .split_ascii_whitespace()
                .find_map(|field| field.strip_prefix(&format!("{name}=")))
                .and_then(|number| number.parse().ok())
                .unwrap_or_else(|| panic!("{}: no {name}: {stdout}\n{stderr}", mode.name()))
        };
        assert_eq!(number("instances"), 0, "{}: {stdout}", mode.name());
        // In the job's own user namespace, and for a user who runs nothing else, the
        // count is the job's alone. Otherwise the user's other programs may hold some.
        if mode.namespaces || mode.uid.is_some() {
            assert_eq!(
                number("queued"),
                PENDING_SIGNALS,
                "{}: {stdout}",
                mode.name()
            );
        } else {
            assert!(
                number("queued") <= PENDING_SIGNALS,
                "{}: {stdout}",
                mode.name()
            );
        }
        assert!(
            stdout.contains(&format!("pending={PENDING_SIGNALS},{PENDING_SIGNALS}\n")),
            "{}: {stdout}",
            mode.name()
        );
        assert!(
            stdout.contains("msgqueue=0,0\n"),
            "{}: {stdout}",
            mode.name()
        );
        assert!(
            stdout.contains("raise_pending=ERROR:ValueError"),
            "{}: {stdout}",
            mode.name()
        );
    }
}

/// `fork` fails with EAGAIN for more reasons than `RLIMIT_NPROC`. A launcher that runs
/// as root inside a pids cgroup with one place left must not conclude from a refused
/// fork that the kernel applies the limit to root: it does not, and a job that relied
/// on it would have no cap.
#[test]
fn a_full_cgroup_around_the_launcher_is_not_taken_for_the_process_limit() {
    use std::os::unix::process::CommandExt;
    const TEST: &str = "a_full_cgroup_around_the_launcher_is_not_taken_for_the_process_limit";
    if !sandbox_available(TEST) {
        return;
    }
    let process_cap = &report().process_cap;
    let (true, false, Some(parent), Some("cgroup v1 pids hierarchy")) = (
        is_root(),
        process_cap.rlimit_enforced,
        &process_cap.cgroup_parent,
        process_cap.cgroup.as_deref(),
    ) else {
        eprintln!(
            "SKIPPED {TEST}: needs root of the initial user namespace and a cgroup v1 pids \
             hierarchy to put the launcher into a cgroup with one place left"
        );
        return;
    };
    let full = std::path::Path::new(parent).join(format!("full-{}", std::process::id()));
    std::fs::create_dir(&full).unwrap();
    // The launcher itself and one more task.
    std::fs::write(full.join("pids.max"), "2").unwrap();
    let members = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(
        full.join("cgroup.procs").as_os_str(),
    ))
    .unwrap();
    let mut probe = Command::new(LAUNCHER);
    probe.arg("probe").stdin(Stdio::null());
    // SAFETY: the closure only makes system calls, on a value prepared before the fork.
    unsafe {
        probe.pre_exec(move || {
            let fd = libc::open(members.as_ptr(), libc::O_WRONLY);
            if fd < 0 || libc::write(fd, c"0".as_ptr().cast(), 1) != 1 {
                return Err(std::io::Error::last_os_error());
            }
            libc::close(fd);
            Ok(())
        });
    }
    let output = probe.output();
    let removed = std::fs::remove_dir(&full);
    let output = output.unwrap();
    removed.unwrap();
    assert!(output.status.success(), "{output:?}");
    let probed: hotend_sandbox::ProbeReport = serde_json::from_slice(&output.stdout).unwrap();
    assert!(probed.root, "{probed:?}");
    assert!(!probed.process_cap.rlimit_enforced, "{probed:?}");
    assert!(
        probed
            .process_cap
            .rlimit_detail
            .as_deref()
            .is_some_and(|why| why.contains("a fork is refused whatever")),
        "{probed:?}"
    );
    for mode in [
        &probed.process_cap.default,
        &probed.process_cap.without_namespaces,
    ] {
        assert_eq!(mode.mechanism, "cgroup-pids", "{probed:?}");
    }
    assert!(probed.usable, "{probed:?}");
}

/// Root has no per-user process limit, so without a cgroup nothing in the kernel could
/// cap a job. The launcher then starts nothing, and the probe says so.
#[test]
fn root_is_refused_where_no_cgroup_can_cap_the_job() {
    use std::os::unix::process::CommandExt;
    if !sandbox_available("root_is_refused_where_no_cgroup_can_cap_the_job") {
        return;
    }
    if report().process_cap.rlimit_enforced {
        eprintln!(
            "SKIPPED root_is_refused_where_no_cgroup_can_cap_the_job: the tests do not run \
             as root, so the kernel's per-user limit applies"
        );
        return;
    }
    // A machine without cgroups: the launcher gets a mount namespace of its own in which
    // the cgroup file systems are not mounted.
    let without_cgroups = |command: &mut Command| {
        // SAFETY: the closure only makes system calls with constant arguments.
        unsafe {
            command.pre_exec(|| {
                let check = |result: libc::c_int| match result {
                    0 => Ok(()),
                    _ => Err(std::io::Error::last_os_error()),
                };
                check(libc::unshare(libc::CLONE_NEWNS))?;
                check(libc::mount(
                    std::ptr::null(),
                    c"/".as_ptr(),
                    std::ptr::null(),
                    libc::MS_REC | libc::MS_PRIVATE,
                    std::ptr::null(),
                ))?;
                check(libc::umount2(c"/sys/fs/cgroup".as_ptr(), libc::MNT_DETACH))
            });
        }
    };
    let mut probe = Command::new(LAUNCHER);
    probe.arg("probe");
    without_cgroups(&mut probe);
    let output = probe.output().unwrap();
    let probed: hotend_sandbox::ProbeReport = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!probed.usable, "{probed:?}");
    assert!(
        probed
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("cap the number of processes")),
        "{probed:?}"
    );
    assert_eq!(probed.process_cap.default.mechanism, "none");
    assert_eq!(probed.process_cap.default.enforcement, "refused");
    assert_eq!(probed.process_cap.without_namespaces.mechanism, "none");

    for profile in [Profile::Engine, Profile::Agent, Profile::Slicer] {
        for namespaces in namespace_modes() {
            let job = tempfile::tempdir().unwrap();
            let marker = job.path().join("program.ran");
            let policy = policy(profile, job.path(), namespaces);
            let touch = format!("touch {}", marker.display());
            let mut command =
                hotend_sandbox::command(&policy, "/bin/sh", ["-c", touch.as_str()]).unwrap();
            without_cgroups(&mut command);
            let output = command.output().unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(125), "{profile}: {stderr}");
            assert!(
                stderr.contains("cap the number of processes"),
                "{profile}: {stderr}"
            );
            assert!(!marker.exists(), "{profile}: the program was started");
        }
    }
}

#[test]
fn memory_hog_is_stopped() {
    if !sandbox_available("memory_hog_is_stopped") {
        return;
    }
    for namespaces in namespace_modes() {
        // One process asking for more than the cap is refused by the kernel.
        let job = tempfile::tempdir().unwrap();
        let mut policy = policy(Profile::Engine, job.path(), namespaces);
        policy.limits.memory_mb = 256;
        let run = run_python(
            &policy,
            r#"
try:
    block = bytearray(1 << 30)
    block[::4096] = b"x" * len(block[::4096])
    print("ALLOCATED")
except MemoryError:
    print("REFUSED")
    sys.exit(3)
"#,
        );
        assert_eq!(run.code, Some(3), "{run}");
        assert_eq!(run.stdout.trim(), "REFUSED", "{run}");

        // Many processes that each stay under the cap are stopped by the supervisor.
        let job = tempfile::tempdir().unwrap();
        let mut policy = common::policy(Profile::Engine, job.path(), namespaces);
        policy.limits.memory_mb = 300;
        policy.limits.wall_seconds = 60;
        let run = run_script_file(
            &policy,
            r#"
for _ in range(8):
    if os.fork() == 0:
        block = bytearray(80 << 20)
        for offset in range(0, len(block), 4096):
            block[offset] = 1
        time.sleep(120)
        os._exit(0)
time.sleep(120)
"#,
        );
        assert_eq!(run.code, Some(EXIT_LIMIT), "{run}");
        assert!(
            run.stderr.contains("memory limit of 300 MB reached"),
            "{run}"
        );
        assert!(run.elapsed < Duration::from_secs(30), "{run}");
        assert_no_processes_left(&job_marker(&policy));
    }
}

#[test]
fn busy_loop_is_stopped() {
    if !sandbox_available("busy_loop_is_stopped") {
        return;
    }
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let mut policy = policy(Profile::Engine, job.path(), namespaces);
        policy.limits.cpu_seconds = 1;
        policy.limits.wall_seconds = 60;
        let run = run_python(&policy, "while True:\n    pass\n");
        // The kernel kills the process when its CPU time is used up.
        assert_eq!(run.code, Some(128 + 9), "{run}");
        assert!(run.elapsed < Duration::from_secs(20), "{run}");
    }
}

#[test]
fn wall_clock_kills_the_whole_tree() {
    if !sandbox_available("wall_clock_kills_the_whole_tree") {
        return;
    }
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let mut policy = policy(Profile::Engine, job.path(), namespaces);
        policy.limits.wall_seconds = 2;
        let run = run_script_file(
            &policy,
            r#"
child = os.fork()
if child == 0:
    # A daemon-style double fork: the grandchild is orphaned and tries to leave the session.
    if os.fork() == 0:
        try:
            os.setsid()
            open("grandchild.escaped", "w").close()
        except OSError:
            pass
        open("grandchild.started", "w").close()
        time.sleep(300)
    os._exit(0)
os.waitpid(child, 0)
while not os.path.exists("grandchild.started"):
    time.sleep(0.01)
print("tree running", flush=True)
time.sleep(300)
"#,
        );
        assert_eq!(run.code, Some(124), "{run}");
        assert!(
            run.stderr.contains("wall-clock limit of 2 s reached"),
            "{run}"
        );
        assert!(run.stdout.contains("tree running"), "{run}");
        assert!(run.elapsed >= Duration::from_secs(2), "{run}");
        assert!(run.elapsed < Duration::from_secs(15), "{run}");
        assert!(job.path().join("grandchild.started").exists());
        assert!(!job.path().join("grandchild.escaped").exists());
        assert_no_processes_left(&job_marker(&policy));
    }
}

#[test]
fn processes_left_behind_are_killed_when_the_program_exits() {
    if !sandbox_available("processes_left_behind_are_killed_when_the_program_exits") {
        return;
    }
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let policy = policy(Profile::Engine, job.path(), namespaces);
        let run = run_script_file(
            &policy,
            r#"
if os.fork() == 0:
    if os.fork() == 0:
        open("background.started", "w").close()
        time.sleep(300)
    os._exit(0)
while not os.path.exists("background.started"):
    time.sleep(0.01)
sys.exit(5)
"#,
        );
        assert_eq!(run.code, Some(5), "{run}");
        assert_no_processes_left(&job_marker(&policy));
    }
}

#[test]
fn terminating_or_killing_the_launcher_takes_the_tree_down() {
    if !sandbox_available("terminating_or_killing_the_launcher_takes_the_tree_down") {
        return;
    }
    for namespaces in namespace_modes() {
        for (signal, expected) in [("-TERM", Some(128 + 15)), ("-KILL", None)] {
            let job = tempfile::tempdir().unwrap();
            let policy = policy(Profile::Engine, job.path(), namespaces);
            // Children and grandchildren of the program: the parent-death signal reaches
            // only the program itself, so these are the ones that could be left behind.
            let mut command = script_file_command(
                &policy,
                r#"
for _ in range(3):
    if os.fork() == 0:
        if os.fork() == 0:
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
        while True:
            with open(f"alive-{os.getpid()}", "a") as f:
                f.write("x")
            time.sleep(0.05)
open("started", "w").close()
time.sleep(300)
"#,
            );
            let mut launcher = command.stdout(Stdio::null()).spawn().unwrap();
            wait_for(&job.path().join("started"));
            let killed = Command::new("kill")
                .args([signal, &launcher.id().to_string()])
                .status()
                .unwrap();
            assert!(killed.success());
            let status = launcher.wait().unwrap();
            // SIGTERM is handled: the launcher kills the tree and reports 128+15. SIGKILL
            // cannot be handled: the parent-death signal kills the program, and the
            // watchdog (or the PID namespace) kills its descendants.
            assert_eq!(status.code(), expected, "signal {signal}");
            assert_no_processes_left(&job_marker(&policy));
            // The launcher removes the job's cgroup; when it was killed, its watchdog does.
            assert_no_cgroup_left(launcher.id());
            // Nothing writes any more.
            let written = || -> u64 {
                std::fs::read_dir(job.path())
                    .unwrap()
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with("alive-"))
                    .map(|e| e.metadata().unwrap().len())
                    .sum()
            };
            let before = written();
            assert!(before > 0, "the descendants never ran");
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(written(), before, "a descendant outlived the launcher");
        }
    }
}

#[test]
fn the_tree_dies_when_the_launchers_parent_dies() {
    if !sandbox_available("the_tree_dies_when_the_launchers_parent_dies") {
        return;
    }
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let policy = policy(Profile::Engine, job.path(), namespaces);
        let sandboxed = script_file_command(
            &policy,
            r#"
open("started", "w").close()
time.sleep(300)
"#,
        );
        // A shell stands in for the core: it starts the launcher in the background, waits
        // for the program to run and then dies without telling anyone.
        let mut shell = Command::new("/bin/sh");
        shell
            .arg("-c")
            .arg(r#""$@" & while [ ! -e "$STARTED" ]; do sleep 0.05; done; kill -KILL $$"#)
            .arg("sh")
            .arg(sandboxed.get_program())
            .args(sandboxed.get_args())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("STARTED", job.path().join("started"))
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        let status = shell.status().unwrap();
        assert_eq!(
            status.code(),
            None,
            "the stand-in parent should die from its own signal"
        );
        assert!(job.path().join("started").exists());
        assert_no_processes_left(&job_marker(&policy));
    }
}

#[test]
fn file_size_and_open_files_are_capped() {
    if !sandbox_available("file_size_and_open_files_are_capped") {
        return;
    }
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let mut policy = policy(Profile::Engine, job.path(), namespaces);
        policy.limits.max_file_mb = 1;
        policy.limits.max_open_files = 32;
        let run = run_python(
            &policy,
            r#"
def write_big():
    with open("big.bin", "wb", buffering=0) as f:
        for _ in range(8):
            f.write(b"x" * (1 << 20))
attempt("big_file", write_big)
def open_many():
    global handles
    handles = [open("/dev/null") for _ in range(64)]
attempt("many_files", open_many)
"#,
        );
        run.assert_denied(&["big_file", "many_files"]);
        let size = std::fs::metadata(job.path().join("big.bin")).unwrap().len();
        assert!(size <= 1 << 20, "big.bin grew to {size} bytes");
    }
}

#[test]
fn the_job_folder_has_a_total_size_limit() {
    if !sandbox_available("the_job_folder_has_a_total_size_limit") {
        return;
    }
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let mut policy = policy(Profile::Engine, job.path(), namespaces);
        policy.limits.max_file_mb = 1;
        policy.limits.max_disk_mb = 8;
        policy.limits.wall_seconds = 60;
        // Every file stays under the per-file limit; together they would fill the disk.
        let run = run_script_file(
            &policy,
            r#"
block = b"x" * (1 << 20)
for i in range(4096):
    with open(f"fill-{i}", "wb") as f:
        f.write(block)
    if i % 16 == 15:
        time.sleep(0.2)
print("filled=ALLOWED", flush=True)
"#,
        );
        assert_eq!(run.code, Some(EXIT_LIMIT), "{run}");
        assert!(run.stderr.contains("disk limit of 8 MB reached"), "{run}");
        assert!(!run.stdout.contains("filled"), "{run}");
        let files = std::fs::read_dir(job.path()).unwrap().count();
        assert!(
            files < 100,
            "{files} files were written under an 8 MB limit"
        );
        assert_no_processes_left(&job_marker(&policy));

        // A job that stays within the limit is left alone.
        let job = tempfile::tempdir().unwrap();
        let mut policy = policy_for(job.path(), namespaces);
        policy.limits.max_disk_mb = 8;
        let run = run_script_file(
            &policy,
            r#"
with open("small", "wb") as f:
    f.write(b"x" * (2 << 20))
time.sleep(0.5)
print("small=ALLOWED", flush=True)
"#,
        );
        run.assert_allowed(&["small"]);
    }
}

fn policy_for(job: &std::path::Path, namespaces: bool) -> Policy {
    policy(Profile::Engine, job, namespaces)
}

/// The launcher keeps a handle on the job folder and on every read-only path while the
/// program runs. On a filesystem without stable inodes (9p, WSL2's Windows drives) that is
/// what keeps the Landlock rules attached to them.
#[test]
fn the_launcher_holds_the_job_folder_and_the_read_only_paths_open() {
    if !sandbox_available("the_launcher_holds_the_job_folder_and_the_read_only_paths_open") {
        return;
    }
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let mut policy = policy(Profile::Engine, job.path(), namespaces);
        policy.read_only.push(extra.path().to_path_buf());
        let mut command = script_file_command(
            &policy,
            r#"
open("started", "w").close()
time.sleep(300)
"#,
        );
        let mut launcher = command.stdout(Stdio::null()).spawn().unwrap();
        wait_for(&job.path().join("started"));
        let held: Vec<std::path::PathBuf> =
            std::fs::read_dir(format!("/proc/{}/fd", launcher.id()))
                .unwrap()
                .flatten()
                .filter_map(|entry| std::fs::read_link(entry.path()).ok())
                .collect();
        let mut expected = policy.read_only.clone();
        expected.push(policy.job_dir.clone());
        for path in expected {
            let path = path.canonicalize().unwrap();
            assert!(
                held.contains(&path),
                "the launcher holds no handle on {}: {held:?}",
                path.display()
            );
        }
        // SAFETY: signals the launcher this test started and has not yet waited for.
        unsafe { libc::kill(launcher.id() as i32, libc::SIGTERM) };
        assert_eq!(launcher.wait().unwrap().code(), Some(128 + 15));
        assert_no_processes_left(&job_marker(&policy));
    }
}

#[test]
fn exit_codes_are_passed_through() {
    if !sandbox_available("exit_codes_are_passed_through") {
        return;
    }
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let policy = policy(Profile::Engine, job.path(), namespaces);
        for code in [0, 1, 2, 42, 255] {
            let run = run_python(&policy, &format!("sys.exit({code})"));
            assert_eq!(run.code, Some(code), "{run}");
        }
        // Death by signal is reported the way shells do: 128 + signal number.
        let run = run_python(&policy, "os.kill(os.getpid(), signal.SIGSEGV)");
        assert_eq!(run.code, Some(128 + 11), "{run}");
        let run = run_python(&policy, "os.abort()");
        assert_eq!(run.code, Some(128 + 6), "{run}");
        // Output and input pass through the launcher untouched.
        let mut command = python_command(&policy, "print(sys.stdin.read().upper(), end='')");
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = command.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), b"hello").unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(output.stdout, b"HELLO");
    }
}

#[test]
fn exit_125_when_the_sandbox_cannot_be_applied() {
    let job = tempfile::tempdir().unwrap();
    let job_path = job.path().to_str().unwrap();
    let marker = job.path().join("program.ran");
    let touch = format!("touch {}", marker.display());
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("no arguments", vec![]),
        ("unknown command", vec!["exec"]),
        (
            "unknown option",
            vec![
                "run",
                "--profile",
                "engine",
                "--job",
                job_path,
                "--network",
                "--",
                "/bin/sh",
                "-c",
                &touch,
            ],
        ),
        (
            "unknown profile",
            vec![
                "run",
                "--profile",
                "admin",
                "--job",
                job_path,
                "--",
                "/bin/sh",
                "-c",
                &touch,
            ],
        ),
        (
            "missing job folder",
            vec![
                "run",
                "--profile",
                "engine",
                "--job",
                "/no/such/job",
                "--",
                "/bin/sh",
                "-c",
                &touch,
            ],
        ),
        (
            "relative job folder",
            vec![
                "run",
                "--profile",
                "engine",
                "--job",
                ".",
                "--",
                "/bin/sh",
                "-c",
                &touch,
            ],
        ),
        (
            "missing read-only path",
            vec![
                "run",
                "--profile",
                "engine",
                "--job",
                job_path,
                "--ro",
                "/no/such/runtime",
                "--",
                "/bin/sh",
                "-c",
                &touch,
            ],
        ),
        (
            "zero limit",
            vec![
                "run",
                "--profile",
                "engine",
                "--job",
                job_path,
                "--memory-mb",
                "0",
                "--",
                "/bin/sh",
                "-c",
                &touch,
            ],
        ),
    ];
    for (name, args) in cases {
        let output = Command::new(LAUNCHER)
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(125), "{name}: {output:?}");
        assert!(
            !output.stderr.is_empty(),
            "{name}: no explanation on stderr"
        );
        assert!(!marker.exists(), "{name}: the program was started");
    }

    if !sandbox_available("exit_125_when_the_sandbox_cannot_be_applied (kernel part)") {
        return;
    }
    for namespaces in namespace_modes() {
        // A confinement step that fails inside the child: with one descriptor allowed, the
        // child cannot open what it needs to build the Landlock ruleset.
        let mut policy = Policy::new(Profile::Engine, job.path());
        policy.namespaces = namespaces;
        policy.limits.max_open_files = 1;
        let output = hotend_sandbox::command(&policy, "/bin/sh", ["-c", touch.as_str()])
            .unwrap()
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(125), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("sandbox setup failed"), "{stderr}");
        assert!(
            !marker.exists(),
            "the program ran without a complete sandbox"
        );

        // The sandbox applies but the program cannot be executed: distinct codes.
        let policy = common::policy(Profile::Engine, job.path(), namespaces);
        let missing = hotend_sandbox::command(&policy, "/no/such/program", [""; 0])
            .unwrap()
            .output()
            .unwrap();
        assert_eq!(missing.status.code(), Some(127), "{missing:?}");
        // Programs outside the read-only paths, here in the job folder, are not executable.
        let dropped = job.path().join("dropped");
        std::fs::copy("/bin/true", &dropped).unwrap();
        let denied = hotend_sandbox::command(&policy, &dropped, [""; 0])
            .unwrap()
            .output()
            .unwrap();
        assert_eq!(denied.status.code(), Some(126), "{denied:?}");
    }
}
