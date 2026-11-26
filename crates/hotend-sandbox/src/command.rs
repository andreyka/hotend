//! Turns a [`Policy`] and a program into a ready-to-spawn command that runs the launcher.

use std::ffi::OsStr;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::backend::unsupported_platform;
use crate::error::SandboxError;
use crate::policy::{Policy, prepare_job_dirs};
use crate::probe::ProbeReport;

/// Overrides where the launcher binary is looked up. Honoured in debug builds only: a
/// release build runs nothing but the launcher that was installed beside it.
pub const LAUNCHER_ENV: &str = "HOTEND_SANDBOX_BIN";

/// `HOTEND_UNSAFE_NO_SANDBOX=1` runs children unconfined. Honoured in debug builds only.
pub const UNSAFE_ENV: &str = "HOTEND_UNSAFE_NO_SANDBOX";

/// `HOTEND_SANDBOX_MACOS_UNTESTED=1` lets the launcher use the macOS backend, which is
/// not tested. The launcher reads it; see [`launcher_switches`].
pub const MACOS_OPT_IN_ENV: &str = "HOTEND_SANDBOX_MACOS_UNTESTED";

/// The process creation flag `CREATE_NO_WINDOW`: a console program started by a windowed
/// application on Windows gets no console window of its own.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The variables of this process that the launcher on `os` reads for itself, besides
/// what the policy allowlists. They stop at the launcher: it builds the sandboxed
/// program's environment from the policy alone.
fn launcher_switches(os: &str) -> &'static [&'static str] {
    match os {
        "macos" => &[MACOS_OPT_IN_ENV],
        _ => &[],
    }
}

/// A command that starts the launcher at `launcher` with nothing in its environment but
/// its own switches for `os`, and without a console window of its own on Windows.
fn launcher_command(
    launcher: PathBuf,
    os: &str,
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Command {
    let mut command = Command::new(launcher);
    command.env_clear();
    for name in launcher_switches(os) {
        if let Some(value) = lookup(name) {
            command.env(name, value);
        }
    }
    no_console_window(&mut command);
    command
}

/// On Windows, keeps `command` from opening a console window ([`CREATE_NO_WINDOW`]).
/// Nothing elsewhere.
pub fn no_console_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

/// Builds the command that runs `program` with `args` inside the sandbox described by `policy`.
///
/// The returned command starts the `hotend-sandbox` launcher. Its environment is already
/// reduced to the allowlisted variables, so secrets held by the caller do not even reach the
/// launcher; to pass a fixed value, allowlist the name and call `.env(name, value)` on the
/// result. Standard streams are left for the caller to configure. For async use, convert with
/// `tokio::process::Command::from(command)`.
///
/// The launcher exits with the program's exit code, 128+N if a signal killed it, 124 at the
/// wall-clock limit and 125 if the sandbox could not be applied (see [`crate::error`]).
///
/// Errors here mean no command can be built: an invalid policy, a platform without a backend
/// or a missing launcher. Whether the kernel can enforce the policy is decided by the launcher
/// at start; use [`probe`] to find out beforehand.
pub fn command<P, I, A>(policy: &Policy, program: P, args: I) -> Result<Command, SandboxError>
where
    P: AsRef<OsStr>,
    I: IntoIterator<Item = A>,
    A: AsRef<OsStr>,
{
    let requested = unsafe_requested();
    command_inner(policy, program, args, requested, &mut std::io::stderr())
}

fn unsafe_requested() -> bool {
    std::env::var_os(UNSAFE_ENV).is_some_and(|v| v == "1")
}

/// How [`command`] starts a child in this process right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confinement {
    /// Through the launcher at this path.
    Sandboxed {
        launcher: PathBuf,
        /// The path came from `HOTEND_SANDBOX_BIN` (debug builds only), not from the
        /// folder of the running program.
        launcher_overridden: bool,
    },
    /// `HOTEND_UNSAFE_NO_SANDBOX=1` in a debug build: children run unconfined.
    Unconfined,
}

/// Whether `path` is on a filesystem without stable inodes: 9p, which WSL2 uses for
/// Windows drives (`/mnt/c`, ...). The sandbox's file rules hold there only because the
/// launcher keeps the granted folders open for as long as the job runs; callers use this
/// to tell the user that such a place is slower and less well tested than the Linux
/// filesystem. Always false where the sandbox is not the Linux one.
pub fn on_9p(path: &std::path::Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::linux::on_9p(path)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

/// Reports how [`command`] would start a child, so that callers can record it with every
/// job: a job that ran without the sandbox, or under a substituted launcher, must be
/// visible in more places than stderr.
pub fn confinement() -> Result<Confinement, SandboxError> {
    if unsafe_requested() && cfg!(debug_assertions) {
        return Ok(Confinement::Unconfined);
    }
    if let Some(error) = unsupported_platform() {
        return Err(error);
    }
    let overridden = launcher_override(cfg!(debug_assertions), &mut std::io::stderr());
    let launcher_overridden = overridden.is_some();
    let exe = std::env::current_exe().ok();
    Ok(Confinement::Sandboxed {
        launcher: resolve_launcher(overridden, exe.as_deref())?,
        launcher_overridden,
    })
}

fn command_inner<P, I, A>(
    policy: &Policy,
    program: P,
    args: I,
    unsafe_requested: bool,
    warnings: &mut dyn Write,
) -> Result<Command, SandboxError>
where
    P: AsRef<OsStr>,
    I: IntoIterator<Item = A>,
    A: AsRef<OsStr>,
{
    policy.validate()?;
    let program = program.as_ref();

    if unsafe_requested && cfg!(debug_assertions) {
        // Warnings are best effort: a closed stderr must not turn into a different behaviour.
        let _ = writeln!(
            warnings,
            "hotend-sandbox: WARNING: {UNSAFE_ENV}=1 is set. Running `{}` WITHOUT A SANDBOX: \
             it has full network and file access as your user. Debug builds only; \
             never use this with code you did not write.",
            program.to_string_lossy()
        );
        prepare_job_dirs(&policy.job_dir)?;
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(&policy.job_dir)
            .env_clear()
            .envs(policy.child_env(&policy.job_dir, |name| std::env::var_os(name)));
        no_console_window(&mut command);
        return Ok(command);
    }
    if unsafe_requested {
        let _ = writeln!(
            warnings,
            "hotend-sandbox: {UNSAFE_ENV} is ignored in release builds; the sandbox stays on."
        );
    }

    if let Some(error) = unsupported_platform() {
        return Err(error);
    }
    let mut command = launcher_command(launcher_path()?, std::env::consts::OS, |name| {
        std::env::var_os(name)
    });
    command.args(policy.launcher_args()).arg(program).args(args);
    for name in &policy.env_allowlist {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    Ok(command)
}

/// Asks the launcher which sandbox mechanisms this machine offers.
///
/// The probe runs in the launcher process because it has to fork, which a multi-threaded
/// application must not do itself.
pub fn probe() -> Result<ProbeReport, SandboxError> {
    let output = launcher_command(launcher_path()?, std::env::consts::OS, |name| {
        std::env::var_os(name)
    })
    .arg("probe")
    .stdin(Stdio::null())
    .stderr(Stdio::null())
    .output()
    .map_err(|e| SandboxError::Probe(format!("cannot run the launcher: {e}")))?;
    if !output.status.success() {
        return Err(SandboxError::Probe(format!(
            "the launcher exited with {}",
            output.status
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| SandboxError::Probe(format!("unreadable probe report: {e}")))
}

/// The value of `HOTEND_SANDBOX_BIN` when this build honours it. A release build ignores
/// the variable and says so: whatever it names would run in place of the sandbox.
fn launcher_override(debug_build: bool, warnings: &mut dyn Write) -> Option<PathBuf> {
    let path = std::env::var_os(LAUNCHER_ENV).filter(|value| !value.is_empty())?;
    if debug_build {
        return Some(PathBuf::from(path));
    }
    let _ = writeln!(
        warnings,
        "hotend-sandbox: {LAUNCHER_ENV} is ignored in release builds; the launcher beside \
         the running program is used."
    );
    None
}

/// The launcher: `hotend-sandbox` next to the current executable (or one folder up when
/// running from cargo's `deps` folder, as tests do). In debug builds `HOTEND_SANDBOX_BIN`
/// names another one.
fn launcher_path() -> Result<PathBuf, SandboxError> {
    let overridden = launcher_override(cfg!(debug_assertions), &mut std::io::stderr());
    let exe = std::env::current_exe().ok();
    resolve_launcher(overridden, exe.as_deref())
}

fn resolve_launcher(
    overridden: Option<PathBuf>,
    current_exe: Option<&std::path::Path>,
) -> Result<PathBuf, SandboxError> {
    if let Some(path) = overridden {
        return if path.is_file() {
            Ok(path)
        } else {
            Err(SandboxError::LauncherNotFound { path })
        };
    }
    let name = format!("hotend-sandbox{}", std::env::consts::EXE_SUFFIX);
    let exe = current_exe.ok_or_else(|| SandboxError::LauncherNotFound {
        path: PathBuf::from(&name),
    })?;
    let dir = exe.parent().unwrap_or(std::path::Path::new("."));
    let beside = dir.join(&name);
    if beside.is_file() {
        return Ok(beside);
    }
    if dir.file_name().is_some_and(|n| n == "deps")
        && let Some(above) = dir.parent().map(|d| d.join(&name))
        && above.is_file()
    {
        return Ok(above);
    }
    Err(SandboxError::LauncherNotFound { path: beside })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::policy::Profile;

    /// The macOS backend runs only when the person opted in, and the launcher is the one
    /// that reads the opt-in: it must reach the launcher, and only there.
    #[test]
    fn the_macos_opt_in_reaches_the_launcher_and_nothing_else_does() {
        let environment = |name: &str| match name {
            MACOS_OPT_IN_ENV => Some(std::ffi::OsString::from("1")),
            "ANTHROPIC_API_KEY" | "HOME" | "PATH" => Some(std::ffi::OsString::from("secret")),
            _ => None,
        };
        let envs = |command: &Command| -> Vec<(String, Option<String>)> {
            command
                .get_envs()
                .map(|(name, value)| {
                    (
                        name.to_string_lossy().into_owned(),
                        value.map(|v| v.to_string_lossy().into_owned()),
                    )
                })
                .collect()
        };
        let mac = launcher_command(PathBuf::from("/x/hotend-sandbox"), "macos", environment);
        assert_eq!(
            envs(&mac),
            [(MACOS_OPT_IN_ENV.to_owned(), Some("1".to_owned()))]
        );
        // Not set: not invented.
        let unset = launcher_command(PathBuf::from("/x/hotend-sandbox"), "macos", |_| None);
        assert!(envs(&unset).is_empty());
        // On the systems whose launcher has no such switch, nothing is passed at all.
        for os in ["linux", "windows"] {
            let other = launcher_command(PathBuf::from("/x/hotend-sandbox"), os, environment);
            assert!(envs(&other).is_empty(), "{os}");
        }
        // The sandboxed program's environment comes from the policy alone.
        let job = tempfile::tempdir().unwrap();
        let policy = Policy::new(Profile::Engine, job.path());
        let child = policy.child_env(job.path(), environment);
        assert!(
            !child.iter().any(|(name, _)| name == MACOS_OPT_IN_ENV),
            "{child:?}"
        );
    }

    #[test]
    fn unsafe_override_is_loud_and_only_works_in_debug_builds() {
        let job = tempfile::tempdir().unwrap();
        let policy = Policy::new(Profile::Engine, job.path());
        let mut warnings = Vec::new();
        let result = command_inner(&policy, "/bin/true", ["x"], true, &mut warnings);
        let text = String::from_utf8(warnings).unwrap();
        assert!(text.contains(UNSAFE_ENV), "no warning printed: {text:?}");
        if cfg!(debug_assertions) {
            let command = result.unwrap();
            assert!(text.contains("WITHOUT A SANDBOX"));
            assert_eq!(command.get_program(), OsStr::new("/bin/true"));
            let names: Vec<_> = command.get_envs().map(|(k, _)| k.to_owned()).collect();
            assert!(names.iter().any(|n| n == "HOME"));
        } else {
            assert!(text.contains("ignored"));
            if let Ok(command) = result {
                assert_ne!(command.get_program(), OsStr::new("/bin/true"));
            }
        }
    }

    /// Serialises the tests that change this process's environment.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn the_launcher_override_is_ignored_by_release_builds_and_says_so() {
        let _guard = ENV.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let stand_in = dir.path().join("not-a-sandbox");
        std::fs::write(&stand_in, "#!/bin/sh\nexec \"$@\"\n").unwrap();
        let saved = std::env::var_os(LAUNCHER_ENV);
        // SAFETY: the tests that read or write this variable hold `ENV`.
        unsafe { std::env::set_var(LAUNCHER_ENV, &stand_in) };

        let mut warnings = Vec::new();
        assert_eq!(launcher_override(false, &mut warnings), None);
        let text = String::from_utf8(warnings).unwrap();
        assert!(
            text.contains(LAUNCHER_ENV) && text.contains("ignored"),
            "{text:?}"
        );
        // What a release build then runs is the launcher beside the program, never the
        // file the variable names.
        let exe = dir.path().join("hotend");
        let beside = dir
            .path()
            .join(format!("hotend-sandbox{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&beside, "").unwrap();
        let ignored = launcher_override(false, &mut Vec::new());
        assert_eq!(resolve_launcher(ignored, Some(&exe)).unwrap(), beside);

        // A debug build honours it, silently, and reports it through `confinement`.
        let mut warnings = Vec::new();
        assert_eq!(
            launcher_override(true, &mut warnings),
            Some(stand_in.clone())
        );
        assert!(warnings.is_empty());
        assert_eq!(
            resolve_launcher(Some(stand_in.clone()), Some(&exe)).unwrap(),
            stand_in
        );
        let missing = resolve_launcher(Some(dir.path().join("gone")), Some(&exe));
        assert!(matches!(
            missing,
            Err(SandboxError::LauncherNotFound { .. })
        ));

        // SAFETY: as above.
        unsafe {
            match saved {
                Some(value) => std::env::set_var(LAUNCHER_ENV, value),
                None => std::env::remove_var(LAUNCHER_ENV),
            }
        }
    }

    #[test]
    fn invalid_policy_is_rejected_before_anything_is_looked_up() {
        let policy = Policy::new(Profile::Engine, "/definitely/not/here");
        let mut sink = Vec::new();
        let error = command_inner(&policy, "/bin/true", [""; 0], false, &mut sink).unwrap_err();
        assert!(matches!(error, SandboxError::InvalidPolicy(_)));
        assert_eq!(error.exit_code(), 125);
    }
}
