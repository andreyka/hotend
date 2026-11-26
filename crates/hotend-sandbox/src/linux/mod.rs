//! Linux backend.
//!
//! Layers, in the order they are applied to the child, and what each one is responsible for:
//!
//! | Step | Mechanism | Provides |
//! | --- | --- | --- |
//! | 0 | pids cgroup, made by the launcher where it can create one; the child is moved into it before it does anything | process cap, exact, also for root |
//! | 1 | new session (`setsid`), parent-death signal | the tree is one process group; its first process dies with the launcher |
//! | 2 | user, PID, network, IPC, UTS, mount namespaces (optional) | second barrier for network, other processes and the tree kill |
//! | 3 | resource limits | memory, CPU, file size, open files, pending signals, no core dumps; the process cap where no cgroup carries it |
//! | 4 | close every inherited descriptor except stdio | no leaked sockets or files |
//! | 5 | drop all capabilities, `no_new_privs` | no privilege, even as root |
//! | 6 | Landlock (required, ABI 3+) | filesystem; TCP and signal scoping on newer kernels |
//! | 7 | seccomp deny-list (required) | no sockets, no escape system calls, no metadata changes, tree stays in its process group |
//! | 7a | seccomp signal guard (only without step 2 and below Landlock ABI 6) | signals stay inside the tree's process group |
//! | 7b | seccomp process-limit guard (only where the cap is `RLIMIT_NPROC` on the user's total) | the job cannot move its own cap |
//! | 8 | supervisor in the launcher | wall clock, exact task count, tree memory, job folder size; keeps the cap on the user's total in step with the user's other programs |
//! | 9 | watchdog process beside the launcher | the whole tree is killed when the launcher dies, even from SIGKILL |
//!
//! Steps 6 and 7 are mandatory: without either the launcher starts nothing. Step 2 is used
//! when the kernel permits it and skipped otherwise; no guarantee relies on it alone. Where
//! it is missing, step 9 is what ends the descendants of a program whose launcher was
//! killed, and step 7a (or Landlock ABI 6) is what keeps the program from signalling the
//! launcher, the watchdog and every other process of the user.
//!
//! The process cap is never left to the supervisor's polling alone: [`ProcessCap`] names
//! the kernel mechanism a job gets, and a launcher that finds none starts nothing.

mod caps;
mod cgroup;
mod child;
mod disk;
mod fs;
mod namespaces;
mod nproc;
mod seccomp;
mod supervisor;
mod watchdog;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;

pub(crate) use self::fs::on_9p;
use crate::backend::Backend;
use crate::error::SandboxError;
use crate::policy::{Limits, Policy};
use crate::probe::{ProbeReport, ProcessCapMode, ProcessCapReport};
use crate::unix::{self, Prepared};

pub(crate) struct LinuxBackend;

/// The kernel mechanism that keeps a job from having more tasks than its limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessCap {
    /// `RLIMIT_NPROC` inside the job's own user namespace: the kernel counts the job's
    /// tasks and nothing else. Exact.
    UserNamespace,
    /// `pids.max` of a cgroup that holds the job and nothing else. Exact, and the only
    /// one that works for root.
    Cgroup,
    /// `RLIMIT_NPROC` set to the tasks the user runs outside the job plus the job's limit
    /// plus an allowance (`nproc::cap_user_total`), and kept there by the supervisor
    /// while the job runs. The kernel enforces it, but against a count the user's other
    /// programs move: a bound, not an exact cap.
    UserTotal,
}

impl ProcessCap {
    /// The mechanism for a job, or `None` when the kernel offers nothing: `RLIMIT_NPROC`
    /// does not apply to this user (root) and no cgroup can be created.
    ///
    /// `cgroup` is asked only when its answer matters, because finding out creates one.
    fn choose(
        namespaces: bool,
        rlimit_enforced: bool,
        cgroup: impl FnOnce() -> bool,
    ) -> Option<Self> {
        if namespaces && rlimit_enforced {
            Some(Self::UserNamespace)
        } else if cgroup() {
            Some(Self::Cgroup)
        } else if rlimit_enforced {
            Some(Self::UserTotal)
        } else {
            None
        }
    }

    fn report(cap: Option<Self>) -> ProcessCapMode {
        let (mechanism, enforcement, bound) = match cap {
            Some(Self::UserNamespace) => (
                "rlimit-in-user-namespace",
                "kernel-exact",
                "the job's user namespace never holds more tasks than max_processes (its \
                 init process is one of them)"
                    .to_owned(),
            ),
            Some(Self::Cgroup) => (
                "cgroup-pids",
                "kernel-exact",
                "the job's cgroup never holds more tasks than max_processes".to_owned(),
            ),
            Some(Self::UserTotal) => (
                "rlimit-on-user-total",
                "kernel-approximate",
                format!(
                    "the job cannot raise the number of tasks of its user above what the \
                     user's other programs run plus max_processes plus {}, nor into the \
                     last eighth of the user's own limit (ulimit -u); the launcher takes \
                     that count at the start and again every second. So the job can pass \
                     max_processes by {} plus whatever the user's other programs ended \
                     within that second, until the supervisor's exact count kills it. The \
                     count is the kernel's for the numeric user id, across containers",
                    nproc::OTHER_PROGRAMS_ALLOWANCE,
                    nproc::OTHER_PROGRAMS_ALLOWANCE
                ),
            ),
            None => (
                "none",
                "refused",
                "no kernel mechanism caps the process count here, so no job is started \
                 (exit code 125)"
                    .to_owned(),
            ),
        };
        ProcessCapMode {
            mechanism: mechanism.to_owned(),
            enforcement: enforcement.to_owned(),
            bound,
        }
    }
}

/// What to tell a user whose machine offers no process cap: why `RLIMIT_NPROC` is no
/// use (`enforcement`), why no cgroup could be made (`no_cgroup`), and what to do.
fn no_process_cap(enforcement: &nproc::Enforcement, no_cgroup: &str) -> String {
    match enforcement {
        nproc::Enforcement::Unknown(why) => format!(
            "nothing in the kernel can be relied on to cap the number of processes of a \
             job here: whether RLIMIT_NPROC applies to this user could not be found out \
             ({why}), and no pids cgroup can be created ({no_cgroup}). Close other \
             programs of this user or raise its process limit, then try again."
        ),
        _ => format!(
            "nothing in the kernel can cap the number of processes of a job here: \
             RLIMIT_NPROC is never applied to root, and no pids cgroup can be created \
             ({no_cgroup}). Run Hotend as an ordinary user, or where the launcher may \
             create cgroups inside its own (in a container: with a writable cgroup mount)."
        ),
    }
}

/// What the child needs to confine itself. Built before the fork.
pub(crate) struct Context<'a> {
    pub prepared: &'a Prepared,
    pub limits: Limits,
    pub filters: &'a seccomp::Filters,
    pub namespaces: bool,
    /// Install the seccomp signal guard: nothing else on this machine keeps signals in.
    pub signal_guard: bool,
    pub process_cap: ProcessCap,
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
    pub launcher_pid: libc::pid_t,
}

impl Backend for LinuxBackend {
    fn name(&self) -> &'static str {
        "landlock+seccomp"
    }

    fn probe(&self) -> ProbeReport {
        let landlock_abi = landlock_abi();
        let seccomp = seccomp_available() && seccomp::build(false).is_ok();
        let namespaces = namespaces::probe();
        // SAFETY: getuid cannot fail.
        let root = unsafe { libc::getuid() } == 0;
        let abi = landlock_abi.as_ref().copied().unwrap_or(0);
        let has_namespaces = namespaces.is_ok();
        let enforcement = nproc::enforced();
        let rlimit_enforced = enforcement == nproc::Enforcement::Applies;
        let cgroup = cgroup::probe();
        let cap = ProcessCap::choose(has_namespaces, rlimit_enforced, || cgroup.is_ok());
        let cap_without_namespaces = ProcessCap::choose(false, rlimit_enforced, || cgroup.is_ok());

        let reason = match (&landlock_abi, seccomp) {
            (Err(e), _) => Some(format!(
                "Landlock is not available ({e}). Hotend needs Linux 6.2 or newer with \
                 Landlock enabled; on WSL2 run `wsl --update` to get a current kernel."
            )),
            (Ok(abi), _) if *abi < fs::MIN_ABI => Some(format!(
                "this kernel has Landlock ABI {abi}, but ABI {} (Linux 6.2) is the first that \
                 can stop a child from truncating files outside its job folder; on WSL2 run \
                 `wsl --update` to get a current kernel.",
                fs::MIN_ABI
            )),
            (_, false) => {
                Some("seccomp filters are not available on this kernel or CPU architecture".into())
            }
            _ if cap.is_none() => Some(no_process_cap(
                &enforcement,
                cgroup.as_ref().err().map_or("unknown", String::as_str),
            )),
            _ => None,
        };

        let mut guarantees = BTreeMap::new();
        let mut list = |name: &str, entries: Vec<(bool, &str)>| {
            let active = entries
                .into_iter()
                .filter(|(on, _)| *on)
                .map(|(_, text)| text.to_owned())
                .collect::<Vec<_>>();
            guarantees.insert(name.to_owned(), active);
        };
        list(
            "network",
            vec![
                (seccomp, "seccomp: every socket system call is denied"),
                (abi >= 4, "landlock: TCP bind and connect are denied"),
                (has_namespaces, "network namespace with no interface"),
            ],
        );
        list(
            "filesystem",
            vec![
                (
                    abi >= fs::MIN_ABI,
                    "landlock: write only in the job folder; read only the job folder, the \
                     policy's read-only paths and the system runtime paths",
                ),
                (
                    seccomp,
                    "seccomp: mode, owner, timestamp and extended-attribute changes are neutralised",
                ),
            ],
        );
        list(
            "privileges",
            vec![
                (
                    true,
                    "no_new_privs; the effective, permitted, inheritable and ambient capability \
                     sets are empty",
                ),
                (
                    root || has_namespaces,
                    "the capability bounding set is empty",
                ),
                (
                    seccomp,
                    "seccomp: ptrace, mounts, namespaces, bpf, keyrings, kexec, modules, perf, \
                     userfaultfd and io_uring are denied",
                ),
                (
                    abi >= fs::MIN_ABI,
                    "landlock: ptrace only within the sandbox",
                ),
                (abi >= 6, "landlock: signals only within the sandbox"),
                (
                    seccomp && abi < 6 && !has_namespaces,
                    "seccomp: signals only to the program and its own process group",
                ),
                (
                    has_namespaces,
                    "user and PID namespaces: no other process is visible",
                ),
            ],
        );
        list(
            "resources",
            vec![
                (
                    true,
                    "rlimits: address space and CPU time per process, file size, open files, \
                     pending signals, no core dumps",
                ),
                (
                    seccomp,
                    "seccomp: no inotify instance and no POSIX message queue, which the \
                     kernel counts against the whole user account",
                ),
                (
                    cap == Some(ProcessCap::UserNamespace),
                    "process count: RLIMIT_NPROC inside the job's user namespace, enforced by \
                     the kernel, exact",
                ),
                (
                    cap == Some(ProcessCap::Cgroup),
                    "process count: pids.max of a cgroup per job, enforced by the kernel, exact",
                ),
                (
                    cap == Some(ProcessCap::UserTotal),
                    "process count: RLIMIT_NPROC on the total of the user (what the user's \
                     other programs run plus the job's limit plus an allowance, brought up \
                     to date every second), enforced by the kernel, approximate",
                ),
                (
                    true,
                    "supervisor: wall clock, exact task count and resident memory of the whole \
                     tree, checked every 100 ms and within about 10 ms of a burst of new \
                     processes; the tree is killed through its process group, which seccomp \
                     forbids leaving",
                ),
                (
                    true,
                    "supervisor: total size, file count and nesting depth of the job folder",
                ),
                (
                    true,
                    "watchdog: the tree is killed when the launcher dies, even from SIGKILL",
                ),
                (
                    has_namespaces,
                    "PID namespace: killing its first process kills the tree",
                ),
            ],
        );
        list(
            "environment",
            vec![(
                true,
                "the launcher builds the environment from an allowlist; HOME and TMPDIR are inside the job folder",
            )],
        );

        let mut limitations = vec![
            "existence and metadata (stat) of files outside the sandbox are visible".to_owned(),
            "two allowances the kernel keeps per user account are not capped per job: \
             epoll watches (fs.epoll.max_user_watches) and pipe buffer memory \
             (fs.pipe-user-pages-soft); a job that uses them up makes epoll_ctl fail, or \
             new pipes small, for the other programs of its user while it runs"
                .to_owned(),
        ];
        if cap_without_namespaces == Some(ProcessCap::UserTotal) {
            limitations.push(format!(
                "{}the process cap is approximate: {}",
                if has_namespaces {
                    "without the namespace layer (--no-namespaces) "
                } else {
                    ""
                },
                ProcessCap::report(cap_without_namespaces).bound
            ));
        }
        if !has_namespaces {
            if abi < 6 {
                limitations.push(
                    "signals are confined by a seccomp filter that cannot tell processes apart: \
                     a subprocess of the sandboxed program cannot raise a signal in itself or \
                     signal a single other process (lifted by Landlock ABI 6, Linux 6.12, or \
                     by the namespace layer)"
                        .to_owned(),
                );
            }
            if !root {
                limitations.push(
                    "the capability bounding set is not emptied (an unprivileged launcher may \
                     not change it outside a user namespace); with no_new_privs and empty \
                     permitted sets nothing can be gained from it"
                        .to_owned(),
                );
            }
            limitations.push(
                "a child can change the scheduling priority and CPU affinity of other processes \
                 of the same user"
                    .to_owned(),
            );
        }

        ProbeReport {
            platform: "linux".into(),
            backend: self.name().into(),
            usable: reason.is_none(),
            reason,
            kernel: kernel_release(),
            landlock_abi: landlock_abi.ok(),
            seccomp,
            namespaces: has_namespaces,
            namespaces_detail: namespaces.err(),
            root,
            process_cap: ProcessCapReport {
                default: ProcessCap::report(cap),
                without_namespaces: ProcessCap::report(cap_without_namespaces),
                rlimit_enforced,
                rlimit_detail: match enforcement {
                    nproc::Enforcement::Unknown(why) => Some(why),
                    _ => None,
                },
                cgroup: cgroup.as_ref().ok().map(|(kind, _)| kind.clone()),
                cgroup_parent: cgroup.as_ref().ok().map(|(_, parent)| parent.clone()),
                cgroup_detail: cgroup.err(),
            },
            guarantees,
            limitations,
        }
    }

    fn run(
        &self,
        policy: &Policy,
        program: &OsStr,
        args: &[OsString],
    ) -> Result<i32, SandboxError> {
        let prepared = unix::prepare(policy, program, args)?;
        let abi = match landlock_abi() {
            Ok(abi) if abi >= fs::MIN_ABI => abi,
            Ok(abi) => {
                return Err(SandboxError::Unavailable(format!(
                    "Landlock ABI {abi} is too old (ABI {} from Linux 6.2 is required); update \
                     the kernel (`wsl --update` on WSL2)",
                    fs::MIN_ABI
                )));
            }
            Err(e) => {
                return Err(SandboxError::Unavailable(format!(
                    "Landlock is not available ({e}); a kernel 6.2 or newer with Landlock \
                     enabled is required (`wsl --update` on WSL2)"
                )));
            }
        };
        if !seccomp_available() {
            return Err(SandboxError::Unavailable(
                "seccomp filters are not supported by this kernel".into(),
            ));
        }
        let filters = seccomp::build(policy.profile.allows_socketpair()).map_err(|message| {
            SandboxError::Setup {
                step: "compile seccomp filters".into(),
                message,
            }
        })?;
        let namespaces = policy.namespaces && namespaces::probe().is_ok();
        // SAFETY: these calls cannot fail.
        let (uid, gid, launcher_pid, launcher_parent) = unsafe {
            (
                libc::getuid(),
                libc::getgid(),
                libc::getpid(),
                libc::getppid(),
            )
        };

        unix::watch_termination_signals();
        let setup = |step: &str, e: io::Error| SandboxError::Setup {
            step: step.into(),
            message: e.to_string(),
        };
        // Held until the tree is gone; see `fs::pin`.
        let (job, pins) = fs::pin(&prepared.job, &prepared.read_only)
            .map_err(|e| setup("open the job folder and the read-only paths", e))?;
        let (read_end, write_end) = unix::pipe().map_err(|e| setup("pipe", e))?;
        let (gate_read, gate_write) = unix::pipe().map_err(|e| setup("pipe", e))?;

        // The process cap. Last of the preparations: a cgroup exists from here on and
        // every way out has to remove it.
        let mut job_cgroup = None;
        let mut no_cgroup = String::new();
        let enforcement = nproc::enforced();
        let rlimit_enforced = enforcement == nproc::Enforcement::Applies;
        let process_cap = ProcessCap::choose(namespaces, rlimit_enforced, || {
            match cgroup::create(policy.limits.max_processes) {
                Ok(cgroup) => job_cgroup = Some(cgroup),
                Err(reason) => no_cgroup = reason,
            }
            job_cgroup.is_some()
        })
        .ok_or_else(|| SandboxError::Unavailable(no_process_cap(&enforcement, &no_cgroup)))?;
        let context = Context {
            prepared: &prepared,
            limits: policy.limits,
            filters: &filters,
            namespaces,
            signal_guard: !namespaces && abi < fs::SIGNAL_SCOPE_ABI,
            process_cap,
            uid,
            gid,
            launcher_pid,
        };
        let discard = |cgroup: Option<cgroup::JobCgroup>| {
            if let Some(cgroup) = cgroup {
                let _ = cgroup.remove();
            }
        };

        let pid = match if namespaces {
            namespaces::clone_into_namespaces()
        } else {
            // SAFETY: the launcher is single-threaded, so the child may run ordinary code.
            unix::cvt(unsafe { libc::fork() })
        } {
            Ok(pid) => pid,
            Err(e) => {
                discard(job_cgroup);
                return Err(setup("fork", e));
            }
        };
        if pid == 0 {
            drop(read_end);
            drop(gate_write);
            child::run(&context, write_end, gate_read);
        }
        drop(write_end);
        drop(gate_read);

        // The child waits at its gate. It is let through only once it is in its cgroup
        // and the watchdog stands, so no process of the job ever runs uncounted or
        // unwatched.
        if let Some(cgroup) = &job_cgroup
            && let Err(e) = cgroup.add(pid)
        {
            supervisor::kill_tree(pid, None);
            discard(job_cgroup);
            return Err(setup("move the job into its cgroup", e));
        }
        let pidfd = match supervisor::pidfd_open(pid) {
            Ok(pidfd) => pidfd,
            Err(e) => {
                supervisor::kill_tree(pid, job_cgroup.as_ref());
                discard(job_cgroup);
                return Err(setup("pidfd_open", e));
            }
        };
        // Forked after the job's first process, so the job never holds the watchdog's pipe.
        let watchdog = match watchdog::Watchdog::start(pid, job_cgroup.as_ref()) {
            Ok(watchdog) => watchdog,
            Err(e) => {
                supervisor::kill_tree(pid, job_cgroup.as_ref());
                discard(job_cgroup);
                return Err(setup("start the watchdog", e));
            }
        };
        let supervisor = supervisor::Supervisor::new(
            pid,
            pidfd,
            launcher_parent,
            job,
            pins,
            watchdog,
            job_cgroup,
        );
        let supervisor = match process_cap {
            ProcessCap::UserTotal => supervisor.following_the_user(uid),
            ProcessCap::UserNamespace | ProcessCap::Cgroup => supervisor,
        };
        // SAFETY: writes one byte from a valid buffer to a pipe this process owns. A child
        // that is already gone makes it fail, and the report below says why it went.
        unsafe {
            libc::write(
                std::os::fd::AsRawFd::as_raw_fd(&gate_write),
                [1u8].as_ptr().cast(),
                1,
            );
        }
        drop(gate_write);
        if let Err(error) = unix::read_child_report(read_end, program) {
            supervisor.abandon();
            return Err(error);
        }
        unix::release_stdio();
        Ok(supervisor.watch(&policy.limits))
    }
}

/// The Landlock ABI of the running kernel, or why there is none.
fn landlock_abi() -> Result<u32, String> {
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;
    // SAFETY: with a null attribute and size 0 this only queries the ABI version.
    let version = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if version > 0 {
        return Ok(version as u32);
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::EOPNOTSUPP) => Err("built into the kernel but disabled at boot".into()),
        Some(libc::ENOSYS) => Err("not built into this kernel".into()),
        _ => Err(io::Error::last_os_error().to_string()),
    }
}

fn seccomp_available() -> bool {
    const SECCOMP_GET_ACTION_AVAIL: libc::c_uint = 2;
    let action: u32 = libc::SECCOMP_RET_ERRNO;
    // SAFETY: the kernel only reads the action value.
    unsafe { libc::syscall(libc::SYS_seccomp, SECCOMP_GET_ACTION_AVAIL, 0usize, &action) == 0 }
}

fn kernel_release() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_owned())
}
