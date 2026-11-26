//! `RLIMIT_NPROC` as the process cap of a job, where the kernel enforces it.
//!
//! What the kernel does (`copy_process` in `kernel/fork.c`, Linux 6.18): a `fork` or
//! `clone` fails with `EAGAIN` when the number of tasks (processes and threads) of the
//! caller's real user id, in the caller's user namespace and the namespaces below it,
//! would exceed the caller's own soft `RLIMIT_NPROC`. Two exemptions: the real user is
//! root of the initial user namespace (whatever capabilities the process still holds), or
//! the caller has `CAP_SYS_RESOURCE` or `CAP_SYS_ADMIN` there. The count belongs to the
//! user, the limit to the process.
//!
//! That gives two caps:
//!
//! - Inside a fresh user namespace the count starts at one, so the limit is the job's own
//!   task count: exact. [`cap_namespace`] sets it.
//! - Without a user namespace the count is everything the user runs. [`cap_user_total`]
//!   measures that count and sets the soft limit to it plus the job's allowance. Other
//!   programs of the user keep their own, much higher limit, so they can still fork while
//!   a job sits at its cap; the job can never push the user's count past the value set
//!   here. The cap moves with what else the user starts and stops, so it is a bound, not
//!   a count: see [`OTHER_PROGRAMS_ALLOWANCE`]. The supervisor moves the soft limit along
//!   with the user's other programs while the job runs (`super::supervisor`), never past
//!   the hard limit set here, and a seccomp filter keeps the job from moving it itself.
//!
//! Neither works for root of the initial user namespace, which is why root needs a cgroup
//! (`super::cgroup`). The count is per user namespace since Linux 5.14; Landlock ABI 3,
//! which the launcher requires, means 6.2 or newer.

use std::io;

use super::caps;
use crate::error::report;

/// Tasks the user's other programs may start while a job runs before they eat into the
/// job's own allowance. Without it an agent that runs for an hour would lose the ability
/// to start a thread as soon as the user opened a browser. It is also how far past its
/// `max_processes` a job can get before the supervisor's exact count ends it.
pub(crate) const OTHER_PROGRAMS_ALLOWANCE: u64 = 1024;

/// No task count can pass the largest process id (`PID_MAX_LIMIT`).
const MOST_TASKS: u64 = 4 * 1024 * 1024;

/// Whether the kernel applies `RLIMIT_NPROC` to this user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Enforcement {
    /// A fork was refused under a limit of 0 and allowed under the user's own limit.
    Applies,
    /// A fork went through under a limit of 0: root of the initial user namespace.
    Exempt,
    /// The trial could not tell: a fork failed for another reason. Says which.
    Unknown(String),
}

/// Exit codes of the throwaway child of [`enforced`].
const TRIAL_APPLIES: libc::c_int = 0;
const TRIAL_EXEMPT: libc::c_int = 1;
const TRIAL_REFUSED_ANYWAY: libc::c_int = 2;
const TRIAL_FAILED: libc::c_int = 3;

/// Whether the kernel refuses a `fork` of this user over `RLIMIT_NPROC` once capabilities
/// are dropped. Tried for real in a throwaway child, because the rule depends on the user
/// id as the kernel sees it, which a process inside a container cannot tell.
///
/// `EAGAIN` from `fork` has other causes than this limit (a pids cgroup around the
/// launcher, the machine's thread or process id limits), so a refusal counts only with its
/// control: the same child must be able to fork under the limit it started with. A trial
/// that cannot tell is repeated once.
pub(crate) fn enforced() -> Enforcement {
    match enforced_once() {
        Enforcement::Unknown(_) => enforced_once(),
        known => known,
    }
}

fn enforced_once() -> Enforcement {
    // SAFETY: the child only makes system calls and exits; the parent collects it.
    let status = unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return Enforcement::Unknown(format!(
                "the launcher could not fork: {}",
                io::Error::last_os_error()
            ));
        }
        if pid == 0 {
            libc::_exit(trial());
        }
        let mut status = 0;
        libc::waitpid(pid, &mut status, 0);
        status
    };
    match libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status)) {
        Some(TRIAL_APPLIES) => Enforcement::Applies,
        Some(TRIAL_EXEMPT) => Enforcement::Exempt,
        Some(TRIAL_REFUSED_ANYWAY) => Enforcement::Unknown(
            "a fork is refused whatever RLIMIT_NPROC is set to: this user, or a cgroup the \
             launcher runs in, is at its process limit"
                .into(),
        ),
        _ => Enforcement::Unknown("the trial process failed".into()),
    }
}

/// Runs in the throwaway child of [`enforced_once`].
fn trial() -> libc::c_int {
    let Ok(own) = limits() else {
        return TRIAL_FAILED;
    };
    if caps::drop_all().is_err() {
        return TRIAL_FAILED;
    }
    match fork_allowed_at(0) {
        Ok(true) => TRIAL_EXEMPT,
        Ok(false) => match fork_allowed_at(own.rlim_cur) {
            Ok(true) => TRIAL_APPLIES,
            Ok(false) => TRIAL_REFUSED_ANYWAY,
            Err(_) => TRIAL_FAILED,
        },
        Err(_) => TRIAL_FAILED,
    }
}

fn limits() -> io::Result<libc::rlimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: passes a valid rlimit struct for the kernel to fill.
    if unsafe { libc::getrlimit(libc::RLIMIT_NPROC, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(limit)
}

/// Sets the soft limit to `soft` and tries one `fork`. `Ok(false)` means the kernel
/// refused it over the limit. The child exits at once and is collected.
fn fork_allowed_at(soft: u64) -> io::Result<bool> {
    let limit = libc::rlimit {
        rlim_cur: soft as libc::rlim_t,
        rlim_max: limits()?.rlim_max,
    };
    // SAFETY: passes a valid rlimit struct; the forked child exits immediately without
    // running any other code, and the parent waits for exactly that child.
    unsafe {
        if libc::setrlimit(libc::RLIMIT_NPROC, &limit) != 0 {
            return Err(io::Error::last_os_error());
        }
        let pid = libc::fork();
        if pid == 0 {
            libc::_exit(0);
        }
        if pid < 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::EAGAIN) {
                Ok(false)
            } else {
                Err(error)
            };
        }
        let mut status = 0;
        libc::waitpid(pid, &mut status, 0);
    }
    Ok(true)
}

/// Caps the calling process and everything it will fork at `max_processes` tasks. For a
/// process in a user namespace of its own, where that is the number of tasks of the job.
///
/// A user whose own limit (`ulimit -u`) is lower keeps the lower one: only a privileged
/// process may raise a hard limit, and the job is still capped, just below its allowance.
///
/// The cap is tried before it is trusted: a fork under a limit of 0 must be refused here,
/// in the namespace the job runs in. It is not for root of the initial user namespace,
/// whose job must never get this far (`enforced`), and this is what stops it if it does.
pub(crate) fn cap_namespace(max_processes: u64) -> io::Result<()> {
    let cap = max_processes.min(limits()?.rlim_max as u64);
    let limit = libc::rlimit {
        rlim_cur: cap as libc::rlim_t,
        rlim_max: cap as libc::rlim_t,
    };
    // SAFETY: passes a valid rlimit struct.
    if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if fork_allowed_at(0)? {
        return Err(io::Error::other(
            "the kernel does not apply RLIMIT_NPROC to this user, so it cannot cap the job",
        ));
    }
    // SAFETY: as above; puts the soft limit back after the trial.
    if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The number of tasks the kernel currently counts for this user, the caller included.
///
/// There is no file that shows the kernel's count, and adding up `/proc` gives a different
/// number wherever some of the user's processes are invisible (another PID namespace) or
/// run under other ids inside a user namespace the user owns. So the count is taken from
/// the check itself: the smallest soft limit at which a `fork` still succeeds is the count
/// plus the one new process, found by bisection in about twenty forks.
fn user_tasks(soft: u64) -> Result<u64, String> {
    let fail = |e: io::Error| format!("cannot measure the user's process count: {e}");
    if fork_allowed_at(0).map_err(fail)? {
        return Err("the kernel does not apply RLIMIT_NPROC to this user".into());
    }
    let mut allowed = soft.min(MOST_TASKS);
    if !fork_allowed_at(allowed).map_err(fail)? {
        return Err(format!(
            "this user has reached its process limit (`ulimit -u` is {soft}); close other \
             programs or raise the limit"
        ));
    }
    let mut refused = 0;
    while allowed - refused > 1 {
        let middle = refused + (allowed - refused) / 2;
        if fork_allowed_at(middle).map_err(fail)? {
            allowed = middle;
        } else {
            refused = middle;
        }
    }
    Ok(allowed - 1)
}

/// The limits for a job of `max_processes` tasks whose user runs `others` tasks besides
/// the job's first process (the launcher and its watchdog among them), under a soft limit
/// of `soft` (`ulimit -u`, which is what the user's other programs run under).
///
/// Returns the cap (the job's soft limit) and the ceiling (its hard limit, the furthest
/// the supervisor may ever move the cap). The cap is the others plus the job's own tasks
/// plus the allowance; the job's first process is one of its `max_processes`.
///
/// The ceiling keeps the last eighth of the user's limit out of the job's reach. A user
/// who already runs that close to the limit gets a job that may take half of what is
/// free, so the other half stays for the user's other programs.
fn user_total_cap(others: u64, max_processes: u64, soft: u64) -> (u64, u64) {
    let wanted = others
        .saturating_add(max_processes)
        .saturating_add(OTHER_PROGRAMS_ALLOWANCE);
    if soft == libc::RLIM_INFINITY {
        return (wanted, soft);
    }
    let reserved = soft - (soft / 8).max(1).min(soft);
    let half_of_free = others.saturating_add(soft.saturating_sub(others) / 2);
    let ceiling = reserved.max(half_of_free).min(soft);
    (wanted.min(ceiling), ceiling)
}

/// Caps the calling process and everything it will fork: the user's task count can no
/// longer be pushed beyond what the user's other programs run now plus `max_processes`
/// plus [`OTHER_PROGRAMS_ALLOWANCE`] (the soft limit), and never into the part of the
/// user's own limit that [`user_total_cap`] keeps free (the hard limit).
///
/// Call it with capabilities already dropped (a capability would exempt the measurement)
/// and before the program is executed. The hard limit cannot be raised again from
/// inside; the soft limit is the supervisor's to move below it, and
/// `seccomp::nproc_guard` keeps the job from moving it.
pub(crate) fn cap_user_total(max_processes: u64) -> Result<(), String> {
    let soft = limits().map_err(|e| format!("getrlimit: {e}"))?.rlim_cur as u64;
    let user_tasks = user_tasks(soft)?;
    let (cap, ceiling) = user_total_cap(user_tasks.saturating_sub(1), max_processes, soft);
    if cap <= user_tasks {
        return Err(format!(
            "this user runs {user_tasks} processes and threads of the {soft} it may have \
             (`ulimit -u`); too few are left to give a job a process allowance of its own"
        ));
    }
    if cap - user_tasks < max_processes.saturating_sub(1) {
        report(format_args!(
            "this user runs {user_tasks} of its {soft} allowed processes (`ulimit -u`): the \
             job can start only {} more, not the {max_processes} of its limit",
            cap - user_tasks
        ));
    }
    let limit = libc::rlimit {
        rlim_cur: cap as libc::rlim_t,
        rlim_max: ceiling as libc::rlim_t,
    };
    // SAFETY: passes a valid rlimit struct. Lowering the hard limit needs no privilege.
    if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &limit) } != 0 {
        return Err(format!("setrlimit: {}", io::Error::last_os_error()));
    }
    Ok(())
}

/// The soft limit the supervisor gives a job whose user now runs `others` tasks outside
/// it: the same sum as at the start, never above the job's hard limit.
pub(crate) fn recentred_cap(others: u64, max_processes: u64, hard: u64) -> u64 {
    others
        .saturating_add(max_processes)
        .saturating_add(OTHER_PROGRAMS_ALLOWANCE)
        .min(hard)
}

/// Whether a process whose soft limit is `soft` could fork now, tried for real: `false`
/// when the kernel refuses a fork under that limit. For the supervisor, which runs it in
/// a throwaway process; that process is itself one task of the user, hence `soft + 1`.
pub(crate) fn fork_possible_under(soft: u64) -> io::Result<bool> {
    fork_allowed_at(soft.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cap_is_the_users_other_tasks_plus_the_job_plus_the_allowance() {
        // The job's first process is one of its 64, so it is not among the 299.
        assert_eq!(
            user_total_cap(299, 64, 60_000),
            (299 + 64 + OTHER_PROGRAMS_ALLOWANCE, 52_500)
        );
        assert_eq!(
            user_total_cap(299, 64, libc::RLIM_INFINITY),
            (299 + 64 + OTHER_PROGRAMS_ALLOWANCE, libc::RLIM_INFINITY)
        );
        assert_eq!(
            recentred_cap(2000, 64, 52_500),
            2000 + 64 + OTHER_PROGRAMS_ALLOWANCE
        );
        assert_eq!(recentred_cap(60_000, 64, 52_500), 52_500);
    }

    #[test]
    fn an_eighth_of_the_users_own_limit_stays_free() {
        // 1000 allowed: the job can take the user to 875 at most.
        assert_eq!(user_total_cap(300, 512, 1000), (875, 875));
        assert_eq!(user_total_cap(0, 1, 1), (0, 0));
        assert_eq!(user_total_cap(0, 1, 0), (0, 0));
    }

    /// A user who already runs inside the last eighth still gets a job: it may take half
    /// of what is free. (3603 of 4096 is the case of a site-wide `soft nproc 4096`.)
    #[test]
    fn a_user_close_to_the_limit_shares_what_is_free_with_the_job() {
        assert_eq!(user_total_cap(3603, 64, 4096), (3849, 3849));
        assert_eq!(user_total_cap(900, 64, 1000), (950, 950));
        // Nothing free, nothing for the job.
        assert_eq!(user_total_cap(1000, 64, 1000), (1000, 1000));
        assert_eq!(user_total_cap(1200, 64, 1000), (1000, 1000));
    }

    /// The decision the launcher takes on this machine agrees with the kernel's rule:
    /// never for root of the initial user namespace, always for anyone else.
    #[test]
    fn enforcement_is_detected() {
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        let initial_namespace = std::fs::read_to_string("/proc/self/uid_map")
            .is_ok_and(|map| map.split_ascii_whitespace().eq(["0", "0", "4294967295"]));
        if uid != 0 {
            assert_eq!(enforced(), Enforcement::Applies);
        } else if initial_namespace {
            assert_eq!(enforced(), Enforcement::Exempt);
        }
    }
}
