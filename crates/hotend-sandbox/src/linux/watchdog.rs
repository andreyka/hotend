//! The watchdog: makes the job's process tree die with the launcher, however the launcher
//! ends.
//!
//! The parent-death signal reaches only the first process of the tree, and a launcher that
//! is killed with SIGKILL runs no cleanup. Without a PID namespace the descendants of the
//! program would then live on with no supervisor: no wall clock, no tree limits. The
//! watchdog closes that gap. It is a forked copy of the launcher that does nothing but
//! wait on a pipe whose only write end the launcher holds. When that end closes without the
//! "done" byte, the launcher is gone, and the watchdog kills the job's process group, which
//! no member can leave (seccomp denies `setsid` and `setpgid`).
//!
//! It sits in a session of its own and ignores the catchable termination signals, so
//! nothing aimed at the launcher's process group or terminal takes it out, and it holds no
//! descriptor but its pipe, so it keeps no stream of the core open. The job cannot signal
//! it: Landlock (ABI 6), the PID namespace or the seccomp signal guard confine signals to
//! the job's own tree.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd};

use super::cgroup::JobCgroup;
use crate::unix::{cvt, pipe};

/// The byte the launcher writes once the tree is gone and nothing is left to kill.
const DONE: u8 = 1;

/// The launcher's handle on its watchdog.
pub(crate) struct Watchdog {
    pid: libc::pid_t,
    alive: OwnedFd,
}

impl Watchdog {
    /// Forks the watchdog for the process group `group` and the job's cgroup, when it has
    /// one. Call it from the single-threaded launcher, after the job's first process has
    /// been forked, so that the job never inherits the pipe.
    pub fn start(group: libc::pid_t, cgroup: Option<&JobCgroup>) -> io::Result<Self> {
        let (read_end, write_end) = pipe()?;
        // SAFETY: the launcher is single-threaded, so the child may run ordinary code.
        let pid = cvt(unsafe { libc::fork() })?;
        if pid == 0 {
            drop(write_end);
            watch(group, cgroup, read_end);
        }
        Ok(Self {
            pid,
            alive: write_end,
        })
    }

    /// Tells the watchdog that the tree has been killed and reaped, and collects it.
    pub fn disarm(self) {
        // SAFETY: writes one byte from a valid buffer to a pipe this process owns, then
        // waits for this process's own child. A watchdog that is already gone makes the
        // write fail with EPIPE (SIGPIPE is ignored in the launcher), which is fine.
        unsafe {
            libc::write(self.alive.as_raw_fd(), [DONE].as_ptr().cast(), 1);
        }
        drop(self.alive);
        let mut status = 0;
        loop {
            // SAFETY: as above.
            let result = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if result == self.pid || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
            {
                return;
            }
        }
    }
}

/// The watchdog process itself. Never returns.
fn watch(group: libc::pid_t, cgroup: Option<&JobCgroup>, alive: OwnedFd) -> ! {
    let fd = alive.as_raw_fd();
    // SAFETY: plain system calls on the calling process. Failures are ignored on purpose:
    // each step only makes the watchdog harder to remove or tidier, and none of them is
    // needed for the kill below.
    unsafe {
        libc::setsid();
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGPIPE] {
            libc::signal(signal, libc::SIG_IGN);
        }
        libc::prctl(libc::PR_SET_NAME, c"hotend-sbx-wd".as_ptr(), 0, 0, 0);
        // Every descriptor but the pipe: the launcher's stdio belongs to the core, which
        // must see end-of-file when the job ends.
        if fd > 0 {
            libc::syscall(
                libc::SYS_close_range,
                0 as libc::c_uint,
                (fd - 1) as libc::c_uint,
                0,
            );
        }
        libc::syscall(
            libc::SYS_close_range,
            (fd + 1) as libc::c_uint,
            libc::c_uint::MAX,
            0,
        );
        libc::chdir(c"/".as_ptr());
    }
    let launcher_finished = loop {
        let mut byte = [0u8; 1];
        // SAFETY: reads one byte into a valid buffer from a pipe this process owns.
        let read = unsafe { libc::read(fd, byte.as_mut_ptr().cast(), 1) };
        if read == 1 {
            break byte[0] == DONE;
        }
        if read < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        // End of file, or an error that makes the pipe useless: the launcher is gone.
        break false;
    };
    if !launcher_finished {
        // One signal reaches every member: the kernel completes no `fork` while SIGKILL is
        // pending. The loop only covers a first process that had not yet entered its own
        // group when the launcher died.
        for _ in 0..50 {
            // SAFETY: signals the job's process group and its first process. The group's id
            // stays reserved while any member is alive.
            let gone = unsafe {
                libc::kill(group, libc::SIGKILL);
                libc::kill(-group, libc::SIGKILL) != 0
                    && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            };
            if gone {
                break;
            }
            // SAFETY: sleeps the calling thread.
            unsafe { libc::usleep(20_000) };
        }
        // The launcher's copy of the cgroup's open file went with the descriptors above;
        // killing and removing go by path.
        if let Some(cgroup) = cgroup {
            cgroup.kill();
            let _ = cgroup.remove();
        }
    }
    // SAFETY: ends the watchdog without running the launcher's exit handlers.
    unsafe { libc::_exit(0) }
}
