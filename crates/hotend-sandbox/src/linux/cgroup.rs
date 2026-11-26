//! A pids cgroup for one job: the process cap the kernel enforces exactly, in every mode.
//!
//! `pids.max` is checked inside `fork` and `clone` against the number of tasks in the
//! cgroup, so no task beyond the cap ever exists, whoever the user is and however fast the
//! program forks. Root needs this: the kernel never applies `RLIMIT_NPROC` to uid 0.
//!
//! The job's cgroup is always a child of the cgroup the launcher itself runs in, so every
//! limit an administrator put on the launcher keeps applying to the job and whatever kills
//! the launcher's cgroup kills the job with it. Three layouts allow that:
//!
//! - a cgroup v1 `pids` hierarchy the launcher may write to (v1 lets a cgroup hold both
//!   processes and child cgroups);
//! - cgroup v2 with the launcher in the root cgroup, which may hold processes and pass
//!   controllers on to its children at the same time; the launcher enables `pids` for
//!   the children itself where nobody has;
//! - cgroup v2 with the launcher in any other cgroup it may write to (a scope of a
//!   systemd user session, a session scope for root, a container's own cgroup): the
//!   job's cgroup is made a *threaded* cgroup. A cgroup that holds processes cannot pass
//!   domain controllers (memory, io) on to its children, but it can pass on the threaded
//!   ones, and `pids` is one (`kernel/cgroup/pids.c`: `.threaded = true`; the rule is in
//!   `cgroup_vet_subtree_control_enable`, and in "Threads" of the kernel's
//!   `cgroup-v2.rst`). The launcher's cgroup becomes a threaded domain for as long as a
//!   job runs in it and is put back afterwards ([`put_back`]).
//!
//! Nothing here is taken on trust from the layout: [`create`] moves a process into the
//! new cgroup and has it try a `fork` that the cap must refuse, and a cgroup that fails
//! that is not used ([`prove`]).
//!
//! The cgroup v2 layouts have never run on a hierarchy that carries the pids controller
//! (the machine this was written on has it on cgroup v1). What was tried there by hand
//! is the part without the controller: a threaded child in a cgroup that holds
//! processes, moving a process into it through `cgroup.procs`, and the lock.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Prefix of the job cgroups this launcher creates, followed by the launcher's process id.
const PREFIX: &str = "hotend-sandbox-";
/// How long [`JobCgroup::remove`] waits for killed processes to leave the cgroup.
const REMOVE_TIMEOUT: Duration = Duration::from_secs(2);
/// A cgroup left behind by a launcher that no longer exists is removed once it is this old.
const STALE_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Version {
    V1,
    V2,
}

/// How the job's cgroup sits in its hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// An ordinary cgroup: cgroup v1, or a child of the cgroup v2 root.
    Plain,
    /// A threaded cgroup inside the launcher's own cgroup, which holds processes
    /// (cgroup v2). The kernel offers no `cgroup.kill` and no readable `cgroup.procs`
    /// there; the tree is killed through its process group, as everywhere.
    Threaded,
}

/// The cgroup of one job. Created empty, with its cap set, before the job's first process
/// is forked.
pub(crate) struct JobCgroup {
    dir: PathBuf,
    version: Version,
    layout: Layout,
    /// `pids.current`, kept open: the supervisor reads it many times a second.
    current: File,
    /// cgroup v2: a shared lock on the launcher's cgroup, held while this cgroup exists,
    /// so that no other launcher takes the controller away from under it ([`put_back`]).
    lock: Option<OwnedFd>,
}

/// The launcher's own cgroup in one hierarchy that could carry the pids controller.
#[derive(Debug, PartialEq, Eq)]
struct Location {
    version: Version,
    dir: PathBuf,
}

/// Creates the cgroup for a job that may hold `max_tasks` tasks. `Err` says why this
/// machine offers none.
pub(crate) fn create(max_tasks: u64) -> Result<JobCgroup, String> {
    let membership = std::fs::read_to_string("/proc/self/cgroup")
        .map_err(|e| format!("cannot read /proc/self/cgroup: {e}"))?;
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|e| format!("cannot read /proc/self/mountinfo: {e}"))?;
    let locations = locate(&membership, &mounts);
    if locations.is_empty() {
        return Err("no cgroup hierarchy with the pids controller is mounted".into());
    }
    let mut reasons = Vec::new();
    for location in locations {
        match create_in(&location, max_tasks) {
            Ok(cgroup) => return Ok(cgroup),
            Err(reason) => reasons.push(reason),
        }
    }
    Err(reasons.join("; "))
}

fn create_in(location: &Location, max_tasks: u64) -> Result<JobCgroup, String> {
    let parent = &location.dir;
    let lock = match location.version {
        Version::V1 => None,
        Version::V2 => Some(lock(parent, libc::LOCK_SH)?),
    };
    remove_stale(parent);
    // SAFETY: getpid cannot fail.
    let launcher = unsafe { libc::getpid() };
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let dir = parent.join(format!("{PREFIX}{launcher}-{nonce:08x}"));
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(&dir)
        .map_err(|e| {
            format!(
                "cannot create a cgroup in {}: {}",
                parent.display(),
                explain(&e)
            )
        })?;
    let mut layout = Layout::Plain;
    let prepared = (|| {
        match location.version {
            Version::V1 if dir.join("pids.max").exists() => {}
            Version::V1 => return Err("the new cgroup has no pids.max".to_owned()),
            Version::V2 => pass_pids_on(parent, &dir, &mut layout)?,
        }
        prove(&dir)?;
        let cap = max_tasks.to_string();
        std::fs::write(dir.join("pids.max"), &cap)
            .map_err(|e| format!("cannot set pids.max in {}: {e}", dir.display()))?;
        if !std::fs::read_to_string(dir.join("pids.max")).is_ok_and(|set| set.trim() == cap) {
            return Err(format!(
                "pids.max in {} does not keep its value",
                dir.display()
            ));
        }
        File::open(dir.join("pids.current"))
            .map_err(|e| format!("cannot open pids.current in {}: {e}", dir.display()))
    })();
    match prepared {
        Ok(current) => Ok(JobCgroup {
            dir,
            version: location.version,
            layout,
            current,
            lock,
        }),
        Err(reason) => {
            let _ = std::fs::remove_dir(&dir);
            if layout == Layout::Threaded {
                put_back(parent, lock.as_ref());
            }
            Err(reason)
        }
    }
}

/// An I/O error on a cgroup file with what it usually means there.
fn explain(error: &io::Error) -> String {
    match error.raw_os_error() {
        Some(libc::EROFS) => format!(
            "{error} (the cgroup file system is mounted read-only, as it is in a container \
             by default)"
        ),
        Some(libc::EACCES | libc::EPERM) => {
            format!("{error} (that cgroup does not belong to the launcher's user)")
        }
        _ => error.to_string(),
    }
}

/// cgroup v2: makes `child`, just created in the launcher's own cgroup `parent`, carry
/// the pids controller. `layout` is set to [`Layout::Threaded`] as soon as `child` is a
/// threaded cgroup, also when a later step fails, so that the caller puts `parent` back.
fn pass_pids_on(parent: &Path, child: &Path, layout: &mut Layout) -> Result<(), String> {
    let kind = |cgroup: &Path| std::fs::read_to_string(cgroup.join("cgroup.type")).ok();
    let make_threaded = || {
        std::fs::write(child.join("cgroup.type"), "threaded").map_err(|e| {
            format!(
                "cannot make a threaded cgroup in {} ({}): the launcher's cgroup holds \
                 processes, so its children get the pids controller only as threaded \
                 cgroups, and it cannot have those while it has child cgroups with \
                 processes in them or passes memory, io or other domain controllers on",
                parent.display(),
                explain(&e)
            )
        })
    };
    if child.join("pids.max").exists() {
        // The parent passes the controller on already. In a cgroup that is a threaded
        // domain already, a new child cannot hold anything until it is threaded too.
        if kind(child).is_some_and(|kind| kind.trim() == "domain invalid") {
            make_threaded()?;
            *layout = Layout::Threaded;
        }
        return Ok(());
    }
    let available = std::fs::read_to_string(parent.join("cgroup.controllers")).unwrap_or_default();
    if !available
        .split_ascii_whitespace()
        .any(|name| name == "pids")
    {
        return Err(format!(
            "cgroup v2 has no pids controller to pass on in {} (its cgroup.controllers \
             lists `{}`): the cgroup above has not enabled it, or a cgroup v1 hierarchy \
             holds it",
            parent.display(),
            available.trim()
        ));
    }
    // Only the root cgroup has no type. It may hold processes and pass any controller
    // on; every other cgroup that holds processes passes on threaded controllers only,
    // to threaded children.
    let root = kind(parent).is_none();
    if !root {
        make_threaded()?;
        *layout = Layout::Threaded;
    }
    std::fs::write(parent.join("cgroup.subtree_control"), "+pids").map_err(|e| {
        format!(
            "cannot enable the pids controller for the children of {}: {}",
            parent.display(),
            explain(&e)
        )
    })?;
    if !child.join("pids.max").exists() {
        return Err(format!(
            "the pids controller was enabled for the children of {}, but the new cgroup \
             has no pids.max",
            parent.display()
        ));
    }
    Ok(())
}

/// Shows that the kernel enforces `pids.max` of the new, empty cgroup `dir` on a process
/// moved into it: with a cap of one task, a process placed there must be refused a
/// `fork`. This is also the trial of whether the launcher may move a process there.
fn prove(dir: &Path) -> Result<(), String> {
    std::fs::write(dir.join("pids.max"), "1")
        .map_err(|e| format!("cannot set pids.max in {}: {e}", dir.display()))?;
    let (gate_read, gate_write) = crate::unix::pipe().map_err(|e| format!("pipe: {e}"))?;
    // SAFETY: the child only makes system calls and exits; the parent collects it. The
    // launcher is single-threaded.
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return Err(format!("fork: {}", io::Error::last_os_error()));
        }
        if pid == 0 {
            // Waits until the parent has moved it, then tries the fork that must fail.
            let mut byte = [0u8; 1];
            if libc::read(gate_read.as_raw_fd(), byte.as_mut_ptr().cast(), 1) != 1 {
                libc::_exit(3);
            }
            let child = libc::fork();
            if child == 0 {
                libc::_exit(0);
            }
            if child > 0 {
                let mut status = 0;
                libc::waitpid(child, &mut status, 0);
                libc::_exit(1);
            }
            let refused = io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN);
            libc::_exit(if refused { 0 } else { 2 });
        }
        let moved = std::fs::write(dir.join("cgroup.procs"), pid.to_string());
        match &moved {
            Ok(()) => {
                libc::write(gate_write.as_raw_fd(), [1u8].as_ptr().cast(), 1);
            }
            Err(_) => {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let mut status = 0;
        libc::waitpid(pid, &mut status, 0);
        moved.map_err(|e| {
            format!(
                "cannot move a process into {}: {}",
                dir.display(),
                explain(&e)
            )
        })?;
        match libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status)) {
            Some(0) => Ok(()),
            Some(1) => Err(format!(
                "pids.max of {} did not stop a fork: the kernel does not enforce it there",
                dir.display()
            )),
            _ => Err(format!(
                "the trial of pids.max in {} failed (status {status})",
                dir.display()
            )),
        }
    }
}

/// Opens the cgroup directory `dir` and takes a lock on it (`flock`; `how` is
/// `LOCK_SH` or `LOCK_EX`). The launchers of one cgroup use it among themselves: shared
/// while a job's cgroup exists, exclusive to change what the cgroup passes on. Waits up
/// to a second for a launcher that is doing the latter.
fn lock(dir: &Path, how: libc::c_int) -> Result<OwnedFd, String> {
    let fd = OwnedFd::from(
        File::open(dir).map_err(|e| format!("cannot open {}: {}", dir.display(), explain(&e)))?,
    );
    let started = Instant::now();
    loop {
        // SAFETY: locks a descriptor this process owns.
        if unsafe { libc::flock(fd.as_raw_fd(), how | libc::LOCK_NB) } == 0 {
            return Ok(fd);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EWOULDBLOCK)
            || started.elapsed() >= Duration::from_secs(1)
        {
            return Err(format!("cannot lock {}: {error}", dir.display()));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Undoes what a threaded job cgroup needed of the launcher's own cgroup `parent`: once
/// no launcher has a job cgroup there and the cgroup has no children left, `pids` is
/// taken out of its `cgroup.subtree_control` again, and with that it is an ordinary
/// domain cgroup as before. `own` is this launcher's shared lock, given up first.
///
/// Best effort: a launcher that cannot do it leaves it to the next one. The controller is
/// taken away only under the exclusive lock, which no launcher with a job cgroup lets
/// through, because a job whose cgroup lost the controller would run without its cap.
fn put_back(parent: &Path, own: Option<&OwnedFd>) {
    if let Some(own) = own {
        // SAFETY: unlocks a descriptor; in the watchdog, which has closed it, this fails
        // and the launcher's death has released the lock already.
        unsafe { libc::flock(own.as_raw_fd(), libc::LOCK_UN) };
    }
    let Ok(dir) = File::open(parent) else {
        return;
    };
    // SAFETY: locks a descriptor this process owns; released when `dir` is dropped.
    if unsafe { libc::flock(dir.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return;
    }
    let children = std::fs::read_dir(parent).map(|entries| {
        entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .count()
    });
    if matches!(children, Ok(0)) {
        let _ = std::fs::write(parent.join("cgroup.subtree_control"), "-pids");
    }
}

/// Removes the cgroups of launchers that died without cleaning up (killed together with
/// their watchdog). Only an empty cgroup can be removed, so a job that still runs is never
/// touched; the age check keeps away from a cgroup another launcher has just created.
fn remove_stale(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|name| name.strip_prefix(PREFIX)) else {
            continue;
        };
        let Some(owner) = rest
            .split('-')
            .next()
            .and_then(|pid| pid.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        // SAFETY: signal 0 only asks whether the process exists.
        let owner_gone = unsafe { libc::kill(owner, 0) } != 0
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        let old = entry
            .metadata()
            .ok()
            .and_then(|meta| {
                SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(meta.mtime().max(0) as u64))
            })
            .and_then(|created| created.elapsed().ok())
            .is_some_and(|age| age >= STALE_AFTER);
        if owner_gone && old {
            let _ = std::fs::remove_dir(entry.path());
        }
    }
}

impl JobCgroup {
    /// Moves a process into the cgroup. Everything it forks afterwards starts there, and
    /// nothing in the sandbox can move out: the cgroup files are outside its Landlock rules.
    pub fn add(&self, pid: libc::pid_t) -> io::Result<()> {
        std::fs::write(self.dir.join("cgroup.procs"), pid.to_string())
    }

    /// Tasks in the cgroup as the kernel counts them for the cap: processes and threads,
    /// including exited ones nobody has collected yet.
    pub fn tasks(&self) -> Option<u64> {
        let mut buffer = [0u8; 32];
        let read = self.current.read_at(&mut buffer, 0).ok()?;
        std::str::from_utf8(&buffer[..read])
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Kills every process in the cgroup in one step, where the kernel can (cgroup v2,
    /// Linux 5.14, not in a threaded cgroup). The process-group kill of the supervisor
    /// reaches the same processes; this one does not depend on anything the processes did.
    pub fn kill(&self) {
        if self.version == Version::V2 && self.layout == Layout::Plain {
            let _ = std::fs::write(self.dir.join("cgroup.kill"), "1");
        }
    }

    /// Removes the cgroup once its processes are gone. A killed process leaves the cgroup
    /// a moment after the signal, so this waits for it, up to [`REMOVE_TIMEOUT`].
    pub fn remove(&self) -> io::Result<()> {
        let started = Instant::now();
        let mut pause = Duration::from_micros(200);
        let removed = loop {
            match std::fs::remove_dir(&self.dir) {
                Ok(()) => break Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => break Ok(()),
                Err(e) if started.elapsed() >= REMOVE_TIMEOUT => break Err(e),
                Err(_) => {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(Duration::from_millis(20));
                }
            }
        };
        if let (Layout::Threaded, Some(parent)) = (self.layout, self.dir.parent()) {
            put_back(parent, self.lock.as_ref());
        }
        removed
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// For the probe report: the kind of hierarchy and the cgroup the job cgroups are
    /// created in.
    fn describe(&self) -> (String, String) {
        let version = match (self.version, self.layout) {
            (Version::V1, _) => "cgroup v1 pids hierarchy",
            (Version::V2, Layout::Plain) => "cgroup v2",
            (Version::V2, Layout::Threaded) => {
                "cgroup v2, threaded cgroups inside the launcher's own"
            }
        };
        let parent = self.dir.parent().unwrap_or(&self.dir);
        (version.to_owned(), parent.display().to_string())
    }
}

/// Tries the whole mechanism once: creates a cgroup, which includes moving a throwaway
/// process into it and seeing its cap refuse a fork ([`prove`]), and removes it again.
/// `Ok` is the kind of hierarchy and the cgroup in which job cgroups are created, `Err`
/// says what the machine lacks.
pub(crate) fn probe() -> Result<(String, String), String> {
    let cgroup = create(1)?;
    let description = cgroup.describe();
    cgroup
        .remove()
        .map_err(|e| format!("cannot remove {}: {e}", cgroup.dir.display()))?;
    Ok(description)
}

/// The launcher's own cgroup directory in every mounted hierarchy that could carry the
/// pids controller, cgroup v1 first: while a v1 hierarchy holds the controller, v2 cannot.
fn locate(membership: &str, mountinfo: &str) -> Vec<Location> {
    let mut locations = Vec::new();
    for version in [Version::V1, Version::V2] {
        // /proc/self/cgroup: `<id>:<controllers>:<path>`; cgroup v2 is `0::<path>`.
        let Some(path) = membership.lines().find_map(|line| {
            let mut fields = line.splitn(3, ':');
            let (_id, controllers, path) = (fields.next()?, fields.next()?, fields.next()?);
            let wanted = match version {
                Version::V1 => controllers.split(',').any(|name| name == "pids"),
                Version::V2 => controllers.is_empty(),
            };
            wanted.then_some(path)
        }) else {
            continue;
        };
        for line in mountinfo.lines() {
            let Some((root, mount_point)) = cgroup_mount(line, version) else {
                continue;
            };
            // Inside a cgroup namespace, or under a bind mount of a subtree, the mount shows
            // only part of the hierarchy: the launcher's cgroup must lie inside that part.
            let Some(below) = Path::new(path).strip_prefix(&root).ok() else {
                continue;
            };
            locations.push(Location {
                version,
                dir: mount_point.join(below),
            });
            break;
        }
    }
    locations
}

/// The (root, mount point) of a `/proc/self/mountinfo` line if it is a cgroup mount of
/// `version` with the pids controller (every cgroup v2 mount qualifies; whether pids is
/// enabled there shows when a cgroup is created).
fn cgroup_mount(line: &str, version: Version) -> Option<(PathBuf, PathBuf)> {
    // `<id> <parent> <dev> <root> <mount point> <options> [optional...] - <type> <source> <super options>`
    let (before, after) = line.split_once(" - ")?;
    let mut before = before.split(' ');
    let root = before.nth(3)?;
    let mount_point = before.next()?;
    let mut after = after.split(' ');
    let fs_type = after.next()?;
    let super_options = after.nth(1).unwrap_or("");
    let matches = match version {
        Version::V1 => fs_type == "cgroup" && super_options.split(',').any(|o| o == "pids"),
        Version::V2 => fs_type == "cgroup2",
    };
    matches.then(|| (unescape(root), unescape(mount_point)))
}

/// Undoes the octal escapes (`\040` for a space, ...) the kernel uses in mountinfo paths.
fn unescape(field: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let octal = bytes.get(index + 1..index + 4).filter(|digits| {
            bytes[index] == b'\\' && digits.iter().all(|d| (b'0'..=b'7').contains(d))
        });
        match octal {
            Some(digits) => {
                let value = digits
                    .iter()
                    .fold(0u32, |value, d| value * 8 + u32::from(d - b'0'));
                out.push(value as u8);
                index += 4;
            }
            None => {
                out.push(bytes[index]);
                index += 1;
            }
        }
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// Reads a small file to its end.
#[cfg(test)]
fn read_small(path: &Path) -> io::Result<String> {
    use std::io::Read;
    let mut text = String::new();
    File::open(path)?.read_to_string(&mut text)?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const HYBRID_MOUNTS: &str = "\
24 1 254:0 / / rw,relatime - ext4 /dev/root rw
31 24 0:26 / /sys/fs/cgroup rw,relatime - tmpfs tmpfs rw,mode=755
39 31 0:34 / /sys/fs/cgroup/pids rw,relatime - cgroup cgroup rw,pids
38 31 0:33 / /sys/fs/cgroup/memory rw,relatime - cgroup cgroup rw,memory
41 31 0:36 / /sys/fs/cgroup/unified rw,relatime - cgroup2 cgroup2 rw";
    const HYBRID_MEMBERSHIP: &str = "\
9:name=systemd:/
8:pids:/process_api/shell
4:memory:/process_api/job
0::/";

    #[test]
    fn the_launchers_cgroup_is_found_in_a_hybrid_layout() {
        assert_eq!(
            locate(HYBRID_MEMBERSHIP, HYBRID_MOUNTS),
            vec![
                Location {
                    version: Version::V1,
                    dir: "/sys/fs/cgroup/pids/process_api/shell".into()
                },
                Location {
                    version: Version::V2,
                    dir: "/sys/fs/cgroup/unified".into()
                },
            ]
        );
    }

    #[test]
    fn the_launchers_cgroup_is_found_in_a_unified_layout() {
        let mounts = "\
24 1 254:0 / / rw,relatime shared:1 - ext4 /dev/root rw
35 24 0:30 / /sys/fs/cgroup rw,nosuid,nodev,noexec shared:9 - cgroup2 cgroup2 rw,nsdelegate,memory_recursiveprot";
        let membership = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/x.scope\n";
        assert_eq!(
            locate(membership, mounts),
            vec![Location {
                version: Version::V2,
                dir:
                    "/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/x.scope"
                        .into()
            }]
        );
    }

    #[test]
    fn a_mount_that_shows_another_part_of_the_hierarchy_is_not_used() {
        // A container's bind mount of its own subtree: the mount root is not `/`.
        let mounts = "35 24 0:30 /docker/abc /sys/fs/cgroup rw - cgroup2 cgroup2 rw";
        assert_eq!(
            locate("0::/docker/abc/job\n", mounts),
            vec![Location {
                version: Version::V2,
                dir: "/sys/fs/cgroup/job".into()
            }]
        );
        assert_eq!(locate("0::/elsewhere\n", mounts), vec![]);
        assert_eq!(
            locate("0::/\n", "24 1 254:0 / / rw - ext4 /dev/root rw"),
            vec![]
        );
        assert_eq!(locate("garbage", "garbage"), vec![]);
    }

    #[test]
    fn escaped_mount_points_are_decoded() {
        assert_eq!(
            unescape(r"/mnt/with\040space"),
            PathBuf::from("/mnt/with space")
        );
        assert_eq!(unescape(r"/plain\x"), PathBuf::from(r"/plain\x"));
        assert_eq!(unescape(r"tail\04"), PathBuf::from(r"tail\04"));
    }

    /// Where this machine lets the test create cgroups, the cap is enforced by the kernel
    /// at exactly the number given, and an emptied cgroup can be removed.
    #[test]
    fn a_job_cgroup_caps_the_tasks_of_the_processes_moved_into_it() {
        let cgroup = match create(4) {
            Ok(cgroup) => cgroup,
            Err(reason) => {
                eprintln!("SKIPPED: no usable pids cgroup here: {reason}");
                return;
            }
        };
        assert_eq!(cgroup.tasks(), Some(0));
        assert_eq!(
            read_small(&cgroup.dir.join("pids.max")).unwrap().trim(),
            "4"
        );
        // SAFETY: the forked child only makes system calls and exits; the parent waits for
        // exactly that child.
        let created = unsafe {
            let (mut gate, mut report) = ([0; 2], [0; 2]);
            assert_eq!(libc::pipe(gate.as_mut_ptr()), 0);
            assert_eq!(libc::pipe(report.as_mut_ptr()), 0);
            let pid = libc::fork();
            assert!(pid >= 0);
            if pid == 0 {
                // Waits until the parent has moved it, then forks until the kernel refuses.
                let mut byte = [0u8; 1];
                libc::read(gate[0], byte.as_mut_ptr().cast(), 1);
                let mut created = 0u8;
                loop {
                    let child = libc::fork();
                    if child < 0 {
                        break;
                    }
                    if child == 0 {
                        libc::pause();
                        libc::_exit(0);
                    }
                    created += 1;
                    if created > 32 {
                        break;
                    }
                }
                libc::write(report[1], [created].as_ptr().cast(), 1);
                libc::pause();
                libc::_exit(0);
            }
            cgroup.add(pid).unwrap();
            libc::write(gate[1], [1u8].as_ptr().cast(), 1);
            let mut created = [0u8; 1];
            assert_eq!(libc::read(report[0], created.as_mut_ptr().cast(), 1), 1);
            let tasks = cgroup.tasks();
            // The children are in the forking child's process group only if the test
            // harness is; kill them through the cgroup's member list instead (a threaded
            // cgroup lists threads only, and these processes have one each).
            let members = match cgroup.layout {
                Layout::Plain => "cgroup.procs",
                Layout::Threaded => "cgroup.threads",
            };
            for member in read_small(&cgroup.dir.join(members)).unwrap().lines() {
                libc::kill(member.parse().unwrap(), libc::SIGKILL);
            }
            let mut status = 0;
            assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
            for fd in gate.into_iter().chain(report) {
                libc::close(fd);
            }
            (created[0], tasks)
        };
        // The first process plus three more fill a cap of four.
        assert_eq!(created, (3, Some(4)));
        cgroup.remove().unwrap();
        assert!(!cgroup.dir.exists());
    }
}
