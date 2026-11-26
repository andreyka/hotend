//! The hand-off folder: where a checked file is placed for Bambu Connect, and the path the
//! import link names.
//!
//! Bambu Connect runs on Windows and macOS. When Hotend runs inside WSL, the file must be
//! on a Windows drive and the link must carry the Windows form of its path; Hotend's own
//! process sees the same file as `/mnt/<drive letter>/...`. The translation is a pure
//! function of the path ([`wsl_to_windows`]): no `wslpath`, no shell, no other program.
//!
//! Guarantees:
//! - a [`HandOffFolder`] is an existing directory, canonicalised once when it is chosen;
//!   under WSL it is a directory strictly below `/mnt/<one drive letter>/`, every name in
//!   it is one Windows accepts ([`check_windows_name`]), and its Windows form is at most
//!   [`MAX_WINDOWS_PATH`] characters;
//! - a placed file is resolved again before its path is used ([`HandOffFolder::resolve`]):
//!   its canonical path must be a regular file directly inside the chosen folder, under the
//!   name it was placed as. A symbolic link that leads anywhere else, a `..`, a name
//!   Windows forbids (`<>:"/\|?*`, control characters, a trailing dot or space) or a
//!   reserved device name (`CON`, `NUL`, `COM1`, ...) is refused, and so is a path whose
//!   Windows form is longer than [`MAX_WINDOWS_PATH`] characters;
//! - on native Windows the local path is used as it is (without the `\\?\` prefix that
//!   canonicalisation adds), held to the same name rules; on macOS (and Linux, where there
//!   is no Bambu Connect) the local path is used as it is.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

/// The longest Windows path, in UTF-16 units, that a link may name. Windows' own limit
/// without long-path support is 260 including the terminating zero; Hotend keeps a margin.
pub const MAX_WINDOWS_PATH: usize = 240;

/// Where WSL mounts the Windows drives (its default `automount` root).
pub const WSL_DRIVES_ROOT: &str = "/mnt";

/// The environment variable that names the host kind for tests and the development tools:
/// `wsl`, `windows`, `macos` or `linux`. Any other value is ignored.
pub const HOST_ENV: &str = "HOTEND_HANDOFF_HOST";

/// Where Hotend runs, as far as the hand-off is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    /// Linux inside WSL, with Bambu Connect on the Windows side.
    Wsl,
    /// Native Windows.
    Windows,
    /// macOS.
    MacOs,
    /// Linux (not WSL), where there is no Bambu Connect: the local path is used.
    Linux,
}

impl HostKind {
    /// The host this process runs on. `HOTEND_HANDOFF_HOST` wins when it names a kind;
    /// otherwise Windows and macOS are known from the build, and Linux is WSL when
    /// `/proc/sys/kernel/osrelease` says "microsoft" or `WSL_DISTRO_NAME` is set.
    pub fn detect() -> Self {
        if let Some(named) = std::env::var(HOST_ENV)
            .ok()
            .and_then(|value| Self::named(&value))
        {
            return named;
        }
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::from_signals(
                std::fs::read_to_string("/proc/sys/kernel/osrelease")
                    .ok()
                    .as_deref(),
                std::env::var("WSL_DISTRO_NAME").ok().as_deref(),
            )
        }
    }

    /// Linux's answer from its two signals: the kernel release text and `WSL_DISTRO_NAME`.
    pub fn from_signals(osrelease: Option<&str>, wsl_distro: Option<&str>) -> Self {
        let kernel = osrelease.is_some_and(|text| text.to_ascii_lowercase().contains("microsoft"));
        let distro = wsl_distro.is_some_and(|name| !name.trim().is_empty());
        if kernel || distro {
            Self::Wsl
        } else {
            Self::Linux
        }
    }

    /// The kind a name stands for: `wsl`, `windows`, `macos` or `linux`.
    pub fn named(name: &str) -> Option<Self> {
        match name.trim() {
            "wsl" => Some(Self::Wsl),
            "windows" => Some(Self::Windows),
            "macos" => Some(Self::MacOs),
            "linux" => Some(Self::Linux),
            _ => None,
        }
    }

    /// The name of [`HostKind::named`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wsl => "wsl",
            Self::Windows => "windows",
            Self::MacOs => "macos",
            Self::Linux => "linux",
        }
    }
}

/// Why a hand-off folder or a placed file cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandOffPathError {
    /// The folder does not exist (or cannot be read).
    #[error("the hand-off folder `{0}` does not exist or cannot be read")]
    Missing(String),
    /// It is not a directory.
    #[error("`{0}` is not a folder")]
    NotAFolder(String),
    /// Under WSL, the folder is not on a Windows drive.
    #[error(
        "Bambu Connect runs on Windows, so the hand-off folder must be a folder on a Windows \
         drive (under /mnt/<drive letter>/, for example /mnt/d/Models/Hotend); `{0}` is not. \
         Choose a Windows drive folder for the Bambu Connect hand-off in Settings"
    )]
    NotOnWindowsDrive(String),
    /// On native Windows, the path is not a path on a drive letter.
    #[error("`{0}` is not a path on a drive (such as D:\\Models)")]
    NotADrivePath(String),
    /// The path holds `..` or `.`.
    #[error("the path `{0}` holds `..` or `.`")]
    Dots(String),
    /// A name Windows does not accept.
    #[error("the name `{name}` cannot be used on Windows: {why}")]
    BadName { name: String, why: &'static str },
    /// The Windows form is too long.
    #[error("the Windows path is {0} characters long; at most {MAX_WINDOWS_PATH} are allowed")]
    TooLong(usize),
    /// The placed file is not inside the chosen folder (a link that leaves it, or another
    /// file).
    #[error("`{0}` is not a file directly inside the hand-off folder")]
    OutsideFolder(String),
    /// The path is not valid text.
    #[error("the path is not valid text")]
    NotText,
    /// The folder's Windows path holds a comma. Windows explorer, which opens a project
    /// placed there, takes a comma in its argument as the end of the path and would open
    /// something else.
    #[error(
        "the hand-off folder's Windows path `{0}` holds a comma, and Windows explorer, which \
         opens what is handed off, reads a comma as the end of the path. Choose a folder whose \
         path has no comma"
    )]
    Comma(String),
}

/// At most 200 characters of a path, for a message.
fn shown(path: &Path) -> String {
    path.to_string_lossy().chars().take(200).collect()
}

/// Whether Windows accepts `name` as one file or folder name: not empty, none of
/// `<>:"/\|?*` and no control character, no trailing dot or space, and not a reserved
/// device name (`CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9`, also with an
/// extension, in any case).
pub fn check_windows_name(name: &str) -> Result<(), HandOffPathError> {
    let bad = |why| {
        Err(HandOffPathError::BadName {
            name: name.chars().take(80).collect(),
            why,
        })
    };
    if name.is_empty() {
        return bad("it is empty");
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
    }) {
        return bad("it holds a character Windows forbids in names");
    }
    if name.ends_with(['.', ' ']) {
        return bad("it ends with a dot or a space");
    }
    if name == "." || name == ".." {
        return bad("it is `.` or `..`");
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end()
        .to_uppercase();
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix).is_some_and(|rest| {
            matches!(
                rest,
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
            ) || matches!(rest, "\u{00B9}" | "\u{00B2}" | "\u{00B3}")
        })
    };
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || numbered("COM")
        || numbered("LPT")
    {
        return bad("it is a reserved device name");
    }
    Ok(())
}

fn check_length(windows: &str) -> Result<(), HandOffPathError> {
    let length = windows.encode_utf16().count();
    if length > MAX_WINDOWS_PATH {
        return Err(HandOffPathError::TooLong(length));
    }
    Ok(())
}

/// The Windows form of a WSL path on a Windows drive: `/mnt/d/Models/a.gcode.3mf` is
/// `D:\Models\a.gcode.3mf`. The path must be absolute, start with `/mnt/<one ASCII
/// letter>/`, have at least one name after the drive letter, hold no `..` or `.`, and
/// every name must pass [`check_windows_name`]; the result is at most
/// [`MAX_WINDOWS_PATH`] characters long. Pure: nothing is read or run.
pub fn wsl_to_windows(path: &Path) -> Result<String, HandOffPathError> {
    wsl_to_windows_under(Path::new(WSL_DRIVES_ROOT), path)
}

/// [`wsl_to_windows`] for drives mounted under `root` instead of `/mnt` (WSL's
/// `automount` root can be set in `wsl.conf`; the tests use a folder of their own).
pub fn wsl_to_windows_under(root: &Path, path: &Path) -> Result<String, HandOffPathError> {
    let not_on_drive = || HandOffPathError::NotOnWindowsDrive(shown(path));
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(HandOffPathError::Dots(shown(path)));
    }
    if !root.is_absolute() || !path.is_absolute() {
        return Err(not_on_drive());
    }
    let rest = path.strip_prefix(root).map_err(|_| not_on_drive())?;
    let mut components = rest.components();
    let letter = match components.next() {
        Some(Component::Normal(letter)) => letter.to_str().ok_or(HandOffPathError::NotText)?,
        _ => return Err(not_on_drive()),
    };
    let mut drive = letter.chars();
    let letter = match (drive.next(), drive.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => c.to_ascii_uppercase(),
        _ => return Err(not_on_drive()),
    };
    let mut names = Vec::new();
    for component in components {
        match component {
            Component::Normal(name) => {
                let name = name.to_str().ok_or(HandOffPathError::NotText)?;
                check_windows_name(name)?;
                names.push(name);
            }
            _ => return Err(not_on_drive()),
        }
    }
    if names.is_empty() {
        return Err(not_on_drive());
    }
    let windows = format!("{letter}:\\{}", names.join("\\"));
    check_length(&windows)?;
    Ok(windows)
}

/// A native Windows path as text, held to the same rules: `D:\Models\a.gcode.3mf`, or the
/// same with the `\\?\` prefix of a canonical path (which is removed). A network path or
/// a device path is refused. Pure, and written on text so that it is tested everywhere.
pub fn windows_local(text: &str) -> Result<String, HandOffPathError> {
    let not_drive = || HandOffPathError::NotADrivePath(text.chars().take(200).collect());
    let plain = text.strip_prefix(r"\\?\").unwrap_or(text);
    let mut chars = plain.chars();
    let (Some(letter), Some(':'), Some('\\')) = (chars.next(), chars.next(), chars.next()) else {
        return Err(not_drive());
    };
    if !letter.is_ascii_alphabetic() {
        return Err(not_drive());
    }
    let rest = &plain[3..];
    let mut names = Vec::new();
    for name in rest.split('\\') {
        if name == ".." || name == "." {
            return Err(HandOffPathError::Dots(text.chars().take(200).collect()));
        }
        check_windows_name(name)?;
        names.push(name);
    }
    if names.is_empty() {
        return Err(not_drive());
    }
    let windows = format!("{}:\\{}", letter.to_ascii_uppercase(), names.join("\\"));
    check_length(&windows)?;
    Ok(windows)
}

/// A placed file, resolved inside its hand-off folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedPath {
    /// The path as Hotend's process sees it (canonical).
    pub local: PathBuf,
    /// The path the link names: the Windows form under WSL and on Windows, the local path
    /// elsewhere.
    pub link_path: String,
    /// The Windows form, under WSL and on Windows.
    pub windows: Option<String>,
}

/// The folder the person chose for the Bambu Connect hand-off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandOffFolder {
    folder: PathBuf,
    host: HostKind,
    windows: Option<String>,
    /// Where the Windows drives are mounted, under WSL.
    drives: PathBuf,
}

impl HandOffFolder {
    /// The folder `folder` for a hand-off on `host`: it must exist and be a directory and,
    /// under WSL, be a directory below `/mnt/<drive letter>/` with a valid Windows form.
    /// Under WSL and on Windows its Windows path must hold no comma
    /// ([`HandOffPathError::Comma`]).
    pub fn new(folder: &Path, host: HostKind) -> Result<Self, HandOffPathError> {
        Self::with_drives_root(folder, host, Path::new(WSL_DRIVES_ROOT))
    }

    /// [`HandOffFolder::new`] with the Windows drives of WSL mounted under `drives` instead
    /// of `/mnt`.
    pub fn with_drives_root(
        folder: &Path,
        host: HostKind,
        drives: &Path,
    ) -> Result<Self, HandOffPathError> {
        let canonical = folder
            .canonicalize()
            .map_err(|_| HandOffPathError::Missing(shown(folder)))?;
        if !canonical.is_dir() {
            return Err(HandOffPathError::NotAFolder(shown(folder)));
        }
        let windows =
            match host {
                HostKind::Wsl => Some(wsl_to_windows_under(drives, &canonical).map_err(
                    |error| match error {
                        // The message names the folder the person chose, not where a link led.
                        HandOffPathError::NotOnWindowsDrive(_) => {
                            HandOffPathError::NotOnWindowsDrive(shown(folder))
                        }
                        other => other,
                    },
                )?),
                HostKind::Windows => Some(windows_local(
                    canonical.to_str().ok_or(HandOffPathError::NotText)?,
                )?),
                HostKind::MacOs | HostKind::Linux => {
                    canonical.to_str().ok_or(HandOffPathError::NotText)?;
                    None
                }
            };
        if let Some(windows) = windows.as_deref().filter(|windows| windows.contains(',')) {
            return Err(HandOffPathError::Comma(windows.chars().take(200).collect()));
        }
        Ok(Self {
            folder: canonical,
            host,
            windows,
            drives: drives.to_path_buf(),
        })
    }

    /// The folder, canonical, as Hotend's process sees it.
    pub fn path(&self) -> &Path {
        &self.folder
    }

    /// The folder's Windows form, under WSL and on Windows.
    pub fn windows_path(&self) -> Option<&str> {
        self.windows.as_deref()
    }

    /// The host the folder was chosen for.
    pub fn host(&self) -> HostKind {
        self.host
    }

    /// Resolves `placed`, a file Hotend placed in this folder: its canonical path must be
    /// a regular file directly inside the folder under the same name (a symbolic link that
    /// leads elsewhere is refused), and its path in the link's form is returned.
    pub fn resolve(&self, placed: &Path) -> Result<PlacedPath, HandOffPathError> {
        let outside = || HandOffPathError::OutsideFolder(shown(placed));
        let local = placed.canonicalize().map_err(|_| outside())?;
        if local.parent() != Some(self.folder.as_path())
            || local.file_name() != placed.file_name()
            || !local.is_file()
        {
            return Err(outside());
        }
        let (link_path, windows) = match self.host {
            HostKind::Wsl => {
                let windows = wsl_to_windows_under(&self.drives, &local)?;
                (windows.clone(), Some(windows))
            }
            HostKind::Windows => {
                let windows = windows_local(local.to_str().ok_or(HandOffPathError::NotText)?)?;
                (windows.clone(), Some(windows))
            }
            HostKind::MacOs | HostKind::Linux => (
                local.to_str().ok_or(HandOffPathError::NotText)?.to_owned(),
                None,
            ),
        };
        Ok(PlacedPath {
            local,
            link_path,
            windows,
        })
    }

    /// A project file (`.3mf`, not a sliced `.gcode.3mf`) that Hotend placed in this
    /// folder, for opening with the program the system associates with it ("Open in Bambu
    /// Studio"). Resolved like [`HandOffFolder::resolve`].
    pub fn placed_project(&self, placed: &Path) -> Result<PlacedProject, HandOffPathError> {
        let name = placed
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(HandOffPathError::NotText)?
            .to_ascii_lowercase();
        if !name.ends_with(".3mf") || name.ends_with(".gcode.3mf") {
            return Err(HandOffPathError::BadName {
                name: name.chars().take(80).collect(),
                why: "it is not a project file (.3mf)",
            });
        }
        Ok(PlacedProject(self.resolve(placed)?))
    }
}

/// A project file placed in the hand-off folder, resolved there. Only
/// [`HandOffFolder::placed_project`] makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedProject(PlacedPath);

impl PlacedProject {
    /// The file, resolved.
    pub fn path(&self) -> &PlacedPath {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn wsl_is_known_from_the_kernel_or_the_distribution() {
        let kernel = "5.15.167.4-microsoft-standard-WSL2\n";
        assert_eq!(HostKind::from_signals(Some(kernel), None), HostKind::Wsl);
        assert_eq!(
            HostKind::from_signals(Some("6.8.0-45-generic"), Some("Ubuntu")),
            HostKind::Wsl
        );
        assert_eq!(
            HostKind::from_signals(Some("6.8.0-45-generic"), Some(" ")),
            HostKind::Linux
        );
        assert_eq!(HostKind::from_signals(None, None), HostKind::Linux);
        for kind in [
            HostKind::Wsl,
            HostKind::Windows,
            HostKind::MacOs,
            HostKind::Linux,
        ] {
            assert_eq!(HostKind::named(kind.as_str()), Some(kind));
        }
        assert_eq!(HostKind::named("WSL; rm -rf /"), None);
    }

    #[test]
    fn a_wsl_path_on_a_drive_becomes_its_windows_form() {
        let windows = |path: &str| wsl_to_windows(Path::new(path));
        assert_eq!(
            windows("/mnt/d/Models/Hotend/box-0123.gcode.3mf").unwrap(),
            r"D:\Models\Hotend\box-0123.gcode.3mf"
        );
        assert_eq!(
            windows("/mnt/c/Users/ann marie/Über & co").unwrap(),
            r"C:\Users\ann marie\Über & co"
        );
        // Repeated slashes are one.
        assert_eq!(windows("/mnt/e//a///b").unwrap(), r"E:\a\b");
        // Another automount root.
        assert_eq!(
            wsl_to_windows_under(Path::new("/win"), Path::new("/win/d/x")).unwrap(),
            r"D:\x"
        );
        assert!(wsl_to_windows_under(Path::new("/win"), Path::new("/mnt/d/x")).is_err());
        assert!(wsl_to_windows_under(Path::new("win"), Path::new("/win/d/x")).is_err());
    }

    #[test]
    fn only_a_folder_below_a_drive_letter_is_a_windows_path() {
        for path in [
            "/home/me/handoff",
            "/mnt",
            "/mnt/d",
            "/mnt/d/",
            "/mnt/wsl/x",
            "/mnt/wslg/x",
            "/mnt/dd/x",
            "/mnt/1/x",
            "mnt/d/x",
            "/tmp/mnt/d/x",
            "/media/usb/x",
        ] {
            assert!(
                matches!(
                    wsl_to_windows(Path::new(path)),
                    Err(HandOffPathError::NotOnWindowsDrive(_))
                ),
                "{path}"
            );
        }
        for path in ["/mnt/d/Models/../../etc", "/mnt/d/a/..", "/mnt/d/../c/x"] {
            assert!(
                matches!(
                    wsl_to_windows(Path::new(path)),
                    Err(HandOffPathError::Dots(_))
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn names_windows_forbids_are_refused() {
        for name in [
            "a:b",
            "a<b",
            "a>b",
            "a\"b",
            "a|b",
            "a?b",
            "a*b",
            "a\\b",
            "a\u{1}b",
            "a\nb",
            "end.",
            "end ",
            "CON",
            "con",
            "Con.txt",
            "NUL.gcode.3mf",
            "aux",
            "PRN",
            "COM1",
            "lpt9.x",
            "COM\u{00B9}",
            "CONIN$",
            "",
        ] {
            assert!(check_windows_name(name).is_err(), "{name:?}");
            if !name.is_empty() && !name.contains(['\\', '\n']) {
                let path = format!("/mnt/d/Models/{name}");
                assert!(wsl_to_windows(Path::new(&path)).is_err(), "{path:?}");
            }
        }
        for name in [
            "CONSOLE",
            "COM10",
            "com",
            "box-0123.gcode.3mf",
            "Über & co",
            ".hidden",
            "con-1",
        ] {
            check_windows_name(name).unwrap();
        }
    }

    #[test]
    fn a_windows_path_longer_than_the_limit_is_refused() {
        let ok = format!("/mnt/d/{}", "a".repeat(MAX_WINDOWS_PATH - 3));
        assert_eq!(
            wsl_to_windows(Path::new(&ok)).unwrap().len(),
            MAX_WINDOWS_PATH
        );
        let long = format!("/mnt/d/{}", "a".repeat(MAX_WINDOWS_PATH - 2));
        assert_eq!(
            wsl_to_windows(Path::new(&long)),
            Err(HandOffPathError::TooLong(MAX_WINDOWS_PATH + 1))
        );
        // Counted as Windows counts: in UTF-16 units.
        let wide = format!("/mnt/d/{}", "\u{1F600}".repeat(119));
        assert!(matches!(
            wsl_to_windows(Path::new(&wide)),
            Err(HandOffPathError::TooLong(241))
        ));
    }

    #[test]
    fn a_native_windows_path_is_used_without_its_verbatim_prefix() {
        assert_eq!(
            windows_local(r"\\?\D:\Models\Hotend\a.gcode.3mf").unwrap(),
            r"D:\Models\Hotend\a.gcode.3mf"
        );
        assert_eq!(windows_local(r"c:\x").unwrap(), r"C:\x");
        for refused in [
            r"\\?\UNC\server\share\x",
            r"\\server\share\x",
            r"D:",
            r"D:\",
            r"D:\a\..\b",
            r"D:\a\CON",
            r"D:\a\b:c",
            "/mnt/d/x",
        ] {
            assert!(windows_local(refused).is_err(), "{refused}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_placed_file_must_be_inside_the_folder_under_its_own_name() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("handoff");
        std::fs::create_dir(&folder).unwrap();
        let chosen = HandOffFolder::new(&folder, HostKind::Linux).unwrap();
        assert_eq!(chosen.windows_path(), None);
        let inside = folder.join("a.gcode.3mf");
        std::fs::write(&inside, b"x").unwrap();
        let placed = chosen.resolve(&inside).unwrap();
        assert_eq!(
            placed.local,
            folder.canonicalize().unwrap().join("a.gcode.3mf")
        );
        assert_eq!(placed.link_path, placed.local.to_str().unwrap());
        assert_eq!(placed.windows, None);

        // A link in the folder that leads out of it, or to another file in it.
        let outside = root.path().join("secret.gcode.3mf");
        std::fs::write(&outside, b"y").unwrap();
        let link = folder.join("b.gcode.3mf");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(matches!(
            chosen.resolve(&link),
            Err(HandOffPathError::OutsideFolder(_))
        ));
        let alias = folder.join("c.gcode.3mf");
        std::os::unix::fs::symlink(&inside, &alias).unwrap();
        assert!(matches!(
            chosen.resolve(&alias),
            Err(HandOffPathError::OutsideFolder(_))
        ));
        // A file elsewhere, a path through `..`, and a folder.
        assert!(chosen.resolve(&outside).is_err());
        assert!(chosen.resolve(&folder.join("../secret.gcode.3mf")).is_err());
        let sub = folder.join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert!(chosen.resolve(&sub).is_err());
        assert!(chosen.resolve(&folder.join("missing")).is_err());

        // A folder that is missing or is a file.
        assert!(matches!(
            HandOffFolder::new(&folder.join("missing"), HostKind::Linux),
            Err(HandOffPathError::Missing(_))
        ));
        assert!(matches!(
            HandOffFolder::new(&inside, HostKind::Linux),
            Err(HandOffPathError::NotAFolder(_))
        ));
        // Project files only for opening with their program.
        let project = folder.join("box.3mf");
        std::fs::write(&project, b"p").unwrap();
        assert!(chosen.placed_project(&project).is_ok());
        assert!(chosen.placed_project(&inside).is_err());
    }

    /// A folder whose Windows path holds a comma is refused under WSL (explorer.exe would
    /// cut the path at the comma); elsewhere there is no Windows path and no explorer.
    #[cfg(unix)]
    #[test]
    fn a_hand_off_folder_with_a_comma_in_its_windows_path_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let drives = root.path().canonicalize().unwrap();
        let plain = drives.join("d").join("Models");
        let comma = drives.join("d").join("Models, old");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(&comma).unwrap();
        let chosen = HandOffFolder::with_drives_root(&plain, HostKind::Wsl, &drives).unwrap();
        assert_eq!(chosen.windows_path(), Some(r"D:\Models"));
        let error = HandOffFolder::with_drives_root(&comma, HostKind::Wsl, &drives).unwrap_err();
        assert_eq!(error, HandOffPathError::Comma(r"D:\Models, old".to_owned()));
        assert!(error.to_string().contains("comma"), "{error}");
        // A comma in a folder below a drive, too.
        let below = comma.join("inbox");
        std::fs::create_dir_all(&below).unwrap();
        assert!(matches!(
            HandOffFolder::with_drives_root(&below, HostKind::Wsl, &drives),
            Err(HandOffPathError::Comma(_))
        ));
        assert!(HandOffFolder::new(&comma, HostKind::Linux).is_ok());
    }

    /// Under WSL a folder off the Windows drives is refused with the sentence that says
    /// what to do, also when a link inside a drive folder leads off it.
    #[cfg(unix)]
    #[test]
    fn under_wsl_a_folder_off_the_windows_drives_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let error = HandOffFolder::new(root.path(), HostKind::Wsl).unwrap_err();
        assert!(matches!(error, HandOffPathError::NotOnWindowsDrive(_)));
        let text = error.to_string();
        assert!(
            text.contains("Windows drive") && text.contains("in Settings"),
            "{text}"
        );
    }
}
