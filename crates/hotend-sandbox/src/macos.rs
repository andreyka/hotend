//! macOS backend: Seatbelt through `/usr/bin/sandbox-exec`.
//!
//! UNTESTED ON REAL HARDWARE: this code has been type-checked for macOS but never executed
//! on one. It also gives less than the Linux backend, so it is off by default:
//!
//! - network and filesystem come from the Seatbelt profile in `seatbelt.rs`, unverified;
//! - there is no cap on the number of processes or on the memory of the tree, and
//!   `RLIMIT_AS` is not reliably enforced by macOS;
//! - the wall-clock kill goes to the process group, and nothing stops a descendant from
//!   leaving the group with `setsid`.
//!
//! Until someone verifies and completes it on a Mac, `run` fails closed unless
//! `HOTEND_SANDBOX_MACOS_UNTESTED=1` is set.

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::fd::IntoRawFd;
use std::os::unix::ffi::OsStrExt;
use std::time::{Duration, Instant};

use crate::backend::Backend;
use crate::command::MACOS_OPT_IN_ENV as OPT_IN_ENV;
use crate::error::{EXIT_SANDBOX_FAILED, EXIT_WALL_CLOCK, SandboxError, report};
use crate::policy::Policy;
use crate::probe::ProbeReport;
use crate::seatbelt;
use crate::unix::{self, ChildFailure, cvt};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

pub(crate) struct SeatbeltBackend;

fn refusal() -> String {
    format!(
        "the macOS Seatbelt backend has never been run on a Mac and does not cap process \
         count or tree memory; set {OPT_IN_ENV}=1 to try it on a development machine"
    )
}

fn opted_in() -> bool {
    std::env::var_os(OPT_IN_ENV).is_some_and(|v| v == "1")
}

impl Backend for SeatbeltBackend {
    fn name(&self) -> &'static str {
        "seatbelt (untested)"
    }

    fn probe(&self) -> ProbeReport {
        let mut report = ProbeReport::unsupported("macos", self.name(), refusal());
        if !std::path::Path::new(SANDBOX_EXEC).exists() {
            report.reason = Some(format!("{SANDBOX_EXEC} is missing"));
        } else if opted_in() {
            report.usable = true;
            report.reason = None;
        }
        report.limitations = vec![
            "the Seatbelt profile is unverified".into(),
            "no cap on the number of processes or on the memory of the tree".into(),
            "a descendant that calls setsid escapes the wall-clock kill".into(),
        ];
        report
    }

    fn run(
        &self,
        policy: &Policy,
        program: &OsStr,
        args: &[OsString],
    ) -> Result<i32, SandboxError> {
        if !opted_in() {
            return Err(SandboxError::Unavailable(refusal()));
        }
        let prepared = unix::prepare(policy, program, args)?;
        let profile = seatbelt::profile(&prepared.job, &prepared.read_only);
        let cstring = |value: &OsStr| {
            CString::new(value.as_bytes())
                .map_err(|_| SandboxError::InvalidPolicy("paths cannot contain NUL bytes".into()))
        };
        // sandbox-exec -p <profile> -D NAME=value... <program> <args>...
        let sandbox_exec = cstring(OsStr::new(SANDBOX_EXEC))?;
        let mut argv = vec![sandbox_exec.clone()];
        for argument in profile.arguments() {
            argv.push(cstring(&argument)?);
        }
        argv.push(prepared.program.clone());
        argv.extend(prepared.argv.iter().skip(1).cloned());

        unix::watch_termination_signals();
        let setup = |step: &str, e: io::Error| SandboxError::Setup {
            step: step.into(),
            message: e.to_string(),
        };
        let (read_end, write_end) = unix::pipe().map_err(|e| setup("pipe", e))?;
        // SAFETY: the launcher is single-threaded, so the child may run ordinary code.
        let pid = cvt(unsafe { libc::fork() }).map_err(|e| setup("fork", e))?;
        if pid == 0 {
            drop(read_end);
            let status = write_end.into_raw_fd();
            let failure = start(policy, &prepared, &sandbox_exec, &argv);
            failure.report(status);
            // SAFETY: ends the child without running the launcher's exit handlers.
            unsafe { libc::_exit(EXIT_SANDBOX_FAILED) }
        }
        drop(write_end);
        if let Err(error) = unix::read_child_report(read_end, program) {
            kill_and_reap(pid);
            return Err(error);
        }
        unix::release_stdio();
        Ok(watch(pid, policy.limits.wall_seconds))
    }
}

/// Runs in the child. Returns only if something failed.
fn start(
    policy: &Policy,
    prepared: &unix::Prepared,
    sandbox_exec: &CString,
    argv: &[CString],
) -> ChildFailure {
    // SAFETY: plain system calls on the calling process.
    if let Err(e) = cvt(unsafe { libc::setsid() }) {
        return ChildFailure::setup("setsid", e);
    }
    if let Err(e) = std::env::set_current_dir(&prepared.job) {
        return ChildFailure::setup("chdir", e);
    }
    unix::reset_signals_for_exec();
    let limits = &policy.limits;
    let megabyte = 1024 * 1024;
    for (step, resource, value) in [
        ("CPU time limit", libc::RLIMIT_CPU, limits.cpu_seconds),
        (
            "file size limit",
            libc::RLIMIT_FSIZE,
            limits.max_file_mb.saturating_mul(megabyte),
        ),
        (
            "open files limit",
            libc::RLIMIT_NOFILE,
            limits.max_open_files,
        ),
        ("core dump limit", libc::RLIMIT_CORE, 0),
    ] {
        if let Err(e) = unix::set_rlimit(resource as libc::c_int, value) {
            return ChildFailure::setup(step, e);
        }
    }
    // Accepted by macOS but not reliably enforced; an error is not fatal for that reason.
    let _ = unix::set_rlimit(
        libc::RLIMIT_AS as libc::c_int,
        limits.memory_mb.saturating_mul(megabyte),
    );

    let mut arg_pointers: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    arg_pointers.push(std::ptr::null());
    let mut env_pointers: Vec<*const libc::c_char> =
        prepared.envp.iter().map(|e| e.as_ptr()).collect();
    env_pointers.push(std::ptr::null());
    // SAFETY: both arrays are null-terminated and hold valid C strings that outlive the call.
    unsafe {
        libc::execve(
            sandbox_exec.as_ptr(),
            arg_pointers.as_ptr(),
            env_pointers.as_ptr(),
        )
    };
    ChildFailure {
        exec: true,
        step: "execve sandbox-exec",
        error: io::Error::last_os_error(),
    }
}

fn kill_and_reap(pid: libc::pid_t) -> libc::c_int {
    let mut status = 0;
    // SAFETY: signals the child's process group and waits for the launcher's own child.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, &mut status, 0);
    }
    status
}

fn watch(pid: libc::pid_t, wall_seconds: u64) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(wall_seconds);
    loop {
        let mut status = 0;
        // SAFETY: polls the launcher's own child.
        if unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == pid {
            // SAFETY: kills what the program left behind in its process group.
            unsafe { libc::kill(-pid, libc::SIGKILL) };
            return unix::exit_code(status);
        }
        if let Some(signal) = unix::termination_signal() {
            kill_and_reap(pid);
            return 128 + signal;
        }
        if Instant::now() >= deadline {
            kill_and_reap(pid);
            report(format_args!(
                "wall-clock limit of {wall_seconds} s reached; the process group was killed"
            ));
            return EXIT_WALL_CLOCK;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
