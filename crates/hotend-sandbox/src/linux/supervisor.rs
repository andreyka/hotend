//! The launcher's side while the program runs: wall clock, tree-wide limits, the size of
//! the job folder and cleanup.
//!
//! The process tree is addressed through its process group. The child starts a new session
//! and the seccomp filter denies `setsid` and `setpgid`, so no descendant can leave the
//! group, and `kill(-pgid, SIGKILL)` reaches all of them. The kernel does not let a `fork`
//! complete while that signal is pending, so a fork bomb cannot outrun it.
//!
//! The number of tasks is capped by the kernel before the supervisor ever looks
//! (`super::ProcessCap`). What the supervisor adds is the exact count and the verdict: a
//! job that reaches its process limit is killed and reported, instead of being left to
//! run into refused forks. It looks often without costing much: every tick it reads one
//! small file that tells whether anything forked (the task count of the job's cgroup, or
//! else the last process id the kernel handed out), and it walks `/proc` only on its
//! regular schedule or when that file shows a burst of new processes.
//!
//! Where the cap is the job's `RLIMIT_NPROC` on the total of its user
//! (`super::nproc::cap_user_total`), the supervisor also keeps that limit where it
//! belongs: once a second it counts what the user runs outside the job and sets the soft
//! limit of every process of the job to that count plus the job's allowance, so that a
//! long job does not lose the ability to fork because the user opened other programs,
//! and does not keep room that other programs have given back. See [`Recentring`].

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::time::{Duration, Instant};

use super::cgroup::JobCgroup;
use super::watchdog::Watchdog;
use super::{disk, nproc};
use crate::error::{EXIT_LIMIT, EXIT_WALL_CLOCK, report};
use crate::policy::Limits;
use crate::unix::{self, cvt};

/// How often the tree-wide limits are checked when nothing calls for more.
const CHECK_INTERVAL: Duration = Duration::from_millis(100);
/// The interval once the tree has used half of a limit, to shorten the overshoot. Also the
/// shortest time between two walks of `/proc`.
const CLOSE_CHECK_INTERVAL: Duration = Duration::from_millis(10);
/// How often the cheap sign of new processes is read.
const TICK: Duration = Duration::from_millis(10);
/// The same once nothing has forked for [`IDLE_AFTER`]: a job that only computes, or an
/// agent that waits for its model, wakes the launcher twenty times a second.
const IDLE_TICK: Duration = Duration::from_millis(50);
const IDLE_AFTER: Duration = Duration::from_secs(2);
/// Process ids handed out within one tick, anywhere on the machine, below which the
/// supervisor never recounts early. The number is the machine's, not the job's, so it is
/// only a floor: what counts as a burst is measured against the machine's own recent
/// rate ([`ForkSignal`]).
const FORK_BURST: u64 = 8;
/// A tick is a burst when it brought more than this many times the recent average.
const BURST_FACTOR: u64 = 3;
/// How often the cap on the user's total is moved along with the user's other programs.
const RECENTRE_INTERVAL: Duration = Duration::from_secs(1);

/// Tells a burst of forks from the rate at which the machine forks anyway.
///
/// The last process id is a signal of the whole machine: a build or a shell loop
/// elsewhere moves it hundreds of times a second for as long as it runs. Recounting on
/// every tick that brought [`FORK_BURST`] new ids would make each launcher walk `/proc`
/// all the time on such a machine. So a tick counts as a burst only when it brought
/// several times the running average of the ticks before it: steady activity raises the
/// average and stops triggering after a few ticks, a fork bomb that starts on top of it
/// still stands out.
#[derive(Debug, Default)]
struct ForkSignal {
    seen: Option<u64>,
    /// Sixteen times the running average of new process ids per tick.
    average: u64,
}

impl ForkSignal {
    /// Takes the last process id of this tick. Returns whether anything forked since the
    /// tick before, and whether that was a burst.
    fn tick(&mut self, last_pid: u64) -> (bool, bool) {
        let new = match self.seen.replace(last_pid) {
            None => 0,
            Some(seen) if last_pid >= seen => (last_pid - seen).min(1 << 20),
            // The ids wrapped around: something forked, how much is unknown.
            Some(_) => 1,
        };
        let burst = new >= FORK_BURST && new * 16 > self.average * BURST_FACTOR;
        self.average = self.average - self.average / 8 + new * 2;
        (new != 0, burst)
    }
}

/// Where the process cap is the job's soft `RLIMIT_NPROC`: what the supervisor needs to
/// keep it at the user's other tasks plus the job's allowance.
///
/// The count of the user's other tasks is taken from `/proc` (every process of the user
/// that is not in the job's process group), not with the kernel's own check as at the
/// start: the job cannot change what `/proc` shows outside its group, while it could time
/// forks of its own against a measurement that counts them. What `/proc` does not show
/// here (processes of the same user id in other PID namespaces) is taken from the
/// difference at the start and assumed constant.
struct Recentring {
    uid: libc::uid_t,
    /// The job's hard limit: the furthest the cap may go.
    hard: u64,
    /// Tasks the kernel counted for the user at the start that `/proc` did not show.
    unseen: u64,
    next: Instant,
    /// The line about a job that cannot fork has been printed.
    reported: bool,
}

pub(crate) struct Supervisor {
    /// The first process of the tree; its pid is also the process group id.
    pid: libc::pid_t,
    pidfd: OwnedFd,
    launcher_parent: libc::pid_t,
    /// The job folder, whose contents are added up against the disk limit.
    job: OwnedFd,
    /// Handles on the policy's read-only paths, held open while the tree lives (`fs::pin`).
    _pins: Vec<OwnedFd>,
    watchdog: Watchdog,
    /// The job's cgroup, when that is what caps its processes.
    cgroup: Option<JobCgroup>,
    /// `/proc/loadavg`, read for the last process id when there is no cgroup to ask.
    loadavg: Option<File>,
    /// The user whose total the job's process cap is set on, where that is the cap.
    user_total: Option<libc::uid_t>,
}

/// Kills every process of the tree.
pub(crate) fn kill_tree(pid: libc::pid_t, cgroup: Option<&JobCgroup>) {
    // SAFETY: signals the child's process group and the child itself. While the child is
    // unreaped its pid cannot be reused, so nothing else can be hit.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
    }
    if let Some(cgroup) = cgroup {
        cgroup.kill();
    }
}

/// A handle on the process `pid` that becomes readable when it exits.
pub(crate) fn pidfd_open(pid: libc::pid_t) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_open takes a pid and flags; the result is checked.
    let fd =
        cvt(unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0 as libc::c_uint) } as libc::c_int)?;
    // SAFETY: `fd` was just opened and is owned by nothing else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The last process id the kernel handed out, the fifth field of `/proc/loadavg`. It moves
/// whenever anything on the machine forks.
fn last_pid(loadavg: &File) -> Option<u64> {
    let mut buffer = [0u8; 128];
    let read = loadavg.read_at(&mut buffer, 0).ok()?;
    std::str::from_utf8(&buffer[..read])
        .ok()?
        .split_ascii_whitespace()
        .nth(4)?
        .parse()
        .ok()
}

impl Supervisor {
    pub fn new(
        pid: libc::pid_t,
        pidfd: OwnedFd,
        launcher_parent: libc::pid_t,
        job: OwnedFd,
        pins: Vec<OwnedFd>,
        watchdog: Watchdog,
        cgroup: Option<JobCgroup>,
    ) -> Self {
        let loadavg = match cgroup {
            Some(_) => None,
            None => File::open("/proc/loadavg").ok(),
        };
        Self {
            pid,
            pidfd,
            launcher_parent,
            job,
            _pins: pins,
            watchdog,
            cgroup,
            loadavg,
            user_total: None,
        }
    }

    /// For a job whose process cap is its `RLIMIT_NPROC` on the total of the user `uid`:
    /// makes the supervisor keep that cap in step with the user's other programs.
    pub fn following_the_user(mut self, uid: libc::uid_t) -> Self {
        self.user_total = Some(uid);
        self
    }

    /// Kills the tree and collects the first process. Returns its wait status.
    fn kill_and_reap(&self) -> libc::c_int {
        kill_tree(self.pid, self.cgroup.as_ref());
        self.reap()
    }

    /// Ends a job whose program never started: kills the tree and releases the watchdog.
    pub fn abandon(self) {
        self.kill_and_reap();
        self.finish();
    }

    /// The tree has been killed: removes its cgroup and releases the watchdog.
    fn finish(self) {
        if let Some(cgroup) = &self.cgroup
            && let Err(e) = cgroup.remove()
        {
            report(format_args!(
                "the job's cgroup {} could not be removed: {e}",
                cgroup.dir().display()
            ));
        }
        self.watchdog.disarm();
    }

    fn reap(&self) -> libc::c_int {
        let mut status = 0;
        loop {
            // SAFETY: waits for the launcher's own child.
            let result = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if result == self.pid || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
            {
                return status;
            }
        }
    }

    /// Waits up to `timeout` for the first process to exit, without reaping it.
    fn exited_within(&self, timeout: Duration) -> bool {
        let mut poll = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // Rounded up, so that a wait for less than a millisecond does not turn into a spin.
        let millis = timeout.as_micros().div_ceil(1000).min(i32::MAX as u128) as libc::c_int;
        // SAFETY: polls one valid descriptor. A signal interrupts the wait, which is wanted.
        unsafe { libc::poll(&mut poll, 1, millis) > 0 }
    }

    /// Watches the program until it ends or a limit is hit. Returns the launcher's exit code.
    /// The whole tree is gone when this returns.
    pub fn watch(self, limits: &Limits) -> i32 {
        let code = self.watch_tree(limits);
        self.finish();
        code
    }

    /// Reads the cap the job's first process gave itself and what `/proc` shows of the
    /// user now. `None` when the job is already gone.
    fn recentring(&self, uid: libc::uid_t, limits: &Limits) -> Option<Recentring> {
        let (soft, hard) = process_limit_of(self.pid).ok()?;
        let usage = tree_usage(self.pid, Some(uid));
        // Below its ceiling the cap is the kernel's count of the user's other tasks plus
        // the job's allowance, so the count can be read back from it.
        let counted = (soft < hard)
            .then(|| soft.saturating_sub(nproc::recentred_cap(0, limits.max_processes, u64::MAX)));
        Some(Recentring {
            uid,
            hard,
            unseen: counted.map_or(0, |counted| counted.saturating_sub(usage.others)),
            next: Instant::now() + RECENTRE_INTERVAL,
            reported: false,
        })
    }

    fn watch_tree(&self, limits: &Limits) -> i32 {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(limits.wall_seconds);
        let memory_cap = limits.memory_mb.saturating_mul(1024 * 1024);
        let disk_cap = limits.max_disk_mb.saturating_mul(1024 * 1024);
        let mut next_disk_check = started;
        // The next walk of /proc, and the earliest moment a burst of forks may bring it
        // forward to.
        let mut next_walk = started + CLOSE_CHECK_INTERVAL;
        let mut earliest_walk = started;
        let mut forks = ForkSignal::default();
        let mut seen_tasks = None;
        let mut last_fork = started;
        let mut recentring = self.user_total.and_then(|uid| self.recentring(uid, limits));
        loop {
            let now = Instant::now();
            let tick = if now.duration_since(last_fork) < IDLE_AFTER {
                TICK
            } else {
                IDLE_TICK
            };
            let wait = deadline
                .saturating_duration_since(now)
                .min(next_walk.saturating_duration_since(now))
                .min(tick);
            if self.exited_within(wait) {
                // Stragglers are killed while the zombie still reserves the group's id.
                kill_tree(self.pid, self.cgroup.as_ref());
                return unix::exit_code(self.reap());
            }
            if let Some(signal) = unix::termination_signal() {
                self.kill_and_reap();
                return 128 + signal;
            }
            // SAFETY: getppid cannot fail.
            if unsafe { libc::getppid() } != self.launcher_parent {
                self.kill_and_reap();
                return 128 + libc::SIGHUP;
            }
            let now = Instant::now();
            if now >= deadline {
                self.kill_and_reap();
                report(format_args!(
                    "wall-clock limit of {} s reached; the process tree was killed",
                    limits.wall_seconds
                ));
                return EXIT_WALL_CLOCK;
            }

            // Every tick: did anything fork? With a cgroup the answer is the exact count.
            let mut exceeded = None;
            if let Some(cgroup) = &self.cgroup {
                let tasks = cgroup.tasks();
                if tasks != seen_tasks {
                    seen_tasks = tasks;
                    last_fork = now;
                }
                if let Some(tasks) = tasks.filter(|tasks| *tasks >= limits.max_processes) {
                    exceeded = Some(process_limit(limits, tasks));
                }
            } else if let Some(pid) = self.loadavg.as_ref().and_then(last_pid) {
                let (forked, burst) = forks.tick(pid);
                if forked {
                    last_fork = now;
                }
                if burst {
                    next_walk = next_walk.min(earliest_walk);
                }
            }

            if exceeded.is_none() && now >= next_walk {
                let walk = Instant::now();
                let recentre = recentring.as_mut().filter(|state| walk >= state.next);
                let usage = tree_usage(self.pid, recentre.as_ref().map(|state| state.uid));
                let took = walk.elapsed();
                // Without a cgroup this is the count. With one, the cgroup's own count,
                // read above, is never smaller: every process of the group is in it.
                if usage.tasks >= limits.max_processes {
                    exceeded = Some(process_limit(limits, usage.tasks));
                } else if usage.resident_bytes >= memory_cap {
                    exceeded = Some(format!(
                        "memory limit of {} MB reached by the process tree",
                        limits.memory_mb
                    ));
                }
                if let (None, Some(state)) = (&exceeded, recentre) {
                    state.apply(self.pid, &usage, limits);
                    state.next = Instant::now() + RECENTRE_INTERVAL;
                }
                let close = usage.tasks.saturating_mul(2) >= limits.max_processes
                    || usage.resident_bytes.saturating_mul(2) >= memory_cap;
                let interval = if close {
                    CLOSE_CHECK_INTERVAL
                } else {
                    CHECK_INTERVAL
                };
                // A burst of forks brings the next walk forward, but never so far that
                // walking takes more than a tenth of the launcher's time, and a slow walk
                // (a loaded machine) never pushes the regular one back.
                let finished = Instant::now();
                earliest_walk = finished + CLOSE_CHECK_INTERVAL.max(took * 9).min(CHECK_INTERVAL);
                next_walk = finished + interval;
            }

            if exceeded.is_none() && Instant::now() >= next_disk_check {
                let started = Instant::now();
                let found = disk::usage(self.job.as_fd(), disk_cap);
                // A large folder is walked less often, so the walk never becomes the load.
                next_disk_check = Instant::now() + (started.elapsed() * 4).max(CHECK_INTERVAL);
                exceeded = match found {
                    disk::Usage::Within(_) => None,
                    disk::Usage::TooManyBytes => Some(format!(
                        "disk limit of {} MB reached in the job folder",
                        limits.max_disk_mb
                    )),
                    disk::Usage::TooManyEntries => Some(format!(
                        "disk limit reached: more than {} files in the job folder",
                        disk::MAX_ENTRIES
                    )),
                    disk::Usage::TooDeep => Some(
                        "disk limit reached: folders in the job folder are nested too deep"
                            .to_owned(),
                    ),
                };
            }
            if let Some(reason) = exceeded {
                self.kill_and_reap();
                report(format_args!("{reason}; the process tree was killed"));
                // A code of its own, so the caller can tell a limit from a crash.
                return EXIT_LIMIT;
            }
        }
    }
}

fn process_limit(limits: &Limits, tasks: u64) -> String {
    format!(
        "process limit of {} reached ({tasks} tasks)",
        limits.max_processes
    )
}

impl Recentring {
    /// Sets the soft limit of every process of the job to what the user runs outside
    /// the job now plus the job's allowance, and says so once if the job cannot fork
    /// even then.
    fn apply(&mut self, group: libc::pid_t, usage: &TreeUsage, limits: &Limits) {
        let others = usage.others.saturating_add(self.unseen);
        let cap = nproc::recentred_cap(others, limits.max_processes, self.hard);
        for member in &usage.members {
            // The process may have gone since the walk and its id may belong to another
            // process by now. Only a process that is in the job's group and carries the
            // job's hard limit is touched. One forked a moment ago from a parent that
            // still had the old value is put right by the next round.
            // SAFETY: getpgid only reads.
            if unsafe { libc::getpgid(*member) } == group
                && process_limit_of(*member).is_ok_and(|(_, hard)| hard == self.hard)
            {
                let _ = set_process_limit_of(*member, cap, self.hard);
            }
        }
        if !self.reported
            && usage.tasks.saturating_add(1) < limits.max_processes
            && !a_fork_would_work_under(cap)
        {
            self.reported = true;
            report(format_args!(
                "the job cannot start another process or thread at the moment, although \
                 it runs only {} of the {} it may: its user runs more processes than the \
                 launcher can make room for (the user is close to its own limit, `ulimit \
                 -u`, or runs processes the launcher cannot see, as in other containers \
                 under the same user id)",
                usage.tasks, limits.max_processes
            ));
        }
    }
}

/// Whether a process of the job, whose soft `RLIMIT_NPROC` is `cap`, could fork now.
/// Asked of the kernel in a throwaway process, because only the kernel knows its count.
fn a_fork_would_work_under(cap: u64) -> bool {
    // SAFETY: the child only makes system calls and exits; the parent collects it.
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            // The launcher itself cannot fork: the user is at its own limit.
            return io::Error::last_os_error().raw_os_error() != Some(libc::EAGAIN);
        }
        if pid == 0 {
            libc::_exit(match nproc::fork_possible_under(cap) {
                Ok(false) => 1,
                _ => 0,
            });
        }
        let mut status = 0;
        libc::waitpid(pid, &mut status, 0);
        !(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 1)
    }
}

/// The soft and hard `RLIMIT_NPROC` of the process `pid`.
fn process_limit_of(pid: libc::pid_t) -> io::Result<(u64, u64)> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: passes a valid rlimit struct for the kernel to fill.
    cvt(unsafe { libc::prlimit(pid, libc::RLIMIT_NPROC, std::ptr::null(), &mut limit) })?;
    Ok((limit.rlim_cur, limit.rlim_max))
}

/// Sets the soft `RLIMIT_NPROC` of the process `pid`, a process of the launcher's own
/// user. `hard` must be the hard limit it already has: only the soft limit moves.
fn set_process_limit_of(pid: libc::pid_t, soft: u64, hard: u64) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: soft.min(hard) as libc::rlim_t,
        rlim_max: hard as libc::rlim_t,
    };
    // SAFETY: passes a valid rlimit struct; the old value is not asked for.
    cvt(unsafe { libc::prlimit(pid, libc::RLIMIT_NPROC, &limit, std::ptr::null_mut()) }).map(drop)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct TreeUsage {
    /// Processes and threads.
    tasks: u64,
    resident_bytes: u64,
    /// The processes of the group. Filled only together with `others`.
    members: Vec<libc::pid_t>,
    /// Tasks of the user asked for that are not in the group.
    others: u64,
}

/// Adds up every process whose process group is `pgid`. With `others_of`, also lists
/// those processes and counts the tasks of that user outside the group.
fn tree_usage(pgid: libc::pid_t, others_of: Option<libc::uid_t>) -> TreeUsage {
    // SAFETY: sysconf has no preconditions.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(0) as u64;
    let mut usage = TreeUsage::default();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return usage;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{name}/stat")) else {
            continue;
        };
        let Some((group, threads, resident_pages)) = parse_stat(&stat) else {
            continue;
        };
        if group == pgid {
            usage.tasks += threads;
            usage.resident_bytes += resident_pages * page_size;
            if let (Some(_), Ok(member)) = (others_of, name.parse()) {
                usage.members.push(member);
            }
        } else if let Some(uid) = others_of
            && entry.metadata().is_ok_and(|meta| meta.uid() == uid)
        {
            usage.others += threads;
        }
    }
    usage
}

/// Extracts (process group, thread count, resident pages) from `/proc/<pid>/stat`.
fn parse_stat(stat: &str) -> Option<(libc::pid_t, u64, u64)> {
    // The command name may contain spaces and parentheses; fields are counted after the
    // last closing one. See proc_pid_stat(5): pgrp is field 5, num_threads 20, rss 24.
    let fields: Vec<&str> = stat[stat.rfind(')')? + 1..]
        .split_ascii_whitespace()
        .collect();
    let field = |number: usize| fields.get(number - 3).copied();
    Some((
        field(5)?.parse().ok()?,
        field(20)?.parse().ok()?,
        field(24)?.parse().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn stat_lines_with_awkward_command_names_are_parsed() {
        let line = "4242 (a) b) c) S 1 777 777 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 9 0 12345 \
                    1000000 250 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 1 0 0 0 0 0";
        assert_eq!(parse_stat(line), Some((777, 9, 250)));
        assert_eq!(parse_stat("garbage"), None);
        assert_eq!(parse_stat("1 (x) S 1"), None);
    }

    #[test]
    fn the_last_process_id_moves_when_something_forks() {
        let loadavg = File::open("/proc/loadavg").unwrap();
        let first = last_pid(&loadavg).unwrap();
        assert!(first > 0);
        let status = std::process::Command::new("/bin/true").status().unwrap();
        assert!(status.success());
        assert_ne!(last_pid(&loadavg), Some(first));
    }

    #[test]
    fn own_process_group_is_counted() {
        // SAFETY: getpgrp cannot fail.
        let usage = tree_usage(unsafe { libc::getpgrp() }, None);
        assert!(usage.tasks >= 1);
        assert!(usage.resident_bytes > 0);
        assert_eq!((usage.members.len(), usage.others), (0, 0));
    }

    #[test]
    fn the_users_tasks_outside_a_group_are_counted() {
        // A child in a process group of its own: it is the group, the test is outside.
        // SAFETY: the child only makes system calls; the parent kills and collects it.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0);
            if pid == 0 {
                libc::setpgid(0, 0);
                loop {
                    libc::pause();
                }
            }
            libc::setpgid(pid, pid);
            let usage = tree_usage(pid, Some(libc::getuid()));
            libc::kill(pid, libc::SIGKILL);
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);
            assert_eq!((usage.tasks, &usage.members), (1, &vec![pid]));
            // At least every thread of this test process.
            assert!(usage.others >= 1, "{usage:?}");
        }
    }

    #[test]
    fn a_process_can_read_and_move_its_own_soft_limit() {
        // SAFETY: getpid cannot fail.
        let own = unsafe { libc::getpid() };
        let (soft, hard) = process_limit_of(own).unwrap();
        assert!(soft <= hard);
        set_process_limit_of(own, soft, hard).unwrap();
        assert_eq!(process_limit_of(own).unwrap(), (soft, hard));
    }

    #[test]
    fn steady_forking_elsewhere_is_not_a_burst() {
        let mut signal = ForkSignal::default();
        let mut pid = 1000;
        assert_eq!(signal.tick(pid), (false, false));
        // A shell loop elsewhere: eight or nine new ids every tick, for five seconds.
        let mut bursts = 0;
        for tick in 0..500 {
            pid += 8 + tick % 2;
            let (forked, burst) = signal.tick(pid);
            assert!(forked);
            bursts += u32::from(burst);
        }
        assert!(bursts <= 4, "{bursts} bursts in steady activity");
        // Nothing for a tick is no burst either.
        assert_eq!(signal.tick(pid), (false, false));
        // A fork bomb on top of it stands out at once.
        pid += 400;
        assert_eq!(signal.tick(pid), (true, true));
    }

    #[test]
    fn a_burst_on_a_quiet_machine_is_seen_at_once() {
        let mut signal = ForkSignal::default();
        assert_eq!(signal.tick(500), (false, false));
        assert_eq!(signal.tick(503), (true, false));
        assert_eq!(signal.tick(503 + FORK_BURST), (true, true));
        // Ids that wrap around say that something forked, not how much.
        assert_eq!(signal.tick(301), (true, false));
    }
}
