//! What runs between the fork and the program: the child confines itself, then executes.
//!
//! Nothing here returns to the launcher's code. Failures travel through the status pipe and
//! the child exits; a child that cannot be fully confined never reaches `execve`.

use std::convert::Infallible;
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, IntoRawFd, OwnedFd};

use super::{Context, ProcessCap, caps, fs, namespaces, nproc, seccomp};
use crate::error::EXIT_SANDBOX_FAILED;
use crate::unix::{self, ChildFailure, cvt};

const MIB: u64 = 1024 * 1024;

/// Signals a job may have queued at one time (`RLIMIT_SIGPENDING`), each POSIX timer
/// counting as one. The kernel counts them for the user, as it counts tasks: inside the
/// job's user namespace that is the job's own number; without one it is the number of the
/// whole account, which the job then cannot push past this value. Either way a job can no
/// longer use up the account's allowance (some tens of thousands) and stop real-time
/// signals and timers for every other program of the user. Ordinary signals are delivered
/// even at the limit; only `sigqueue`, real-time signals and `timer_create` fail there.
pub(crate) const PENDING_SIGNALS: u64 = 1024;

/// Entry point of the forked child. `gate` is the pipe on which the launcher says that
/// the child is where it belongs (in the job's cgroup, when there is one).
pub(crate) fn run(context: &Context<'_>, status: OwnedFd, gate: OwnedFd) -> ! {
    let status = status.into_raw_fd();
    let Err(failure) = enter(context, gate);
    failure.report(status);
    // SAFETY: ends the child without running the launcher's exit handlers.
    unsafe { libc::_exit(EXIT_SANDBOX_FAILED) }
}

fn enter(context: &Context<'_>, gate: OwnedFd) -> Result<Infallible, ChildFailure> {
    // A session of its own: one process group for the supervisor to kill, and no controlling
    // terminal to type into.
    // SAFETY: plain system calls on the calling process.
    cvt(unsafe { libc::setsid() }).map_err(|e| ChildFailure::setup("setsid", e))?;
    // Reaches this process only; the watchdog (`super::watchdog`) takes care of the rest of
    // the tree when the launcher is killed.
    // SAFETY: as above.
    cvt(unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) })
        .map_err(|e| ChildFailure::setup("parent-death signal", e))?;
    // SAFETY: as above. Inside a PID namespace the parent is invisible (0), so the check
    // only applies without one; there the launcher's death kills the namespace anyway.
    if !context.namespaces && unsafe { libc::getppid() } != context.launcher_pid {
        return Err(ChildFailure::other("start", "the launcher is gone"));
    }
    // Nothing is forked and no line of the program runs before the launcher has put this
    // process into the job's cgroup: whatever comes later is counted against the cap.
    let mut byte = [0u8; 1];
    // SAFETY: reads one byte into a valid buffer from a pipe this process owns.
    if unsafe { libc::read(gate.as_raw_fd(), byte.as_mut_ptr().cast(), 1) } != 1 {
        return Err(ChildFailure::other("start", "the launcher is gone"));
    }
    drop(gate);

    // In a PID namespace the program waits for this signal from its init before it starts.
    let mut start_signal: Option<OwnedFd> = None;
    if context.namespaces {
        namespaces::setup_inside(context.uid, context.gid)
            .map_err(|(step, e)| ChildFailure::setup(step, e))?;
        let (ready_read, ready_write) = unix::pipe().map_err(|e| ChildFailure::setup("pipe", e))?;
        // This process is the first one of the PID namespace. Programs make poor inits
        // (default-action signals are ignored for PID 1), so it stays behind as a minimal
        // one and the program runs as its child.
        // SAFETY: single-threaded process.
        let program = cvt(unsafe { libc::fork() }).map_err(|e| ChildFailure::setup("fork", e))?;
        if program != 0 {
            drop(ready_read);
            // Exiting on failure is enough: when the init of a PID namespace exits, the
            // kernel kills everything else in it, and the program has not started yet.
            confine(context, false)?;
            // SAFETY: writes one byte to a pipe this process owns, then closes every
            // descriptor it holds, so the launcher and the core see end-of-file as soon as
            // the program closes its own ends. The init needs none of them to wait.
            unsafe {
                libc::write(ready_write.as_raw_fd(), [1u8].as_ptr().cast(), 1);
                libc::syscall(
                    libc::SYS_close_range,
                    0 as libc::c_uint,
                    libc::c_uint::MAX,
                    0,
                );
            }
            std::mem::forget(ready_write);
            init(program);
        }
        drop(ready_write);
        start_signal = Some(ready_read);
    }

    let proc_self = confine(context, true)?;
    // Stays open across execve on purpose: it keeps the Landlock rule for the program's own
    // /proc entries alive (see `fs::apply`).
    if let Some(fd) = proc_self {
        let _ = fd.into_raw_fd();
    }
    if let Some(ready) = start_signal {
        let mut byte = [0u8; 1];
        // SAFETY: reads one byte into a valid buffer from a pipe this process owns.
        if unsafe { libc::read(ready.as_raw_fd(), byte.as_mut_ptr().cast(), 1) } != 1 {
            return Err(ChildFailure::other("start", "the namespace init failed"));
        }
    }

    let prepared = context.prepared;
    let mut argv: Vec<*const libc::c_char> = prepared.argv.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    let mut envp: Vec<*const libc::c_char> = prepared.envp.iter().map(|e| e.as_ptr()).collect();
    envp.push(std::ptr::null());
    // SAFETY: argv and envp are null-terminated arrays of valid C strings that outlive the call.
    unsafe { libc::execve(prepared.program.as_ptr(), argv.as_ptr(), envp.as_ptr()) };
    Err(ChildFailure {
        exec: true,
        step: "execve",
        error: io::Error::last_os_error(),
    })
}

/// Applies every restriction to the calling process. For the program itself (`is_program`)
/// the returned handle on its `/proc` directory must stay open.
fn confine(context: &Context<'_>, is_program: bool) -> Result<Option<OwnedFd>, ChildFailure> {
    let prepared = context.prepared;
    std::env::set_current_dir(&prepared.job).map_err(|e| ChildFailure::setup("chdir", e))?;
    // SAFETY: umask cannot fail.
    unsafe { libc::umask(0o022) };
    unix::reset_signals_for_exec();

    let limits = &context.limits;
    let rlimits = [
        (
            "address space limit",
            libc::RLIMIT_AS,
            limits.memory_mb.saturating_mul(MIB),
        ),
        ("CPU time limit", libc::RLIMIT_CPU, limits.cpu_seconds),
        (
            "file size limit",
            libc::RLIMIT_FSIZE,
            limits.max_file_mb.saturating_mul(MIB),
        ),
        (
            "open files limit",
            libc::RLIMIT_NOFILE,
            limits.max_open_files,
        ),
        ("core dump limit", libc::RLIMIT_CORE, 0),
        (
            "pending signal limit",
            libc::RLIMIT_SIGPENDING,
            pending_signals(),
        ),
        // POSIX message queues are refused by seccomp; this is the same door locked twice.
        ("message queue limit", libc::RLIMIT_MSGQUEUE, 0),
    ];
    for (step, resource, value) in rlimits {
        unix::set_rlimit(resource as libc::c_int, value)
            .map_err(|e| ChildFailure::setup(step, e))?;
    }
    if context.process_cap == ProcessCap::UserNamespace {
        // The count the kernel compares with is the one of this user namespace, which
        // holds the job and nothing else.
        nproc::cap_namespace(limits.max_processes)
            .map_err(|e| ChildFailure::setup("process limit", e))?;
    }

    // Everything inherited except stdio closes at execve.
    // SAFETY: flags descriptors of the calling process only.
    cvt(unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3 as libc::c_uint,
            libc::c_uint::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    } as libc::c_int)
    .map_err(|e| ChildFailure::setup("close inherited descriptors", e))?;

    let proc_self = if is_program {
        // SAFETY: opens a path with a valid C string; the result is checked.
        let fd =
            cvt(unsafe { libc::open(c"/proc/self".as_ptr(), libc::O_PATH | libc::O_DIRECTORY) })
                .map_err(|e| ChildFailure::setup("open /proc/self", e))?;
        // SAFETY: `fd` was just opened and is owned by nothing else.
        Some(unsafe { OwnedFd::from_raw_fd(fd) })
    } else {
        None
    };

    caps::drop_all().map_err(|e| ChildFailure::setup("drop capabilities", e))?;
    if context.process_cap == ProcessCap::UserTotal {
        // After the capabilities are gone, because holding one exempts from the limit.
        nproc::cap_user_total(limits.max_processes)
            .map_err(|message| ChildFailure::other("process limit", message))?;
    }
    // SAFETY: prctl with integer arguments only.
    cvt(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) })
        .map_err(|e| ChildFailure::setup("no_new_privs", e))?;
    fs::apply(
        &prepared.job,
        &prepared.read_only,
        proc_self.as_ref().map(|fd| fd.as_fd()),
    )
    .map_err(|message| ChildFailure::other("landlock", message))?;
    context
        .filters
        .apply()
        .map_err(|message| ChildFailure::other("seccomp", message))?;
    if context.process_cap == ProcessCap::UserTotal {
        // The soft limit set above is the cap, and only the supervisor may move it.
        seccomp::nproc_guard()
            .and_then(|guard| seccomp::apply_program(&guard))
            .map_err(|message| ChildFailure::other("seccomp process-limit guard", message))?;
    }
    if context.signal_guard {
        // Compiled here because it names this process: without a PID namespace the program
        // is this very process, and its id is the id of the tree's process group.
        // SAFETY: getpid cannot fail.
        seccomp::signal_guard(unsafe { libc::getpid() })
            .and_then(|guard| seccomp::apply_program(&guard))
            .map_err(|message| ChildFailure::other("seccomp signal guard", message))?;
    }
    Ok(proc_self)
}

/// [`PENDING_SIGNALS`], or seven eighths of the limit the launcher runs under where that
/// is less: a hard limit can only be lowered, and a job never takes the whole of it.
fn pending_signals() -> u64 {
    let mut own = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: passes a valid rlimit struct for the kernel to fill.
    if unsafe { libc::getrlimit(libc::RLIMIT_SIGPENDING, &mut own) } != 0
        || own.rlim_max == libc::RLIM_INFINITY
    {
        return PENDING_SIGNALS;
    }
    let hard = own.rlim_max;
    PENDING_SIGNALS.min(hard - hard / 8)
}

/// The init of the PID namespace: reaps whatever is reparented to it and exits with the
/// program's result. Its exit makes the kernel kill every process left in the namespace.
fn init(program: libc::pid_t) -> ! {
    loop {
        let mut status = 0;
        // SAFETY: waits for any child of the calling process.
        let pid = unsafe { libc::waitpid(-1, &mut status, 0) };
        if pid == program {
            // SAFETY: ends the init process.
            unsafe { libc::_exit(unix::exit_code(status)) };
        }
        if pid < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            // SAFETY: as above.
            unsafe { libc::_exit(EXIT_SANDBOX_FAILED) };
        }
    }
}
