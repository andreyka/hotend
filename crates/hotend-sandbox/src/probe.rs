//! The report `hotend-sandbox probe` prints and [`crate::probe`] returns.

use std::collections::BTreeMap;

/// Which confinement mechanisms this machine offers and which one enforces each guarantee.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProbeReport {
    /// `linux`, `macos`, `windows`, ...
    pub platform: String,
    /// The backend that would run a job, for example `landlock+seccomp`.
    pub backend: String,
    /// True when all five guarantees can be enforced. When false, `run` exits 125.
    pub usable: bool,
    /// Why the sandbox is not usable and what the user can do about it.
    pub reason: Option<String>,
    /// Kernel release, when known.
    pub kernel: Option<String>,
    /// Landlock ABI version of the running kernel; `None` when Landlock is absent or disabled.
    pub landlock_abi: Option<u32>,
    /// Seccomp filters can be installed.
    pub seccomp: bool,
    /// The optional layer (user, PID, network, IPC, UTS and mount namespaces) can be created.
    pub namespaces: bool,
    /// Why the namespace layer is not available.
    pub namespaces_detail: Option<String>,
    /// The launcher runs as user id 0. The kernel never applies a per-user process limit
    /// to root of the initial user namespace, so a job then needs a pids cgroup (see
    /// `process_cap`).
    pub root: bool,
    /// Which kernel mechanism caps the number of processes of a job.
    #[serde(default)]
    pub process_cap: ProcessCapReport,
    /// For `network`, `filesystem`, `privileges`, `resources` and `environment`: the
    /// mechanisms that enforce the guarantee on this machine, strongest first.
    pub guarantees: BTreeMap<String, Vec<String>>,
    /// Things a child can still do on this machine that a stricter kernel would stop.
    pub limitations: Vec<String>,
}

/// How the process cap (`max_processes`) is enforced on this machine.
///
/// The launcher never runs a job whose process count only its own polling would limit:
/// where the kernel offers no mechanism, `mechanism` is `none` and every `run` exits 125.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProcessCapReport {
    /// What a job gets by default (with the namespace layer, when the machine has it).
    pub default: ProcessCapMode,
    /// What a job gets without the namespace layer (`--no-namespaces`).
    pub without_namespaces: ProcessCapMode,
    /// The kernel applies `RLIMIT_NPROC` to this user, as tried with a real `fork`. Never
    /// true for user id 0 of the initial user namespace; true for "root" inside a user
    /// namespace that maps it to an ordinary user (a rootless container).
    pub rlimit_enforced: bool,
    /// Why the trial behind `rlimit_enforced` could not tell (a `fork` failed for another
    /// reason, such as a full process quota); `rlimit_enforced` is then false.
    #[serde(default)]
    pub rlimit_detail: Option<String>,
    /// The kind of cgroup hierarchy in which the launcher can create a pids cgroup per
    /// job, when it can.
    pub cgroup: Option<String>,
    /// The cgroup those job cgroups are created in: the launcher's own.
    pub cgroup_parent: Option<String>,
    /// Why no cgroup can be used.
    pub cgroup_detail: Option<String>,
}

/// The process cap of one mode.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProcessCapMode {
    /// `rlimit-in-user-namespace`, `cgroup-pids`, `rlimit-on-user-total` or `none`.
    pub mechanism: String,
    /// `kernel-exact`, `kernel-approximate` or `refused` (no job is started).
    pub enforcement: String,
    /// What the mechanism bounds, in words.
    pub bound: String,
}

impl Default for ProcessCapMode {
    fn default() -> Self {
        Self {
            mechanism: "none".into(),
            enforcement: "refused".into(),
            bound: String::new(),
        }
    }
}

impl ProbeReport {
    /// A report for a platform without a working backend.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn unsupported(platform: &str, backend: &str, reason: String) -> Self {
        Self {
            platform: platform.to_owned(),
            backend: backend.to_owned(),
            usable: false,
            reason: Some(reason),
            kernel: None,
            landlock_abi: None,
            seccomp: false,
            namespaces: false,
            namespaces_detail: None,
            root: false,
            process_cap: ProcessCapReport::default(),
            guarantees: BTreeMap::new(),
            limitations: Vec::new(),
        }
    }
}
