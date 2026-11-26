//! Opening the Bambu Connect import link: one small interface, one stand-in, and one real
//! opener per platform.
//!
//! Guarantees:
//! - an opener takes a [`BambuConnectLink`], which only [`crate::HandOff::link`] makes:
//!   after a person's hand-off confirmation, for the file that passed the check, while the
//!   placed copy still holds those bytes. A link built from any other text cannot be
//!   passed to an opener;
//! - a real opener starts exactly one program by its absolute path, with the link (or the
//!   placed project's path) as one argument: `std::process::Command`, no shell, no
//!   `cmd /c start`, nothing interpolated. What it starts is an [`OpenCommand`] built by a
//!   pure function, tested without running anything;
//! - on Windows and from WSL the link goes to `rundll32.exe url.dll,FileProtocolHandler`,
//!   which hands it to the Windows shell as Win+R does, and so to the program registered
//!   for `bambu-connect:`; a project goes to `explorer.exe`. Both start in the Windows
//!   directory: from WSL a program otherwise starts in `\\wsl.localhost\...`;
//! - success means the program was started. `explorer.exe` exits with 1 even when it did
//!   what it was asked, so no exit status is read;
//! - no real opener runs in this crate's test build or while `HOTEND_STAND_IN_OPENER` is
//!   set ([`real_openers_allowed`]); [`platform_opener`] then gives the stand-in;
//! - [`StandInOpener`] opens nothing. It records every link it was asked to open, in memory
//!   and, when `HOTEND_STAND_IN_OPENER` names a file, as one line appended to that file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::delivery::BambuConnectLink;
use crate::handoff::{HostKind, PlacedProject};

/// The environment variable that makes [`platform_opener`] give the stand-in, and that
/// names the file the stand-in appends what it was asked to open to.
pub const STAND_IN_ENV: &str = "HOTEND_STAND_IN_OPENER";

/// Why a link or a file was not opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenError {
    /// Real openers are switched off in this process (a test build, or the stand-in is
    /// named).
    #[error("real openers are switched off in this process ({0})")]
    Refused(&'static str),
    /// There is no Bambu Connect on this system.
    #[error("Bambu Connect runs on Windows and macOS; there is nothing to open it with here")]
    Unsupported,
    /// The program could not be started.
    #[error("`{program}` could not be started: {reason}")]
    Spawn { program: String, reason: String },
    /// The stand-in could not write its record.
    #[error("the stand-in opener could not write its record: {0}")]
    Record(String),
}

/// What opens the import link (and, for the "Open in Bambu Studio" fallback, a placed
/// project file with the program the system associates with it).
pub trait LinkOpener: Send + Sync {
    /// Opens the Bambu Connect import link.
    fn open(&self, link: &BambuConnectLink) -> Result<(), OpenError>;

    /// Opens a project file that Hotend placed in the hand-off folder with the program the
    /// system associates with `.3mf` files.
    fn open_project(&self, project: &PlacedProject) -> Result<(), OpenError>;

    /// What this opener is, for the job record and the interface.
    fn describe(&self) -> &'static str;

    /// True for an opener that opens nothing (the stand-in).
    fn is_stand_in(&self) -> bool {
        false
    }
}

/// `Ok` when a real opener may run in this process: not in this crate's test build, and
/// not while [`STAND_IN_ENV`] is set.
pub fn real_openers_allowed() -> Result<(), OpenError> {
    if cfg!(test) {
        return Err(OpenError::Refused("test build"));
    }
    if std::env::var_os(STAND_IN_ENV).is_some() {
        return Err(OpenError::Refused("HOTEND_STAND_IN_OPENER is set"));
    }
    Ok(())
}

/// The opener for `host`: the stand-in while real openers are switched off, otherwise
/// the Windows shell through WSL's Windows interop, the Windows shell on Windows, `open`
/// on macOS, and none on Linux.
pub fn platform_opener(host: HostKind) -> Arc<dyn LinkOpener> {
    if real_openers_allowed().is_err() {
        return Arc::new(StandInOpener::from_env());
    }
    match host {
        HostKind::Wsl => Arc::new(WindowsFromWsl),
        HostKind::Windows => Arc::new(NativeWindows),
        HostKind::MacOs => Arc::new(MacOs),
        HostKind::Linux => Arc::new(NoBambuConnect),
    }
}

/// Opens nothing; records what it was asked to open.
#[derive(Debug, Default)]
pub struct StandInOpener {
    asked: Mutex<Vec<String>>,
    file: Option<PathBuf>,
}

impl StandInOpener {
    /// A stand-in that records in memory only.
    pub fn new() -> Self {
        Self::default()
    }

    /// A stand-in that also appends each record, one line, to `file`.
    pub fn recording_to(file: impl Into<PathBuf>) -> Self {
        Self {
            asked: Mutex::new(Vec::new()),
            file: Some(file.into()),
        }
    }

    /// A stand-in that appends to the file [`STAND_IN_ENV`] names, when it names one.
    pub fn from_env() -> Self {
        match std::env::var_os(STAND_IN_ENV).filter(|value| !value.is_empty()) {
            Some(file) => Self::recording_to(file),
            None => Self::new(),
        }
    }

    /// Every link it was asked to open, oldest first. A project is recorded as
    /// `open-project <path>`.
    pub fn asked(&self) -> Vec<String> {
        self.asked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn record(&self, line: String) -> Result<(), OpenError> {
        if let Some(file) = &self.file {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
                .and_then(|mut out| writeln!(out, "{line}"))
                .map_err(|error| OpenError::Record(error.to_string()))?;
        }
        self.asked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(line);
        Ok(())
    }
}

impl LinkOpener for StandInOpener {
    fn open(&self, link: &BambuConnectLink) -> Result<(), OpenError> {
        self.record(link.as_str().to_owned())
    }

    fn open_project(&self, project: &PlacedProject) -> Result<(), OpenError> {
        self.record(format!("open-project {}", project.path().link_path))
    }

    fn describe(&self) -> &'static str {
        "stand-in opener: nothing was opened"
    }

    fn is_stand_in(&self) -> bool {
        true
    }
}

/// Exactly what a real opener starts: one program by its absolute path, its arguments
/// (each passed as one argument, never through a shell) and the directory it starts in.
/// The openers build it with pure functions, so what would run is tested without running it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCommand {
    /// The program, by its absolute path.
    pub program: PathBuf,
    /// Its arguments, each one argument.
    pub args: Vec<String>,
    /// The directory it starts in; `None` keeps Hotend's own.
    pub current_dir: Option<PathBuf>,
}

/// The process [`start`] spawns for `command`: no shell, no standard streams, and Hotend's
/// environment without the variables that may hold a secret ([`crate::secret::SECRET_ENV`]),
/// which the opened program has no use for.
fn process(command: &OpenCommand) -> Command {
    let mut process = Command::new(&command.program);
    process
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::secret::without_secret_env(&mut process);
    if let Some(dir) = &command.current_dir {
        process.current_dir(dir);
    }
    process
}

/// Starts `command` and reaps it on a thread of its own. Success is the start: the exit
/// status is not read.
fn start(command: &OpenCommand) -> Result<(), OpenError> {
    real_openers_allowed()?;
    let mut child = process(command).spawn().map_err(|error| OpenError::Spawn {
        program: command.program.display().to_string(),
        reason: error.to_string(),
    })?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The Windows path of a project for explorer.exe, which opens it with its program.
fn project_windows_path(project: &PlacedProject) -> Result<&str, OpenError> {
    project
        .path()
        .windows
        .as_deref()
        .ok_or(OpenError::Unsupported)
}

/// The `url.dll` entry point that hands its text to the Windows shell (`ShellExecute`, as
/// Win+R does), which starts the program registered for the link's scheme.
pub const FILE_PROTOCOL_HANDLER: &str = "url.dll,FileProtocolHandler";

/// The link text, checked again for the one character class a link may hold: letters,
/// digits and ``-_.!~*'()`` (what `encodeURIComponent` leaves), `%` followed by two hex
/// digits, and the link's own `:/?=&`. rundll32 hands everything after its first argument
/// to the entry point as one raw string (no parsing, no quotes removed), so a space, a
/// quote or any non-ASCII character would change what Windows receives; none can be in a
/// link the core built, and this refuses one that somehow is.
fn checked_link(link: &BambuConnectLink) -> Result<&str, OpenError> {
    let text = link.as_str();
    // The shell hands any scheme to its program: only Bambu Connect's import link, in the
    // one shape `HandOff::link` writes, may reach it.
    let shaped = text
        .strip_prefix(crate::delivery::LINK_PREFIX)
        .is_some_and(|rest| rest.matches('&').count() == 2 && rest.ends_with("&version=1.0.0"));
    if !shaped {
        return Err(OpenError::Spawn {
            program: FILE_PROTOCOL_HANDLER.to_owned(),
            reason: "the link is not a Bambu Connect import link".to_owned(),
        });
    }
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let fine = byte.is_ascii_alphanumeric()
            || b"-_.!~*'():/?=&".contains(&byte)
            || (byte == b'%'
                && bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit));
        if !fine {
            return Err(OpenError::Spawn {
                program: FILE_PROTOCOL_HANDLER.to_owned(),
                reason: format!("the link holds a character it may not hold at {index}"),
            });
        }
        index += 1;
    }
    Ok(text)
}

/// The command that opens `link` on Windows: `<rundll32> url.dll,FileProtocolHandler
/// <link>`, started in `windows_dir` (a Windows directory, never the WSL share).
fn windows_link_command(
    rundll32: PathBuf,
    windows_dir: PathBuf,
    link: &BambuConnectLink,
) -> Result<OpenCommand, OpenError> {
    Ok(OpenCommand {
        program: rundll32,
        args: vec![
            FILE_PROTOCOL_HANDLER.to_owned(),
            checked_link(link)?.to_owned(),
        ],
        current_dir: Some(windows_dir),
    })
}

/// The command that opens a placed project on Windows: `<explorer> <Windows path>`,
/// started in `windows_dir`. The path is one argument; a path with a space reaches
/// explorer.exe quoted as one (WSL's interop and Rust on Windows both quote it), and a
/// comma, which explorer.exe would read as the end of the path, is refused when the folder
/// is chosen ([`crate::HandOffPathError::Comma`]).
fn windows_project_command(
    explorer: PathBuf,
    windows_dir: PathBuf,
    project: &PlacedProject,
) -> Result<OpenCommand, OpenError> {
    Ok(OpenCommand {
        program: explorer,
        args: vec![project_windows_path(project)?.to_owned()],
        current_dir: Some(windows_dir),
    })
}

/// The Bambu Connect link and placed projects, from WSL through its Windows interop: the
/// link through `rundll32.exe url.dll,FileProtocolHandler`, a project through
/// `explorer.exe`, both started in `C:\Windows`.
#[derive(Debug, Clone, Copy)]
pub struct WindowsFromWsl;

/// Where WSL mounts the Windows explorer.
pub const WSL_EXPLORER: &str = "/mnt/c/Windows/explorer.exe";

/// Where WSL mounts rundll32.
pub const WSL_RUNDLL32: &str = "/mnt/c/Windows/System32/rundll32.exe";

/// The Windows directory as WSL mounts it: what the opened programs start in. A program
/// started from WSL otherwise starts in `\\wsl.localhost\<distribution>\...`, a UNC path
/// Windows programs do not accept as their directory.
pub const WSL_WINDOWS_DIR: &str = "/mnt/c/Windows";

impl WindowsFromWsl {
    /// The command [`LinkOpener::open`] starts for `link`.
    pub fn link_command(link: &BambuConnectLink) -> Result<OpenCommand, OpenError> {
        windows_link_command(WSL_RUNDLL32.into(), WSL_WINDOWS_DIR.into(), link)
    }

    /// The command [`LinkOpener::open_project`] starts for `project`.
    pub fn project_command(project: &PlacedProject) -> Result<OpenCommand, OpenError> {
        windows_project_command(WSL_EXPLORER.into(), WSL_WINDOWS_DIR.into(), project)
    }
}

impl LinkOpener for WindowsFromWsl {
    fn open(&self, link: &BambuConnectLink) -> Result<(), OpenError> {
        start(&Self::link_command(link)?)
    }

    fn open_project(&self, project: &PlacedProject) -> Result<(), OpenError> {
        start(&Self::project_command(project)?)
    }

    fn describe(&self) -> &'static str {
        "Windows shell, from WSL"
    }
}

/// The Bambu Connect link and placed projects on Windows: like [`WindowsFromWsl`], with
/// the programs under `%SystemRoot%`, started there.
#[derive(Debug, Clone, Copy)]
pub struct NativeWindows;

impl NativeWindows {
    fn system_root() -> PathBuf {
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .filter(|root| root.is_absolute())
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
    }

    /// The command [`LinkOpener::open`] starts for `link`, with Windows under `root`.
    pub fn link_command(root: &Path, link: &BambuConnectLink) -> Result<OpenCommand, OpenError> {
        windows_link_command(
            root.join("System32").join("rundll32.exe"),
            root.to_path_buf(),
            link,
        )
    }

    /// The command [`LinkOpener::open_project`] starts for `project`, with Windows under
    /// `root`.
    pub fn project_command(root: &Path, project: &PlacedProject) -> Result<OpenCommand, OpenError> {
        windows_project_command(root.join("explorer.exe"), root.to_path_buf(), project)
    }
}

impl LinkOpener for NativeWindows {
    fn open(&self, link: &BambuConnectLink) -> Result<(), OpenError> {
        start(&Self::link_command(&Self::system_root(), link)?)
    }

    fn open_project(&self, project: &PlacedProject) -> Result<(), OpenError> {
        start(&Self::project_command(&Self::system_root(), project)?)
    }

    fn describe(&self) -> &'static str {
        "Windows shell"
    }
}

/// `/usr/bin/open` on macOS.
#[derive(Debug, Clone, Copy)]
pub struct MacOs;

impl MacOs {
    fn open_command(argument: &str) -> OpenCommand {
        OpenCommand {
            program: PathBuf::from("/usr/bin/open"),
            args: vec![argument.to_owned()],
            current_dir: None,
        }
    }
}

impl LinkOpener for MacOs {
    fn open(&self, link: &BambuConnectLink) -> Result<(), OpenError> {
        start(&Self::open_command(link.as_str()))
    }

    fn open_project(&self, project: &PlacedProject) -> Result<(), OpenError> {
        start(&Self::open_command(&project.path().link_path))
    }

    fn describe(&self) -> &'static str {
        "macOS open"
    }
}

/// Linux without WSL: there is no Bambu Connect to open.
#[derive(Debug, Clone, Copy)]
pub struct NoBambuConnect;

impl LinkOpener for NoBambuConnect {
    fn open(&self, _link: &BambuConnectLink) -> Result<(), OpenError> {
        Err(OpenError::Unsupported)
    }

    fn open_project(&self, _project: &PlacedProject) -> Result<(), OpenError> {
        Err(OpenError::Unsupported)
    }

    fn describe(&self) -> &'static str {
        "no opener (Bambu Connect runs on Windows and macOS)"
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// The guard: in this test build no real opener can start anything, whatever it is
    /// asked, and the platform's opener is the stand-in.
    #[test]
    fn no_real_opener_runs_in_a_test_build() {
        assert_eq!(
            real_openers_allowed(),
            Err(OpenError::Refused("test build"))
        );
        let command = OpenCommand {
            program: PathBuf::from("/bin/true"),
            args: vec!["x".to_owned()],
            current_dir: Some(PathBuf::from("/")),
        };
        assert_eq!(start(&command), Err(OpenError::Refused("test build")));
        for host in [
            HostKind::Wsl,
            HostKind::Windows,
            HostKind::MacOs,
            HostKind::Linux,
        ] {
            let opener = platform_opener(host);
            assert!(opener.is_stand_in(), "{host:?}: {}", opener.describe());
        }
    }

    /// The opened program inherits neither the printer's access code nor an API key: the
    /// process a real opener would spawn has the variables removed (built, never spawned).
    #[test]
    fn the_opened_program_does_not_inherit_a_secret() {
        let command = OpenCommand {
            program: PathBuf::from("/usr/bin/xdg-open"),
            args: vec!["x".to_owned()],
            current_dir: Some(PathBuf::from("/")),
        };
        let process = process(&command);
        let mut removed: Vec<_> = process
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .filter_map(|(name, _)| name.to_str())
            .collect();
        removed.sort_unstable();
        let mut secret = crate::secret::SECRET_ENV;
        secret.sort_unstable();
        assert_eq!(removed, secret);
        for name in [
            "BAMBU_LAN_ACCESS_CODE",
            "ANTHROPIC_API_KEY",
            "TAVILY_API_KEY",
        ] {
            assert!(removed.contains(&name), "{name}");
        }
        assert!(process.get_envs().all(|(_, value)| value.is_none()));
        assert_eq!(process.get_program(), command.program.as_os_str());
    }

    const SAMPLE_PATH: &str =
        r"D:\Models\Hotend\bracket-0123456789abcdef0123456789abcdef.gcode.3mf";
    const SAMPLE_LINK: &str = "bambu-connect://import-file?path=D%3A%5CModels%5CHotend%5C\
        bracket-0123456789abcdef0123456789abcdef.gcode.3mf&name=Wall%20bracket&version=1.0.0";

    /// From WSL the link goes to rundll32 url.dll,FileProtocolHandler as one argument, `&`
    /// and all, and rundll32 starts in the Windows directory, not on the WSL share.
    #[test]
    fn from_wsl_the_link_goes_to_the_windows_shell_from_the_windows_directory() {
        let link = BambuConnectLink::for_tests(SAMPLE_PATH, "Wall bracket");
        assert_eq!(link.as_str(), SAMPLE_LINK);
        assert_eq!(
            WindowsFromWsl::link_command(&link).unwrap(),
            OpenCommand {
                program: PathBuf::from("/mnt/c/Windows/System32/rundll32.exe"),
                args: vec![
                    "url.dll,FileProtocolHandler".to_owned(),
                    SAMPLE_LINK.to_owned()
                ],
                current_dir: Some(PathBuf::from("/mnt/c/Windows")),
            }
        );
    }

    #[test]
    fn on_windows_the_link_goes_to_the_same_entry_point_under_the_system_root() {
        let link = BambuConnectLink::for_tests(SAMPLE_PATH, "Wall bracket");
        let root = Path::new(r"C:\Windows");
        assert_eq!(
            NativeWindows::link_command(root, &link).unwrap(),
            OpenCommand {
                program: root.join("System32").join("rundll32.exe"),
                args: vec![
                    "url.dll,FileProtocolHandler".to_owned(),
                    SAMPLE_LINK.to_owned()
                ],
                current_dir: Some(root.to_path_buf()),
            }
        );
    }

    /// Whatever a folder, file or label holds, the link that reaches rundll32 holds only
    /// characters nothing on the way re-reads: no space, quote, `^`, `,`, `;`, `<`, `>`,
    /// `|`, non-ASCII, and `%` only as an escape of two hex digits; `&` only twice, as the
    /// link's own separators.
    #[test]
    fn nothing_in_a_path_or_a_label_reaches_rundll32_unencoded() {
        let hostile_path =
            "D:\\Models\\A & B ^ 100% \"q\" 'x' (1) ;=,!~\u{e9}\u{4e2d}\t\\a b.gcode.3mf";
        let hostile_label = "x\" & calc ^ %PATH% , y\u{e9}";
        let link = BambuConnectLink::for_tests(hostile_path, hostile_label);
        let command = WindowsFromWsl::link_command(&link).unwrap();
        assert_eq!(command.args.len(), 2);
        let text = &command.args[1];
        assert_eq!(text, link.as_str());
        assert!(text.starts_with(crate::delivery::LINK_PREFIX));
        for bad in [' ', '"', '^', ',', ';', '<', '>', '|', '\t', '\\'] {
            assert!(!text.contains(bad), "{bad:?} in {text}");
        }
        assert!(text.is_ascii());
        assert_eq!(text.matches('&').count(), 2, "{text}");
        assert!(text.ends_with("&version=1.0.0"), "{text}");
        let bytes = text.as_bytes();
        for (index, byte) in bytes.iter().enumerate() {
            if *byte == b'%' {
                assert!(
                    bytes[index + 1].is_ascii_hexdigit() && bytes[index + 2].is_ascii_hexdigit()
                );
            }
        }
        // The path's own `%` is encoded, so no `%PATH%`-like sequence survives.
        assert!(text.contains("100%25"), "{text}");
        assert!(!text.contains("%PATH%"), "{text}");
    }

    /// A placed project under a folder whose name has spaces and `&` goes to explorer.exe
    /// as one argument (quoted by the interop, not by Hotend), from the Windows directory.
    #[test]
    fn a_project_goes_to_explorer_as_one_argument_from_the_windows_directory() {
        let root = tempfile::tempdir().unwrap();
        let drives = root.path().join("mnt");
        let folder = drives.join("d").join("Models").join("My Prints & Co");
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("Wall bracket.3mf");
        std::fs::write(&file, b"project").unwrap();
        let chosen =
            crate::handoff::HandOffFolder::with_drives_root(&folder, HostKind::Wsl, &drives)
                .unwrap();
        let project = chosen.placed_project(&file).unwrap();
        let expected = r"D:\Models\My Prints & Co\Wall bracket.3mf";
        assert_eq!(
            WindowsFromWsl::project_command(&project).unwrap(),
            OpenCommand {
                program: PathBuf::from("/mnt/c/Windows/explorer.exe"),
                args: vec![expected.to_owned()],
                current_dir: Some(PathBuf::from("/mnt/c/Windows")),
            }
        );
        let root_dir = Path::new(r"C:\Windows");
        assert_eq!(
            NativeWindows::project_command(root_dir, &project).unwrap(),
            OpenCommand {
                program: root_dir.join("explorer.exe"),
                args: vec![expected.to_owned()],
                current_dir: Some(root_dir.to_path_buf()),
            }
        );

        // A comma, which explorer.exe would read as the end of the path, never gets this
        // far: such a folder is refused when it is chosen.
        let comma = drives.join("d").join("Models, old");
        std::fs::create_dir_all(&comma).unwrap();
        assert!(matches!(
            crate::handoff::HandOffFolder::with_drives_root(&comma, HostKind::Wsl, &drives),
            Err(crate::handoff::HandOffPathError::Comma(_))
        ));
    }

    /// A link holding a character outside its class is refused before anything starts.
    #[test]
    fn a_link_with_a_stray_character_is_refused() {
        for text in [
            "bambu-connect://import-file?path=a b",
            "bambu-connect://import-file?path=a\"b",
            "bambu-connect://import-file?path=a%2",
            "bambu-connect://import-file?path=a%zz",
            "bambu-connect://import-file?path=\u{e9}",
            "bambu-connect://import-file?path=a^b",
            // Well-formed characters, but not the one shape of an import link: another
            // scheme the shell would start, or extra parameters.
            "file:/C:/Windows/System32/calc.exe",
            "ms-settings:privacy",
            "bambu-connect://import-file?path=a&name=b&version=1.0.0&x=y",
            "bambu-connect://import-file?path=a&name=b",
        ] {
            let link = BambuConnectLink::raw_for_tests(text);
            assert!(
                matches!(
                    WindowsFromWsl::link_command(&link),
                    Err(OpenError::Spawn { .. })
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn macos_open_is_unchanged() {
        assert_eq!(
            MacOs::open_command(SAMPLE_LINK),
            OpenCommand {
                program: PathBuf::from("/usr/bin/open"),
                args: vec![SAMPLE_LINK.to_owned()],
                current_dir: None,
            }
        );
    }
}
