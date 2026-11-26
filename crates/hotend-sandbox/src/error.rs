//! The error type shared by the library and the launcher.

use std::path::PathBuf;

/// Exit code of the launcher when the sandbox could not be applied. The program was not started.
pub const EXIT_SANDBOX_FAILED: i32 = 125;
/// Exit code of the launcher when it killed the process tree at the wall-clock limit.
pub const EXIT_WALL_CLOCK: i32 = 124;
/// Exit code of the launcher when it killed the process tree at its process, memory or disk
/// limit; the reason is on stderr (`hotend-sandbox: ... limit ...`).
pub const EXIT_LIMIT: i32 = 123;
/// Exit code of the launcher when the program exists but could not be executed.
pub const EXIT_NOT_EXECUTABLE: i32 = 126;
/// Exit code of the launcher when the program was not found.
pub const EXIT_NOT_FOUND: i32 = 127;

/// Why a sandboxed command could not be prepared or started.
///
/// Every variant means the program did not run. Messages never contain environment values.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// This operating system has no backend yet.
    #[error("sandboxing is not supported on {platform} yet: {detail}")]
    NotSupported {
        platform: &'static str,
        detail: &'static str,
    },

    /// The machine lacks a mechanism a guarantee depends on. Nothing weaker is used instead.
    #[error("the sandbox cannot be applied on this machine: {0}")]
    Unavailable(String),

    /// The policy or the command line is wrong; fix the caller.
    #[error("invalid sandbox policy: {0}")]
    InvalidPolicy(String),

    /// The launcher binary is missing.
    #[error(
        "sandbox launcher not found at {path}; it belongs next to the running program (in a \
         development checkout: `cargo build -p hotend-sandbox`)"
    )]
    LauncherNotFound { path: PathBuf },

    /// A confinement step failed while the child was being set up.
    #[error("sandbox setup failed at step `{step}`: {message}")]
    Setup { step: String, message: String },

    /// The sandbox was applied but the program could not be executed.
    #[error("cannot start `{program}`: {message}")]
    Exec {
        program: String,
        message: String,
        not_found: bool,
    },

    /// `hotend-sandbox probe` did not produce a usable report.
    #[error("sandbox probe failed: {0}")]
    Probe(String),
}

impl SandboxError {
    /// The launcher exit code for this error (PROTOCOL.md section 7).
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Exec {
                not_found: true, ..
            } => EXIT_NOT_FOUND,
            Self::Exec { .. } => EXIT_NOT_EXECUTABLE,
            _ => EXIT_SANDBOX_FAILED,
        }
    }
}

/// Prints a launcher message to stderr. Unlike `eprintln!` it never panics: a closed stderr
/// must not change the launcher's exit code.
pub(crate) fn report(message: impl std::fmt::Display) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "hotend-sandbox: {message}");
}
