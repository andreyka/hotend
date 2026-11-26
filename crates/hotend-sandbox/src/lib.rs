//! Confinement for the child processes Hotend starts: the CAD engine that runs AI-written
//! Python, the agent process and vendor slicers.
//!
//! # What a sandboxed child gets
//!
//! 1. No network: it cannot create or use a socket of any family. It talks to its parent over
//!    the stdio pipes it inherits and nothing else.
//! 2. Filesystem: it writes only inside its job folder and reads only the job folder, the
//!    read-only paths of the policy and the system paths a dynamically linked program needs.
//! 3. No way up: `no_new_privs`, no capabilities, no ptrace, mounts, namespaces, BPF, keyrings
//!    or kernel modules.
//! 4. Resource caps: memory, CPU time, process count, file size, open files and a wall-clock
//!    limit that kills the whole process tree. The process count is capped by the kernel,
//!    never by the launcher's polling alone.
//! 5. A clean environment: only allowlisted variables, with `HOME` and `TMPDIR` inside the
//!    job folder.
//!
//! If any of these cannot be enforced the launcher starts nothing and exits 125.
//!
//! # How to use it
//!
//! ```no_run
//! use hotend_sandbox::{Policy, Profile, command};
//!
//! let policy = Policy::new(Profile::Engine, "/data/jobs/0190-example")
//!     .read_only("/opt/hotend/python/.venv")
//!     .read_only("/opt/hotend/python-base");
//! let status = command(&policy, "/opt/hotend/python/.venv/bin/python", ["-I", "-m", "hotend_engine.worker"])?
//!     .status()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! [`command`] returns a `std::process::Command` that runs the `hotend-sandbox` launcher
//! binary; the launcher applies the sandbox to itself and then executes the program.
//! [`probe`] reports which mechanisms the machine offers.
//!
//! # Platforms
//!
//! Linux is the only working backend: Landlock (ABI 3 or newer) and seccomp are required,
//! namespaces are used as an extra layer when available. The macOS Seatbelt backend is
//! written but has never run on a Mac and refuses to start unless explicitly enabled.
//! Windows returns [`SandboxError::NotSupported`]. `docs/notes/sandbox.md` has the details.

mod backend;
#[doc(hidden)]
pub mod cli;
mod command;
pub mod error;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod policy;
mod probe;
pub mod seatbelt;
#[cfg(unix)]
mod unix;

pub use backend::{Backend, platform_backend};
pub use command::{
    CREATE_NO_WINDOW, Confinement, LAUNCHER_ENV, MACOS_OPT_IN_ENV, UNSAFE_ENV, command,
    confinement, no_console_window, on_9p, probe,
};
pub use error::SandboxError;
pub use policy::{Limits, Policy, Profile, SANDBOX_DIR};
pub use probe::{ProbeReport, ProcessCapMode, ProcessCapReport};
