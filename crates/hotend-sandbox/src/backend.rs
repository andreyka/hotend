//! The per-OS backend interface.

use std::ffi::{OsStr, OsString};

use crate::error::SandboxError;
use crate::policy::Policy;
use crate::probe::ProbeReport;

/// One operating system's way of confining a child.
///
/// `run` forks, so it must only be called from a single-threaded process. The launcher binary
/// is that process; applications use [`crate::command`] instead of calling a backend.
pub trait Backend {
    /// Short name used in the probe report.
    fn name(&self) -> &'static str;

    /// Which mechanisms this machine offers. Cheap, and safe to call repeatedly.
    fn probe(&self) -> ProbeReport;

    /// Starts `program` confined by `policy`, waits for it and returns the exit code to
    /// report. An error means the program was not started.
    fn run(&self, policy: &Policy, program: &OsStr, args: &[OsString])
    -> Result<i32, SandboxError>;
}

/// The backend for the operating system this binary was built for.
pub fn platform_backend() -> Box<dyn Backend> {
    #[cfg(target_os = "linux")]
    {
        Box::new(crate::linux::LinuxBackend)
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(crate::macos::SeatbeltBackend)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Box::new(Unsupported)
    }
}

/// Reason given on platforms without a backend. `None` where one exists.
pub(crate) fn unsupported_platform() -> Option<SandboxError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        None
    }
    #[cfg(target_os = "windows")]
    {
        Some(SandboxError::NotSupported {
            platform: "Windows",
            detail: "the planned backend (AppContainer or restricted token, plus a job object) \
                     is described in docs/notes/sandbox.md; run Hotend inside WSL2 until then",
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Some(SandboxError::NotSupported {
            platform: std::env::consts::OS,
            detail: "only Linux has a working backend",
        })
    }
}

/// Backend for platforms where nothing is implemented. Every job fails closed.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
struct Unsupported;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Backend for Unsupported {
    fn name(&self) -> &'static str {
        "unsupported"
    }

    fn probe(&self) -> ProbeReport {
        let reason = unsupported_platform()
            .map(|e| e.to_string())
            .unwrap_or_default();
        ProbeReport::unsupported(std::env::consts::OS, self.name(), reason)
    }

    fn run(
        &self,
        _policy: &Policy,
        _program: &OsStr,
        _args: &[OsString],
    ) -> Result<i32, SandboxError> {
        Err(unsupported_platform().unwrap_or(SandboxError::Unavailable(
            "no sandbox backend for this platform".into(),
        )))
    }
}

#[cfg(all(test, not(any(target_os = "linux", target_os = "macos"))))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::policy::Profile;

    #[test]
    fn every_job_fails_closed_without_a_backend() {
        let job = tempfile::tempdir().unwrap();
        let policy = Policy::new(Profile::Engine, job.path());
        let error = platform_backend()
            .run(&policy, OsStr::new("program"), &[])
            .unwrap_err();
        assert!(matches!(error, SandboxError::NotSupported { .. }));
        assert_eq!(error.exit_code(), 125);
        assert!(!platform_backend().probe().usable);
        let error = crate::command(&policy, "program", [""; 0]).unwrap_err();
        assert!(matches!(error, SandboxError::NotSupported { .. }));
    }
}
