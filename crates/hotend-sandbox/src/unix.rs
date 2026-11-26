//! Pieces shared by the Unix backends: preparing what the child needs before the fork, the
//! error channel from the child, and translating wait statuses and signals.

use std::ffi::{CString, OsStr, OsString};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};

use crate::error::SandboxError;
use crate::policy::{CHILD_PATH, Policy, prepare_job_dirs};

/// Everything computed before the fork, so the child only has to make system calls.
pub(crate) struct Prepared {
    /// Canonical job folder.
    pub job: PathBuf,
    /// Canonical read-only paths.
    pub read_only: Vec<PathBuf>,
    pub program: CString,
    pub argv: Vec<CString>,
    pub envp: Vec<CString>,
}

pub(crate) fn prepare(
    policy: &Policy,
    program: &OsStr,
    args: &[OsString],
) -> Result<Prepared, SandboxError> {
    policy.validate()?;
    let job = canonical(&policy.job_dir)?;
    prepare_job_dirs(&job)?;
    let read_only = policy
        .read_only
        .iter()
        .map(|p| canonical(p))
        .collect::<Result<Vec<_>, _>>()?;
    let program_path = resolve_program(program)?;

    let mut argv = vec![cstring(program)?];
    for arg in args {
        argv.push(cstring(arg)?);
    }
    let mut envp = Vec::new();
    for (name, value) in policy.child_env(&job, |name| std::env::var_os(name)) {
        let mut entry = name.into_vec();
        entry.push(b'=');
        entry.extend_from_slice(value.as_bytes());
        envp.push(cstring(OsStr::from_bytes(&entry))?);
    }
    Ok(Prepared {
        job,
        read_only,
        program: cstring(program_path.as_os_str())?,
        argv,
        envp,
    })
}

fn canonical(path: &Path) -> Result<PathBuf, SandboxError> {
    std::fs::canonicalize(path)
        .map_err(|e| SandboxError::InvalidPolicy(format!("cannot resolve {}: {e}", path.display())))
}

fn cstring(value: &OsStr) -> Result<CString, SandboxError> {
    CString::new(value.as_bytes()).map_err(|_| {
        SandboxError::InvalidPolicy("arguments and paths cannot contain NUL bytes".into())
    })
}

/// Finds the program without resolving symlinks: a virtual environment's `python` must be
/// executed through its own path to be recognised as one.
fn resolve_program(program: &OsStr) -> Result<PathBuf, SandboxError> {
    let not_found = || SandboxError::Exec {
        program: program.to_string_lossy().into_owned(),
        message: "no such file".into(),
        not_found: true,
    };
    if program.is_empty() {
        return Err(not_found());
    }
    if program.as_bytes().contains(&b'/') {
        let path = Path::new(program);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().map_err(|_| not_found())?.join(path)
        };
        return if path.is_file() {
            Ok(path)
        } else {
            Err(not_found())
        };
    }
    CHILD_PATH
        .split(':')
        .map(|dir| Path::new(dir).join(program))
        .find(|candidate| candidate.is_file())
        .ok_or_else(not_found)
}

/// `Err(last OS error)` for a negative return value.
pub(crate) fn cvt(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

/// The exit code the launcher reports for a wait status: the child's own code, or 128+N when
/// signal N killed it.
pub(crate) fn exit_code(status: libc::c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

/// A close-on-exec pipe: (read end, write end).
pub(crate) fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` is a valid array of two descriptors for the kernel to fill.
    cvt(unsafe { libc::pipe(fds.as_mut_ptr()) })?;
    // SAFETY: on success both descriptors are open and owned by nobody else.
    let ends = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&ends.0, &ends.1] {
        // SAFETY: plain fcntl on a descriptor we own.
        cvt(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) })?;
    }
    Ok(ends)
}

/// A failure reported by the child before the program started.
pub(crate) struct ChildFailure {
    /// The sandbox was applied; only `execve` failed.
    pub exec: bool,
    pub step: &'static str,
    pub error: io::Error,
}

impl ChildFailure {
    pub fn setup(step: &'static str, error: io::Error) -> Self {
        Self {
            exec: false,
            step,
            error,
        }
    }

    #[cfg(target_os = "linux")]
    pub fn other(step: &'static str, message: impl ToString) -> Self {
        Self::setup(step, io::Error::other(message.to_string()))
    }

    /// Sends the failure to the launcher. Runs in the child; errors cannot be reported further.
    pub fn report(&self, fd: RawFd) {
        let errno = self.error.raw_os_error().unwrap_or(0);
        let text = format!(
            "{}\x1f{}\x1f{}\x1f{}",
            if self.exec { "exec" } else { "setup" },
            errno,
            self.step,
            self.error
        );
        // SAFETY: writes a valid buffer to a descriptor inherited from the launcher.
        unsafe { libc::write(fd, text.as_ptr().cast(), text.len()) };
    }
}

/// Reads the child's error channel to its end. `Ok(())` means the program was executed.
pub(crate) fn read_child_report(read_end: OwnedFd, program: &OsStr) -> Result<(), SandboxError> {
    let mut text = String::new();
    let mut file = std::fs::File::from(read_end);
    if let Err(e) = file.read_to_string(&mut text) {
        return Err(SandboxError::Setup {
            step: "read child status".into(),
            message: e.to_string(),
        });
    }
    if text.is_empty() {
        return Ok(());
    }
    let mut parts = text.splitn(4, '\x1f');
    let kind = parts.next().unwrap_or_default();
    let errno: i32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let step = parts.next().unwrap_or("unknown").to_owned();
    let message = parts.next().unwrap_or("unknown error").to_owned();
    if kind == "exec" {
        Err(SandboxError::Exec {
            program: program.to_string_lossy().into_owned(),
            message,
            not_found: errno == libc::ENOENT,
        })
    } else {
        Err(SandboxError::Setup { step, message })
    }
}

/// Last termination signal the launcher received, or 0.
static TERMINATION_SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn note_signal(signal: libc::c_int) {
    TERMINATION_SIGNAL.store(signal, Ordering::SeqCst);
}

/// Makes SIGTERM, SIGINT and SIGHUP set a flag instead of killing the launcher, so it can
/// take the process tree down with it.
pub(crate) fn watch_termination_signals() {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: installs an async-signal-safe handler (one atomic store). No SA_RESTART,
        // so a blocking poll returns early.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = note_signal as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

pub(crate) fn termination_signal() -> Option<i32> {
    match TERMINATION_SIGNAL.load(Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}

/// Puts the signal state back to what a freshly started program expects. Rust ignores
/// SIGPIPE at startup and ignored signals survive `execve`.
pub(crate) fn reset_signals_for_exec() {
    // SAFETY: resets dispositions and the signal mask of the calling (child) process.
    unsafe {
        for signal in [libc::SIGPIPE, libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::signal(signal, libc::SIG_DFL);
        }
        let mut empty: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut empty);
        libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
    }
}

/// `setrlimit` with the same soft and hard value.
pub(crate) fn set_rlimit(resource: libc::c_int, value: u64) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    // SAFETY: passes a valid rlimit struct. The cast covers libc's per-target resource type.
    cvt(unsafe { libc::setrlimit(resource as _, &limit) }).map(drop)
}

/// Replaces the launcher's stdin and stdout with `/dev/null` once the child runs, so the
/// parent sees end-of-file as soon as the child closes its ends.
pub(crate) fn release_stdio() {
    if let Ok(null) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
    {
        // SAFETY: dup2 onto the standard descriptors of this process.
        unsafe {
            libc::dup2(null.as_raw_fd(), 0);
            libc::dup2(null.as_raw_fd(), 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_are_found_by_path_or_on_the_fixed_search_path() {
        assert!(resolve_program(OsStr::new("/bin/sh")).is_ok());
        assert!(resolve_program(OsStr::new("sh")).is_ok());
        for missing in ["/no/such/program", "no-such-program-xyz", ""] {
            let error = resolve_program(OsStr::new(missing)).err();
            assert!(
                matches!(
                    error,
                    Some(SandboxError::Exec {
                        not_found: true,
                        ..
                    })
                ),
                "{missing:?}"
            );
        }
    }
}
