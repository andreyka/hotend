//! What a sandboxed child may do: profile, job folder, read-only paths, limits, environment.
//!
//! A [`Policy`] is plain data. The launcher re-validates everything it receives, so a policy
//! built by a buggy caller fails closed instead of running with less confinement.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::error::SandboxError;

/// Folder inside the job folder that holds the child's `HOME` and `TMPDIR`.
pub const SANDBOX_DIR: &str = ".sandbox";

/// `PATH` given to every child. Programs are normally passed as absolute paths.
pub(crate) const CHILD_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Variables the launcher always sets itself; they cannot be allowlisted.
const RESERVED_ENV: [&str; 5] = ["HOME", "TMPDIR", "TMP", "TEMP", "PATH"];

/// Substrings that mark a variable name as a credential. Such names are refused even when
/// allowlisted: no child of Hotend is ever supposed to hold a key (IMPLEMENTATION_BRIEF 3).
const CREDENTIAL_MARKERS: [&str; 7] = [
    "KEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "ACCESS_CODE",
];

/// Which kind of child is being confined. All profiles give the same guarantees. They differ
/// in their default limits, and the agent may create anonymous socket pairs (see
/// [`Profile::allows_socketpair`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// The CAD worker that runs AI-written model code.
    Engine,
    /// The agent loop. Produces no files; talks to the core over stdio only.
    Agent,
    /// A vendor slicer command line.
    Slicer,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Engine => "engine",
            Self::Agent => "agent",
            Self::Slicer => "slicer",
        }
    }

    /// Whether the child may call `socketpair(AF_UNIX, SOCK_STREAM)` and `send`/`recv` on
    /// the result. Only the agent may: every Python event loop (asyncio, and trio the same)
    /// wakes itself through such a pair and cannot start without one. The pair is anonymous
    /// and connected to itself; it has no address, cannot be bound or connected to anything
    /// and cannot carry file descriptors, so it reaches nothing outside the process tree.
    pub fn allows_socketpair(self) -> bool {
        self == Self::Agent
    }

    /// Default limits of this profile.
    pub fn default_limits(self) -> Limits {
        match self {
            Self::Engine => Limits {
                wall_seconds: 120,
                cpu_seconds: 480,
                memory_mb: 4096,
                max_processes: 512,
                max_file_mb: 1024,
                max_disk_mb: 2048,
                max_open_files: 1024,
            },
            Self::Agent => Limits {
                wall_seconds: 3600,
                cpu_seconds: 3600,
                memory_mb: 2048,
                max_processes: 128,
                max_file_mb: 64,
                max_disk_mb: 256,
                max_open_files: 256,
            },
            Self::Slicer => Limits {
                wall_seconds: 900,
                cpu_seconds: 3600,
                memory_mb: 8192,
                max_processes: 512,
                max_file_mb: 4096,
                max_disk_mb: 8192,
                max_open_files: 1024,
            },
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Profile {
    type Err = SandboxError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "engine" => Ok(Self::Engine),
            "agent" => Ok(Self::Agent),
            "slicer" => Ok(Self::Slicer),
            other => Err(SandboxError::InvalidPolicy(format!(
                "unknown profile `{other}`; expected engine, agent or slicer"
            ))),
        }
    }
}

/// Resource caps of one sandboxed job. Every field must be at least 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Wall-clock seconds before the launcher kills the whole process tree (exit code 124).
    pub wall_seconds: u64,
    /// CPU seconds per process before the kernel kills it.
    pub cpu_seconds: u64,
    /// Cap on the address space of each process and on the resident memory of the whole tree.
    pub memory_mb: u64,
    /// Cap on the number of tasks (processes and threads) in the tree. The kernel enforces
    /// it (how, and how exactly, is in the probe's `process_cap`), and the launcher kills
    /// a tree that reaches it. A machine where the kernel cannot enforce it runs no job.
    pub max_processes: u64,
    /// Largest file a process may write.
    pub max_file_mb: u64,
    /// Cap on everything in the job folder together, counted by the launcher. The job
    /// folder may also hold at most 100 000 files and folders, nested at most 64 deep.
    pub max_disk_mb: u64,
    /// Open file descriptors per process.
    pub max_open_files: u64,
}

impl Limits {
    fn validate(&self) -> Result<(), SandboxError> {
        let fields = [
            ("wall-seconds", self.wall_seconds, 7 * 24 * 3600),
            ("cpu-seconds", self.cpu_seconds, 7 * 24 * 3600),
            ("memory-mb", self.memory_mb, 1 << 22),
            ("max-processes", self.max_processes, 1 << 20),
            ("max-file-mb", self.max_file_mb, 1 << 22),
            ("max-disk-mb", self.max_disk_mb, 1 << 24),
            ("max-open-files", self.max_open_files, 1 << 20),
        ];
        for (name, value, max) in fields {
            if value == 0 || value > max {
                return Err(SandboxError::InvalidPolicy(format!(
                    "limit {name} must be between 1 and {max}, got {value}"
                )));
            }
        }
        Ok(())
    }
}

/// Everything the launcher needs to confine one child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub profile: Profile,
    /// The only writable location. Also the child's working directory.
    pub job_dir: PathBuf,
    /// Extra paths the child may read and execute, for example the Python environment.
    pub read_only: Vec<PathBuf>,
    pub limits: Limits,
    /// Names of variables copied from the caller's environment. Everything else is dropped.
    pub env_allowlist: Vec<String>,
    /// Use the namespace layer when the kernel permits it (default). Every guarantee holds
    /// without it, but two of them then rest on one mechanism instead of two, the process
    /// cap of an ordinary user becomes a bound on the user's total instead of an exact
    /// count unless a cgroup carries it, and where the kernel also lacks Landlock's signal
    /// scoping (ABI 6) a subprocess of the program can no longer signal itself or a single
    /// other process (see the probe's `process_cap` and `limitations`). Turning it off
    /// exists so the mandatory layers can be tested alone.
    pub namespaces: bool,
}

impl Policy {
    /// A policy with the profile's default limits, no extra read-only paths and an empty
    /// environment allowlist.
    pub fn new(profile: Profile, job_dir: impl Into<PathBuf>) -> Self {
        Self {
            profile,
            job_dir: job_dir.into(),
            read_only: Vec::new(),
            limits: profile.default_limits(),
            env_allowlist: Vec::new(),
            namespaces: true,
        }
    }

    /// Adds a path the child may read and execute.
    pub fn read_only(mut self, path: impl Into<PathBuf>) -> Self {
        self.read_only.push(path.into());
        self
    }

    /// Allows one environment variable through to the child.
    pub fn allow_env(mut self, name: impl Into<String>) -> Self {
        self.env_allowlist.push(name.into());
        self
    }

    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Checks everything that can be checked without touching the kernel.
    pub fn validate(&self) -> Result<(), SandboxError> {
        self.limits.validate()?;
        check_dir("job folder", &self.job_dir)?;
        if self.job_dir.parent().is_none() {
            return Err(SandboxError::InvalidPolicy(
                "the job folder cannot be the filesystem root".into(),
            ));
        }
        for path in &self.read_only {
            if !path.is_absolute() {
                return Err(SandboxError::InvalidPolicy(format!(
                    "read-only path {} must be absolute",
                    path.display()
                )));
            }
            if !path.exists() {
                return Err(SandboxError::InvalidPolicy(format!(
                    "read-only path {} does not exist",
                    path.display()
                )));
            }
        }
        for name in &self.env_allowlist {
            check_env_name(name)?;
        }
        Ok(())
    }

    /// The `hotend-sandbox run` arguments that reproduce this policy, up to and including `--`.
    pub(crate) fn launcher_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["run".into(), "--profile".into()];
        args.push(self.profile.as_str().into());
        args.push("--job".into());
        args.push(self.job_dir.clone().into());
        for path in &self.read_only {
            args.push("--ro".into());
            args.push(path.clone().into());
        }
        for name in &self.env_allowlist {
            args.push("--env".into());
            args.push(name.into());
        }
        let limits = [
            ("--wall-seconds", self.limits.wall_seconds),
            ("--cpu-seconds", self.limits.cpu_seconds),
            ("--memory-mb", self.limits.memory_mb),
            ("--max-processes", self.limits.max_processes),
            ("--max-file-mb", self.limits.max_file_mb),
            ("--max-disk-mb", self.limits.max_disk_mb),
            ("--max-open-files", self.limits.max_open_files),
        ];
        for (flag, value) in limits {
            args.push(flag.into());
            args.push(value.to_string().into());
        }
        if !self.namespaces {
            args.push("--no-namespaces".into());
        }
        args.push("--".into());
        args
    }

    /// The complete environment of the child: fixed variables plus the allowlisted ones that
    /// `lookup` knows. `job_dir` must be the path the child will see.
    pub(crate) fn child_env(
        &self,
        job_dir: &Path,
        lookup: impl Fn(&str) -> Option<OsString>,
    ) -> Vec<(OsString, OsString)> {
        let home = job_dir.join(SANDBOX_DIR).join("home");
        let tmp = job_dir.join(SANDBOX_DIR).join("tmp");
        let mut env: Vec<(OsString, OsString)> = vec![
            ("PATH".into(), CHILD_PATH.into()),
            ("HOME".into(), home.into()),
            ("TMPDIR".into(), tmp.clone().into()),
            ("TMP".into(), tmp.clone().into()),
            ("TEMP".into(), tmp.into()),
            ("LANG".into(), "C.UTF-8".into()),
        ];
        for name in &self.env_allowlist {
            if let Some(value) = lookup(name) {
                env.retain(|(existing, _)| existing != OsStr::new(name));
                env.push((name.into(), value));
            }
        }
        env
    }
}

fn check_dir(what: &str, path: &Path) -> Result<(), SandboxError> {
    if !path.is_absolute() {
        return Err(SandboxError::InvalidPolicy(format!(
            "{what} {} must be an absolute path",
            path.display()
        )));
    }
    if !path.is_dir() {
        return Err(SandboxError::InvalidPolicy(format!(
            "{what} {} is not an existing directory",
            path.display()
        )));
    }
    Ok(())
}

/// Accepts a name only if it is a plain identifier, not set by the launcher itself and does
/// not look like a credential.
pub(crate) fn check_env_name(name: &str) -> Result<(), SandboxError> {
    let mut chars = name.chars();
    let plain = match chars.next() {
        Some(first) => {
            (first.is_ascii_alphabetic() || first == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    };
    if !plain {
        return Err(SandboxError::InvalidPolicy(format!(
            "`{name}` is not a valid environment variable name"
        )));
    }
    if RESERVED_ENV.contains(&name) {
        return Err(SandboxError::InvalidPolicy(format!(
            "{name} is set by the sandbox and cannot be passed through"
        )));
    }
    let upper = name.to_ascii_uppercase();
    if CREDENTIAL_MARKERS.iter().any(|m| upper.contains(m)) {
        return Err(SandboxError::InvalidPolicy(format!(
            "{name} looks like a credential; sandboxed children never receive secrets"
        )));
    }
    Ok(())
}

/// Creates `<job>/.sandbox/{home,tmp}` without following symlinks a previous job may have
/// left behind, so the unconfined launcher never creates anything outside the job folder.
pub(crate) fn prepare_job_dirs(job_dir: &Path) -> Result<(), SandboxError> {
    let base = job_dir.join(SANDBOX_DIR);
    for dir in [base.clone(), base.join("home"), base.join("tmp")] {
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.file_type().is_dir() => {}
            Ok(_) => {
                return Err(SandboxError::InvalidPolicy(format!(
                    "{} exists and is not a directory",
                    dir.display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&dir).map_err(|e| SandboxError::Setup {
                    step: "create job subfolders".into(),
                    message: format!("{}: {e}", dir.display()),
                })?;
            }
            Err(e) => {
                return Err(SandboxError::Setup {
                    step: "create job subfolders".into(),
                    message: format!("{}: {e}", dir.display()),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn credential_like_names_are_refused() {
        for name in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "GITHUB_TOKEN",
            "aws_secret_access_key",
            "BAMBU_ACCESS_CODE",
            "DB_PASSWORD",
        ] {
            assert!(check_env_name(name).is_err(), "{name} must be refused");
        }
    }

    #[test]
    fn plain_names_pass_and_reserved_or_malformed_names_do_not() {
        for name in ["LC_ALL", "OPENBLAS_NUM_THREADS", "_X1"] {
            assert!(check_env_name(name).is_ok(), "{name} must pass");
        }
        for name in ["HOME", "TMPDIR", "PATH", "", "1ABC", "A=B", "A B", "É"] {
            assert!(check_env_name(name).is_err(), "{name:?} must be refused");
        }
    }

    #[test]
    fn child_env_contains_only_fixed_and_allowlisted_variables() {
        let policy = Policy::new(Profile::Engine, "/job").allow_env("LC_ALL");
        let env = policy.child_env(Path::new("/job"), |name| match name {
            "LC_ALL" => Some("C".into()),
            _ => Some("leak".into()),
        });
        let names: Vec<_> = env.iter().map(|(k, _)| k.to_string_lossy()).collect();
        assert_eq!(
            names,
            ["PATH", "HOME", "TMPDIR", "TMP", "TEMP", "LANG", "LC_ALL"]
        );
        let home = &env.iter().find(|(k, _)| k == "HOME").unwrap().1;
        assert_eq!(Path::new(home), Path::new("/job/.sandbox/home"));
    }

    #[test]
    fn launcher_args_round_trip_through_the_command_line_parser() {
        let dir = tempfile::tempdir().unwrap();
        let mut policy = Policy::new(Profile::Slicer, dir.path())
            .read_only(dir.path())
            .allow_env("LC_ALL");
        policy.namespaces = false;
        policy.limits.memory_mb = 123;
        let mut args = policy.launcher_args();
        args.push("prog".into());
        args.push("--job".into());
        let parsed = crate::cli::parse(args.into_iter().skip(1)).unwrap();
        assert_eq!(parsed.policy, policy);
        assert_eq!(parsed.program, OsString::from("prog"));
        assert_eq!(parsed.args, [OsString::from("--job")]);
    }

    #[test]
    fn zero_limits_and_relative_paths_are_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let mut policy = Policy::new(Profile::Engine, dir.path());
        assert!(policy.validate().is_ok());
        policy.limits.wall_seconds = 0;
        assert!(policy.validate().is_err());
        assert!(Policy::new(Profile::Engine, "relative").validate().is_err());
        let missing = Policy::new(Profile::Engine, dir.path()).read_only("/no/such/path");
        assert!(missing.validate().is_err());
    }

    #[test]
    fn job_subfolders_are_created_once() {
        let job = tempfile::tempdir().unwrap();
        prepare_job_dirs(job.path()).unwrap();
        prepare_job_dirs(job.path()).unwrap();
        assert!(job.path().join(SANDBOX_DIR).join("home").is_dir());
        assert!(job.path().join(SANDBOX_DIR).join("tmp").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn job_subfolders_are_not_created_through_a_symlink() {
        let job = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), job.path().join(SANDBOX_DIR)).unwrap();
        assert!(prepare_job_dirs(job.path()).is_err());
        assert!(!elsewhere.path().join("home").exists());
    }
}
