//! Locating the Python runtime that the engine and the agent run in.
//!
//! The core never installs or imports anything: it only needs the interpreter's path and the
//! folders a sandboxed child must be able to read to start it.
//!
//! Guarantees:
//! - A release build runs only the interpreter named in `HOTEND_PYTHON`, or the one its
//!   caller names: the desktop application names the interpreter its installer put under
//!   the application's resource folder ([`PythonRuntime::packaged_interpreter`]). The path
//!   of the checkout it was compiled in is a development convenience of debug builds; on
//!   another machine that path belongs to whoever creates it.
//! - The interpreter is asked for its prefixes outside the sandbox (the answer is what the
//!   sandbox needs to be set up), so on Unix it is run only if the file and its folder
//!   belong to this user or to root and others cannot write to them.
//! - When the home folder is known, no read-only path of the runtime is the top of a drive,
//!   the home folder, a folder above it, or `~/.local` itself: a sandboxed child could read
//!   all of it ([`RuntimeError::ReadOnlyTooWide`]). Folders below them are fine. `~/.local`
//!   is recognised by its own path, so not when it is a link to another folder; without a
//!   known home folder only the top of a drive is refused.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// Environment variable naming the Python interpreter (normally `<venv>/bin/python`).
pub const PYTHON_ENV: &str = "HOTEND_PYTHON";
/// Environment variable naming the folder that holds the `hotend_*` packages when they are
/// installed editable (this repository's `python/` folder).
pub const PYTHON_SRC_ENV: &str = "HOTEND_PYTHON_SRC";

/// Why the Python runtime cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// No interpreter at the configured or default location.
    #[error(
        "no Python interpreter at {path}; run `uv sync --frozen` in the python/ folder or set \
         {PYTHON_ENV} to the environment's interpreter"
    )]
    PythonNotFound { path: PathBuf },
    /// A release build was not told where its interpreter is.
    #[error(
        "{PYTHON_ENV} is not set; a release build of Hotend needs it to name the Python \
         interpreter of its environment (normally `<environment>/bin/python`)"
    )]
    NotConfigured,
    /// The interpreter could be replaced by another user of this machine.
    #[error(
        "the Python interpreter at {path} is not used: {reason}; it must belong to you or \
         to root and must not be writable by others"
    )]
    PythonUntrusted { path: PathBuf, reason: &'static str },
    /// The interpreter did not answer the prefix query.
    #[error("the Python interpreter at {path} does not start: {detail}")]
    PythonBroken { path: PathBuf, detail: String },
    /// A folder the sandbox would have to let AI-written code read is too wide.
    #[error(
        "the Python runtime is not used: sandboxed code would have to read {path}, which is \
         the top of a drive, your home folder, a folder above it or ~/.local, and so \
         everything in it"
    )]
    ReadOnlyTooWide { path: PathBuf },
}

impl RuntimeError {
    /// What the person can do about it, when this error says more than the general fix.
    pub fn fix(&self) -> Option<&'static str> {
        match self {
            Self::ReadOnlyTooWide { .. } => Some(
                "put the Python environment (and HOTEND_PYTHON_SRC, when it is set) in a \
                 folder of its own, for example a project folder's python/.venv or uv's \
                 ~/.local/share/uv/python, and point HOTEND_PYTHON at its interpreter",
            ),
            _ => None,
        }
    }
}

/// Whether letting sandboxed code read `dir` would expose too much: the top of a drive, the
/// home folder, a folder above it, or `~/.local` itself (the slicer's installation is held to
/// the same rule, `hotend_slicer::detect`).
fn too_wide(dir: &Path, home: Option<&Path>) -> bool {
    dir.parent().is_none()
        || home
            .is_some_and(|home| dir == home || home.starts_with(dir) || dir == home.join(".local"))
}

/// The first of `paths` that is [`too_wide`], as an error.
fn check_read_only(paths: &[PathBuf], home: Option<&Path>) -> Result<(), RuntimeError> {
    match paths.iter().find(|path| too_wide(path, home)) {
        Some(path) => Err(RuntimeError::ReadOnlyTooWide { path: path.clone() }),
        None => Ok(()),
    }
}

/// The home folder, as the sandbox sees it (links resolved).
fn home_folder() -> Option<PathBuf> {
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    Some(home.canonicalize().unwrap_or(home))
}

/// A Python interpreter and the read-only paths a sandboxed child needs to run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonRuntime {
    /// The interpreter, as configured (not resolved, so a virtual environment is detected).
    pub python: PathBuf,
    /// The environment, the interpreter's real installation prefix and, for an editable
    /// install, the package source folder.
    pub read_only: Vec<PathBuf>,
}

impl PythonRuntime {
    /// Finds the runtime: `HOTEND_PYTHON` and `HOTEND_PYTHON_SRC` when set. Without them
    /// a debug build uses the development layout of this repository
    /// (`python/.venv/bin/python` and `python/`); a release build fails with
    /// [`RuntimeError::NotConfigured`].
    pub async fn discover() -> Result<Self, RuntimeError> {
        let set = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let (python, source) =
            locate(cfg!(debug_assertions), set(PYTHON_ENV), set(PYTHON_SRC_ENV))?;
        Self::from_interpreter(python, source).await
    }

    /// Where an installed Hotend keeps its interpreter: the Python environment is the
    /// folder `python` under the application's resource folder, with the interpreter at
    /// `bin/python` (`python.exe` on Windows). The `hotend_*` packages are installed in
    /// that environment, so there is no source folder to add.
    pub fn packaged_interpreter(resources: &Path) -> PathBuf {
        let environment = resources.join("python");
        if cfg!(windows) {
            environment.join("python.exe")
        } else {
            environment.join("bin").join("python")
        }
    }

    /// Builds the runtime for a known interpreter by asking it for its prefixes.
    ///
    /// The interpreter is Hotend's own and runs here with an empty environment, in isolated
    /// mode, executing one fixed line; no model or agent code is involved.
    pub async fn from_interpreter(
        python: PathBuf,
        source: Option<PathBuf>,
    ) -> Result<Self, RuntimeError> {
        if !python.is_file() {
            return Err(RuntimeError::PythonNotFound { path: python });
        }
        if let Some(reason) = untrusted(&python) {
            return Err(RuntimeError::PythonUntrusted {
                path: python,
                reason,
            });
        }
        let broken = |detail: String| RuntimeError::PythonBroken {
            path: python.clone(),
            detail,
        };
        let mut command = tokio::process::Command::new(&python);
        command
            .args([
                "-I",
                "-c",
                "import sys; print(sys.prefix); print(sys.base_prefix)",
            ])
            .env_clear()
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // No console window beside the application's own window on Windows.
        #[cfg(windows)]
        command.creation_flags(hotend_sandbox::CREATE_NO_WINDOW);
        let output = tokio::time::timeout(Duration::from_secs(30), command.output())
            .await
            .map_err(|_| broken("no answer within 30 seconds".into()))?
            .map_err(|e| broken(e.to_string()))?;
        if !output.status.success() {
            return Err(broken(format!("exited with {}", output.status)));
        }
        let mut read_only: Vec<PathBuf> = Vec::new();
        let mut add = |path: PathBuf| {
            if let Ok(real) = path.canonicalize()
                && !read_only.iter().any(|known| real.starts_with(known))
            {
                read_only.push(real);
            }
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let prefixes: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if prefixes.len() != 2 {
            return Err(broken("unexpected answer to the prefix query".into()));
        }
        for prefix in prefixes {
            add(PathBuf::from(prefix));
        }
        // A base interpreter that lives outside its own prefix (unusual) must be readable too.
        if let Some(real_dir) = python
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            add(real_dir);
        }
        if let Some(source) = source {
            add(source);
        }
        check_read_only(&read_only, home_folder().as_deref())?;
        Ok(Self { python, read_only })
    }
}

/// Where the interpreter and the package sources are, from the two environment variables
/// and the kind of build.
fn locate(
    debug_build: bool,
    python: Option<PathBuf>,
    source: Option<PathBuf>,
) -> Result<(PathBuf, Option<PathBuf>), RuntimeError> {
    if !debug_build {
        return Ok((python.ok_or(RuntimeError::NotConfigured)?, source));
    }
    let dev_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python");
    let python = python.unwrap_or_else(|| {
        dev_root.join(if cfg!(windows) {
            ".venv/Scripts/python.exe"
        } else {
            ".venv/bin/python"
        })
    });
    let source = source.or_else(|| {
        dev_root
            .join("pyproject.toml")
            .is_file()
            .then_some(dev_root)
    });
    Ok((python, source))
}

/// Why a program must not be run with this user's rights, if there is a reason: the file
/// (after following links) or the folder it is in can be replaced by someone else.
#[cfg(unix)]
fn untrusted(program: &Path) -> Option<&'static str> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    let mine = |meta: &std::fs::Metadata| meta.uid() == me || meta.uid() == 0;
    let Ok(real) = program.canonicalize() else {
        return Some("it cannot be resolved");
    };
    let Ok(file) = real.metadata() else {
        return Some("it cannot be examined");
    };
    if !mine(&file) {
        return Some("the file belongs to another user");
    }
    if file.mode() & 0o002 != 0 {
        return Some("the file is writable by everyone");
    }
    let Some(Ok(folder)) = real.parent().map(Path::metadata) else {
        return Some("its folder cannot be examined");
    };
    if !mine(&folder) {
        return Some("its folder belongs to another user");
    }
    // A sticky world-writable folder (like /tmp) lets nobody replace another user's file.
    if folder.mode() & 0o002 != 0 && folder.mode() & 0o1000 == 0 {
        return Some("its folder is writable by everyone");
    }
    None
}

#[cfg(not(unix))]
fn untrusted(_program: &Path) -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn an_installed_hotend_has_its_interpreter_under_the_resource_folder() {
        let python = PythonRuntime::packaged_interpreter(Path::new("/usr/lib/Hotend"));
        if cfg!(windows) {
            assert_eq!(python, Path::new("/usr/lib/Hotend/python/python.exe"));
        } else {
            assert_eq!(python, Path::new("/usr/lib/Hotend/python/bin/python"));
        }
    }

    #[test]
    fn a_release_build_never_falls_back_to_the_checkout_it_was_built_in() {
        let error = locate(false, None, None).unwrap_err();
        assert!(matches!(error, RuntimeError::NotConfigured));
        assert!(error.to_string().contains(PYTHON_ENV));
        // Not even for the package sources.
        let (python, source) = locate(false, Some("/opt/hotend/bin/python".into()), None).unwrap();
        assert_eq!(python, Path::new("/opt/hotend/bin/python"));
        assert_eq!(source, None);
        let (_, source) = locate(
            false,
            Some("/opt/hotend/bin/python".into()),
            Some("/opt/hotend/src".into()),
        )
        .unwrap();
        assert_eq!(source.as_deref(), Some(Path::new("/opt/hotend/src")));

        // A debug build uses the checkout, and the variables win over it.
        let (python, source) = locate(true, None, None).unwrap();
        assert!(python.starts_with(env!("CARGO_MANIFEST_DIR")));
        assert!(source.is_some_and(|source| source.starts_with(env!("CARGO_MANIFEST_DIR"))));
        let (python, _) = locate(true, Some("/opt/hotend/bin/python".into()), None).unwrap();
        assert_eq!(python, Path::new("/opt/hotend/bin/python"));
    }

    /// The home folder, anything above it, `~/.local` and the top of a drive are refused as
    /// read-only folders; folders below them are not.
    #[test]
    fn a_read_only_folder_that_would_expose_the_home_folder_is_refused() {
        let home = Path::new("/home/maker");
        for wide in ["/", "/home", "/home/maker", "/home/maker/.local"] {
            let error = check_read_only(&[PathBuf::from("/usr"), PathBuf::from(wide)], Some(home))
                .unwrap_err();
            assert!(
                matches!(&error, RuntimeError::ReadOnlyTooWide { path } if path == Path::new(wide)),
                "{wide}: {error}"
            );
            assert!(error.fix().is_some_and(|fix| fix.contains("HOTEND_PYTHON")));
        }
        let fine: Vec<PathBuf> = [
            "/usr",
            "/home/maker/.local/share/uv/python/cpython-3.13.5-linux-x86_64-gnu",
            "/home/maker/hotend/python/.venv",
            "/home/maker/hotend/python",
            "/home/maker/.localtools",
            "/opt/hotend/python",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        check_read_only(&fine, Some(home)).unwrap();
        // Without a known home folder only the top of a drive is refused.
        check_read_only(&[PathBuf::from("/home/maker")], None).unwrap();
        assert!(check_read_only(&[PathBuf::from("/")], None).is_err());
    }

    /// An environment whose prefix is the home folder is refused before anything runs in
    /// the sandbox; the error says what to do.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_runtime_rooted_in_the_home_folder_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let Some(home) = home_folder() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("python");
        let body = format!("#!/bin/sh\necho {}\necho /usr\n", home.display());
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut attempt = 0;
        let error = loop {
            match PythonRuntime::from_interpreter(path.clone(), None).await {
                // A file just written may still be open for writing elsewhere (ETXTBSY).
                Err(RuntimeError::PythonBroken { detail, .. })
                    if detail.contains("os error 26") && attempt < 50 =>
                {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => break other.unwrap_err(),
            }
        };
        assert!(
            matches!(&error, RuntimeError::ReadOnlyTooWide { path } if *path == home),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_interpreter_that_others_can_replace_is_not_run() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("ran");
        let script = |dir: &Path| {
            std::fs::create_dir_all(dir).unwrap();
            let path = dir.join("python");
            let body = format!(
                "#!/bin/sh\ntouch {}\necho /usr\necho /usr\n",
                marker.display()
            );
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };

        // A folder anyone may write to: the file could be swapped before it is run.
        let open_dir = tmp.path().join("open");
        let python = script(&open_dir);
        std::fs::set_permissions(&open_dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let error = PythonRuntime::from_interpreter(python.clone(), None)
            .await
            .unwrap_err();
        assert!(
            matches!(error, RuntimeError::PythonUntrusted { .. }),
            "{error}"
        );
        // Reached through a link, it is still the real file that counts.
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&python, &link).unwrap();
        assert!(matches!(
            PythonRuntime::from_interpreter(link, None).await,
            Err(RuntimeError::PythonUntrusted { .. })
        ));
        // A file anyone may write to.
        let python = script(&tmp.path().join("closed"));
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            PythonRuntime::from_interpreter(python.clone(), None).await,
            Err(RuntimeError::PythonUntrusted { .. })
        ));
        assert!(!marker.exists(), "a refused interpreter was run");

        // The same file, owned by this user and closed to others, is used.
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        // While this test wrote the script, another test thread may have forked; its child
        // holds a copy of the write descriptor until it calls exec, and the kernel refuses to
        // run a file open for writing (ETXTBSY). That is the test process racing itself, not
        // the check under test, so wait for the child to let go.
        let mut attempt = 0;
        loop {
            match PythonRuntime::from_interpreter(python.clone(), None).await {
                Err(RuntimeError::PythonBroken { detail, .. })
                    if detail.contains("os error 26") && attempt < 50 =>
                {
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                result => {
                    result.unwrap();
                    break;
                }
            }
        }
        assert!(marker.exists());
    }

    #[tokio::test]
    async fn a_missing_interpreter_is_a_typed_error_that_names_the_fix() {
        let error = PythonRuntime::from_interpreter("/nonexistent/python".into(), None)
            .await
            .unwrap_err();
        assert!(matches!(error, RuntimeError::PythonNotFound { .. }));
        assert!(error.to_string().contains(PYTHON_ENV));
    }

    #[tokio::test]
    async fn the_repository_runtime_is_found_with_its_read_only_paths() {
        let Ok(runtime) = PythonRuntime::discover().await else {
            eprintln!("SKIPPED: no Python environment in python/.venv");
            return;
        };
        assert!(runtime.python.is_file());
        assert!(runtime.read_only.len() >= 2, "{runtime:?}");
        assert!(
            runtime
                .read_only
                .iter()
                .all(|p| p.is_absolute() && p.exists())
        );
    }
}
