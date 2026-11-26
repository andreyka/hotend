//! The Landlock ruleset: which files a confined child may touch.
//!
//! Landlock decides on the real location of a file, after symlinks, `..` and
//! `/proc/self/root` have been resolved, so none of those lead out. It does not cover
//! everything: `stat`, `access` and `readlink` work on any path (existence and metadata of
//! files outside the sandbox are visible), and metadata changes are handled by the seccomp
//! filter instead.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access, AccessFs, AccessNet, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd,
    Ruleset, RulesetAttr, RulesetCreated, RulesetCreatedAttr, RulesetStatus, Scope,
};

/// Oldest Landlock ABI that can enforce the filesystem guarantee. ABI 3 (Linux 6.2) added
/// control over truncation; before it a child could empty any file its user may write.
pub(crate) const MIN_ABI: u32 = 3;

/// First Landlock ABI that confines signals to the sandbox (Linux 6.12).
pub(crate) const SIGNAL_SCOPE_ABI: u32 = 6;

/// Opens a handle on the job folder and on every read-only path of the policy, for the
/// launcher to hold until the process tree is gone. Returns (job folder, the others).
///
/// Landlock attaches a rule to an inode in memory. Some filesystems have no stable inodes:
/// 9p, as WSL2 uses it for Windows drives (`/mnt/c`, ...), creates a new inode each time a
/// name is looked up while nothing holds its directory entry, and that inode carries no
/// rule. A checkout on such a drive would lose read access to its own Python runtime as
/// soon as the kernel dropped the entry. An open handle keeps the entry and all its
/// ancestors in use, so every later lookup arrives at the inode that has the rule. The
/// handles are opened before the fork, so the child's own rules land on the same inodes.
pub(crate) fn pin(job: &Path, read_only: &[PathBuf]) -> std::io::Result<(OwnedFd, Vec<OwnedFd>)> {
    let open = |path: &Path| {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(path)
            .map(OwnedFd::from)
    };
    let job = open(job)?;
    let others = read_only
        .iter()
        .map(|path| open(path))
        .collect::<Result<_, _>>()?;
    Ok((job, others))
}

/// The `f_type` of a 9p mount (`V9FS_MAGIC`).
const V9FS_MAGIC: i64 = 0x0102_1997;

/// Whether `path` is on a 9p filesystem, where rules depend on the handles of [`pin`].
pub(crate) fn on_9p(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a NUL-terminated string that outlives the call, and `stat` is a
    // zeroed plain-data struct the kernel fills in; it is read only after a success.
    unsafe {
        let mut stat: libc::statfs = std::mem::zeroed();
        // The field's integer type differs between C libraries and word sizes.
        #[allow(clippy::unnecessary_cast, clippy::cast_lossless)]
        let is_9p = libc::statfs(path.as_ptr(), &mut stat) == 0 && stat.f_type as i64 == V9FS_MAGIC;
        is_9p
    }
}

/// Directories and files a dynamically linked program needs, read-only. Missing ones are
/// skipped. `/usr` is granted as a whole. From `/etc` only the loader's files and a few
/// files that say nothing about the user and that runtimes open without handling a refusal
/// (Python's `mimetypes` crashes on an unreadable `/etc/mime.types`). No account, host or
/// resolver files: name lookups must fail.
const SYSTEM_READ_ONLY: [&str; 15] = [
    "/usr",
    "/lib",
    "/lib64",
    "/lib32",
    "/bin",
    "/sbin",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    "/etc/alternatives",
    "/etc/localtime",
    "/etc/mime.types",
    "/etc/os-release",
    "/etc/ssl/openssl.cnf",
    "/sys/devices/system/cpu",
];

/// Global `/proc` files that runtimes read to size thread pools and allocators. The rest of
/// `/proc`, and with it every other process, stays out of reach.
const PROC_READ_ONLY: [&str; 5] = [
    "/proc/cpuinfo",
    "/proc/meminfo",
    "/proc/stat",
    "/proc/loadavg",
    "/proc/sys/vm/overcommit_memory",
];

const DEVICES_READ_ONLY: [&str; 3] = ["/dev/zero", "/dev/urandom", "/dev/random"];

/// Applies the ruleset to the calling thread and sets `no_new_privs`.
///
/// `proc_self` is an open handle on the caller's own `/proc/<pid>` directory. The rule is
/// attached to that inode; the caller keeps the handle open for the life of the process,
/// because the kernel would otherwise be free to recreate the inode without the rule.
pub(crate) fn apply(
    job: &Path,
    read_only: &[PathBuf],
    proc_self: Option<BorrowedFd<'_>>,
) -> Result<(), String> {
    // The rights are named by the ABI they were tested with, not taken from the running
    // kernel, so a newer kernel never changes behaviour silently.
    let required = AccessFs::from_all(ABI::V3);
    let read = AccessFs::ReadFile | AccessFs::ReadDir;
    let read_execute = read | AccessFs::Execute;
    // In the job folder: no execute, no device, socket or FIFO nodes, and no symlinks. A
    // symlink could not take the child anywhere, but one left among the job's outputs could
    // mislead whoever reads them later.
    let read_write = required
        & !(AccessFs::Execute
            | AccessFs::MakeChar
            | AccessFs::MakeBlock
            | AccessFs::MakeSock
            | AccessFs::MakeFifo
            | AccessFs::MakeSym);

    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(required)
        .map_err(text)?
        // Newer kernels also get: no ioctl on device files, no TCP bind or connect, no
        // signals or abstract-socket connections to processes outside the sandbox.
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(AccessFs::IoctlDev)
        .map_err(text)?
        .handle_access(AccessNet::from_all(ABI::V4))
        .map_err(text)?
        // `SIGNAL_SCOPE_ABI` names the ABI this line relies on.
        .scope(Scope::from_all(ABI::V6))
        .map_err(text)?
        .create()
        .map_err(text)?;

    ruleset = allow(ruleset, job, read_write, true)?;
    for path in read_only {
        ruleset = allow(ruleset, path, read_execute, true)?;
    }
    for path in SYSTEM_READ_ONLY {
        ruleset = allow(ruleset, Path::new(path), read_execute, false)?;
    }
    for path in PROC_READ_ONLY.iter().chain(&DEVICES_READ_ONLY) {
        ruleset = allow(ruleset, Path::new(path), read, false)?;
    }
    let null = AccessFs::ReadFile | AccessFs::WriteFile;
    ruleset = allow(ruleset, Path::new("/dev/null"), null, false)?;
    if let Some(fd) = proc_self {
        ruleset = ruleset.add_rule(PathBeneath::new(fd, read)).map_err(text)?;
    }

    let status = ruleset.restrict_self().map_err(text)?;
    if status.ruleset == RulesetStatus::NotEnforced {
        return Err("the kernel did not enforce the Landlock ruleset".into());
    }
    if !status.no_new_privs {
        return Err("no_new_privs was not set".into());
    }
    Ok(())
}

/// Adds one rule. Paths the policy names must exist; system paths are optional.
fn allow(
    ruleset: RulesetCreated,
    path: &Path,
    access: BitFlags<AccessFs>,
    required: bool,
) -> Result<RulesetCreated, String> {
    let fd = match PathFd::new(path) {
        Ok(fd) => fd,
        Err(_) if !required => return Ok(ruleset),
        Err(e) => return Err(format!("cannot open {}: {e}", path.display())),
    };
    // Directory-only rights on a file are an error for the kernel.
    let access = if is_dir(fd.as_fd()) {
        access
    } else {
        access & AccessFs::from_file(ABI::V3)
    };
    ruleset
        .add_rule(PathBeneath::new(fd, access))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn is_dir(fd: BorrowedFd<'_>) -> bool {
    std::fs::File::from(match fd.try_clone_to_owned() {
        Ok(owned) => owned,
        Err(_) => return false,
    })
    .metadata()
    .map(|m| m.is_dir())
    .unwrap_or(false)
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}
