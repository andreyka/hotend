//! The optional extra layer: fresh user, PID, network, IPC, UTS and mount namespaces.
//!
//! No guarantee depends on this layer; Landlock, seccomp and the supervisor provide them
//! all. Where the kernel allows it, namespaces add a second, independent barrier: no network
//! interface exists at all, no other process is visible or signalable, and killing the first
//! process of the PID namespace takes every descendant with it.
//!
//! The layer is all or nothing. Either every namespace and the private `/proc` can be set
//! up, or none is used.

use std::ffi::CStr;
use std::io::{self, Read};

use crate::unix::{cvt, pipe};

pub(crate) const CLONE_FLAGS: libc::c_int = libc::CLONE_NEWUSER
    | libc::CLONE_NEWPID
    | libc::CLONE_NEWNET
    | libc::CLONE_NEWIPC
    | libc::CLONE_NEWUTS
    | libc::CLONE_NEWNS;

/// `fork` that puts the child into new namespaces. Returns 0 in the child.
///
/// The raw system call is used because glibc's `clone` needs a separate stack. The child
/// must not rely on glibc's cached thread id; it only sets itself up and calls `fork` or
/// `execve`. Must be called from a single-threaded process.
pub(crate) fn clone_into_namespaces() -> io::Result<libc::pid_t> {
    let flags = (CLONE_FLAGS | libc::SIGCHLD) as libc::c_ulong;
    // SAFETY: with a null stack the child continues on a copy of this stack, like fork. The
    // remaining arguments (tid pointers, TLS) are unused with these flags.
    let pid = unsafe { libc::syscall(libc::SYS_clone, flags, 0usize, 0usize, 0usize, 0usize) };
    if pid < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(pid as libc::pid_t)
    }
}

/// Finishes the namespaces from inside: maps the caller's own ids one to one, detaches the
/// mount tree from the host and mounts a `/proc` that shows only this PID namespace.
///
/// `uid` and `gid` are the ids the launcher had before the clone.
pub(crate) fn setup_inside(
    uid: libc::uid_t,
    gid: libc::gid_t,
) -> Result<(), (&'static str, io::Error)> {
    std::fs::write("/proc/self/setgroups", "deny").map_err(|e| ("setgroups", e))?;
    std::fs::write("/proc/self/uid_map", format!("{uid} {uid} 1")).map_err(|e| ("uid_map", e))?;
    std::fs::write("/proc/self/gid_map", format!("{gid} {gid} 1")).map_err(|e| ("gid_map", e))?;
    mount(None, c"/", None, libc::MS_REC | libc::MS_PRIVATE).map_err(|e| ("private mounts", e))?;
    mount(
        Some(c"proc"),
        c"/proc",
        Some(c"proc"),
        libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
    )
    .map_err(|e| ("mount /proc", e))?;
    Ok(())
}

fn mount(
    source: Option<&CStr>,
    target: &CStr,
    fstype: Option<&CStr>,
    flags: libc::c_ulong,
) -> io::Result<()> {
    let ptr = |s: Option<&CStr>| s.map_or(std::ptr::null(), CStr::as_ptr);
    // SAFETY: all pointers are null or valid NUL-terminated strings.
    cvt(unsafe {
        libc::mount(
            ptr(source),
            target.as_ptr(),
            ptr(fstype),
            flags,
            std::ptr::null(),
        )
    })
    .map(drop)
}

/// Tries the whole layer in a throwaway child. `Err` says which step the kernel refused.
pub(crate) fn probe() -> Result<(), String> {
    // SAFETY: getuid and getgid cannot fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let (read_end, write_end) = pipe().map_err(|e| e.to_string())?;
    let pid = clone_into_namespaces().map_err(|e| format!("clone: {e}"))?;
    if pid == 0 {
        let code = match setup_inside(uid, gid) {
            Ok(()) => 0,
            Err((step, error)) => {
                let text = format!("{step}: {error}");
                // SAFETY: writes a valid buffer to the inherited pipe.
                unsafe {
                    libc::write(
                        std::os::fd::AsRawFd::as_raw_fd(&write_end),
                        text.as_ptr().cast(),
                        text.len(),
                    )
                };
                1
            }
        };
        // SAFETY: ends the throwaway child without running the launcher's exit handlers.
        unsafe { libc::_exit(code) };
    }
    drop(write_end);
    let mut message = String::new();
    let _ = std::fs::File::from(read_end).read_to_string(&mut message);
    let mut status = 0;
    // SAFETY: waits for the child created above.
    unsafe { libc::waitpid(pid, &mut status, 0) };
    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
        Ok(())
    } else if message.is_empty() {
        Err("namespace setup failed".into())
    } else {
        Err(message)
    }
}
