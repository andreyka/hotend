//! Finds installed slicers and reads their versions.
//!
//! Guarantees:
//! - a slicer is never downloaded or bundled; only executables already on the machine are
//!   considered, in this order: the explicit override (settings, then the environment
//!   variable), the `PATH`, the usual install locations of the operating system;
//! - an explicit override is final: when it is set, nothing else is searched for that slicer;
//! - the version is read by running `<executable> --help` **inside the sandbox** in an empty
//!   temporary job folder. No slicer code runs unconfined, and no user file is involved. A
//!   program that prints nothing has an unknown version here (measured: the Windows build
//!   of Bambu Studio 02.09.01.70 is a launcher without a console and prints nothing at
//!   all). Its version is then taken from the first line of the file it slices, and the
//!   version policy is applied to that before the file is used ([`crate::Slicer`]);
//! - the fake slicer is used only through an explicit override and is always labelled.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use hotend_sandbox::{Limits, Profile};

use crate::error::SlicerError;
use crate::run::run_sandboxed;
use crate::version::{SlicerKind, SlicerVersion, Verdict, VersionPolicy};

/// Banner prefix of the fake slicer's `--help` output.
pub const FAKE_BANNER: &str = "HotendFakeSlicer-";

/// Wall-clock limit of a version probe.
const PROBE_WALL_SECONDS: u64 = 30;

/// The operating systems whose install locations are known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    Windows,
    MacOs,
}

impl Os {
    /// The system this build runs on; `None` for any other.
    pub fn current() -> Option<Self> {
        match std::env::consts::OS {
            "linux" => Some(Self::Linux),
            "windows" => Some(Self::Windows),
            "macos" => Some(Self::MacOs),
            _ => None,
        }
    }

    fn path_separator(self) -> char {
        match self {
            Self::Windows => ';',
            Self::Linux | Self::MacOs => ':',
        }
    }

    /// File names an executable of this slicer may have in a `bin` folder or on the `PATH`.
    fn executable_names(self, kind: SlicerKind) -> &'static [&'static str] {
        match (self, kind) {
            (Self::Windows, SlicerKind::BambuStudio) => &["bambu-studio.exe"],
            // The console build prints to the terminal; the GUI build does not.
            (Self::Windows, SlicerKind::PrusaSlicer) => {
                &["prusa-slicer-console.exe", "prusa-slicer.exe"]
            }
            (_, SlicerKind::BambuStudio) => &["bambu-studio", "BambuStudio"],
            (_, SlicerKind::PrusaSlicer) => &["prusa-slicer", "PrusaSlicer"],
        }
    }
}

/// How an executable was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// Named explicitly in the settings or the environment.
    Override,
    /// On the `PATH`.
    SearchPath,
    /// In a usual install location.
    KnownLocation,
}

/// A path that may hold a slicer. Nothing has been checked yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: SlicerKind,
    pub path: PathBuf,
    pub found: Found,
}

/// The paths where a slicer of `kind` is looked for on `os`, most specific first.
///
/// `setting` is the explicit path from the application's settings; `env` looks up an
/// environment variable. This function touches neither the file system nor the real
/// environment, so the search order of every system can be tested anywhere.
pub fn candidates(
    kind: SlicerKind,
    os: Os,
    setting: Option<&Path>,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<Candidate> {
    let explicit = setting.map(Path::to_path_buf).or_else(|| {
        env(kind.override_env())
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    });
    if let Some(path) = explicit {
        // On macOS an application is a folder, and the folder is what the system's file
        // dialog returns when a person picks the application. The program is inside it.
        let bundle = os == Os::MacOs
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"));
        let programs = if bundle {
            os.executable_names(kind)
                .iter()
                .map(|name| path.join("Contents").join("MacOS").join(name))
                .collect()
        } else {
            vec![path]
        };
        return programs
            .into_iter()
            .map(|path| Candidate {
                kind,
                path,
                found: Found::Override,
            })
            .collect();
    }

    let mut list: Vec<Candidate> = Vec::new();
    let mut add = |path: PathBuf, found: Found| {
        if path.is_absolute_on(os) && !list.iter().any(|known| known.path == path) {
            list.push(Candidate { kind, path, found });
        }
    };
    let names = os.executable_names(kind);

    if let Some(path) = env("PATH") {
        let path = path.to_string_lossy().into_owned();
        for dir in path
            .split(os.path_separator())
            .filter(|dir| !dir.is_empty())
        {
            for name in names {
                add(Path::new(dir).join(name), Found::SearchPath);
            }
        }
    }

    let dir = |name: &str| env(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    match os {
        Os::Linux => {
            let mut folders = vec![PathBuf::from("/usr/bin"), PathBuf::from("/usr/local/bin")];
            folders.extend(
                match kind {
                    SlicerKind::BambuStudio => ["/opt/BambuStudio/bin", "/opt/bambu-studio/bin"],
                    SlicerKind::PrusaSlicer => ["/opt/PrusaSlicer/bin", "/opt/prusa-slicer/bin"],
                }
                .map(PathBuf::from),
            );
            if let Some(home) = dir("HOME") {
                folders.push(home.join(".local").join("bin"));
            }
            for folder in folders {
                for name in names {
                    add(folder.join(name), Found::KnownLocation);
                }
            }
        }
        Os::MacOs => {
            let bundles: &[&str] = match kind {
                SlicerKind::BambuStudio => &["BambuStudio.app/Contents/MacOS/BambuStudio"],
                SlicerKind::PrusaSlicer => &[
                    "PrusaSlicer.app/Contents/MacOS/PrusaSlicer",
                    "Original Prusa Drivers/PrusaSlicer.app/Contents/MacOS/PrusaSlicer",
                ],
            };
            let mut roots = vec![PathBuf::from("/Applications")];
            if let Some(home) = dir("HOME") {
                roots.push(home.join("Applications"));
            }
            for root in roots {
                for bundle in bundles {
                    add(root.join(bundle), Found::KnownLocation);
                }
            }
        }
        Os::Windows => {
            let folder: &[&str] = match kind {
                SlicerKind::BambuStudio => &["Bambu Studio"],
                SlicerKind::PrusaSlicer => &["Prusa3D", "PrusaSlicer"],
            };
            let mut roots: Vec<PathBuf> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
                .into_iter()
                .filter_map(dir)
                .collect();
            if let Some(local) = dir("LOCALAPPDATA") {
                roots.push(local.join("Programs"));
            }
            for root in roots {
                let install = folder.iter().fold(root, |path, part| path.join(part));
                for name in names {
                    add(install.join(name), Found::KnownLocation);
                }
            }
        }
    }
    list
}

/// `Path::is_absolute` answers for the system the code runs on; the search order is also
/// computed for other systems, so this decides by the target's own rules.
trait AbsoluteOn {
    fn is_absolute_on(&self, os: Os) -> bool;
}

impl AbsoluteOn for PathBuf {
    fn is_absolute_on(&self, os: Os) -> bool {
        let text = self.to_string_lossy();
        match os {
            Os::Linux | Os::MacOs => text.starts_with('/'),
            Os::Windows => {
                let bytes = text.as_bytes();
                text.starts_with("\\\\")
                    || (bytes.len() > 2
                        && bytes[0].is_ascii_alphabetic()
                        && bytes[1] == b':'
                        && matches!(bytes[2], b'\\' | b'/'))
            }
        }
    }
}

/// A slicer executable that exists, with what could be learned about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub kind: SlicerKind,
    pub path: PathBuf,
    pub found: Found,
    /// The version the program printed; `None` when it could not be read.
    pub version: Option<SlicerVersion>,
    /// `true` for Hotend's fake slicer, whose output is a placeholder.
    pub fake: bool,
    /// Folders the program needs to read besides the system's own: its install location.
    pub read_only: Vec<PathBuf>,
    /// What the version policy says about using it.
    pub verdict: Verdict,
    /// Why the version is unknown, if it is.
    pub note: Option<String>,
}

impl Installation {
    /// Name for [`crate::SlicerInfo`].
    pub fn display_name(&self) -> String {
        if self.fake {
            format!("Hotend fake slicer ({} command line)", self.kind)
        } else {
            self.kind.display_name().to_owned()
        }
    }

    /// Version for [`crate::SlicerInfo`].
    pub fn version_text(&self) -> String {
        self.version
            .as_ref()
            .map_or_else(|| "unknown".to_owned(), |v| v.as_str().to_owned())
    }
}

/// Finds the installed slicers of `kind` on this machine and probes each one.
///
/// A missing explicit override is reported as an error rather than skipped, because the
/// user asked for exactly that program.
pub fn detect(
    kind: SlicerKind,
    setting: Option<&Path>,
    policy: &VersionPolicy,
) -> Result<Vec<Installation>, SlicerError> {
    let Some(os) = Os::current() else {
        return Ok(Vec::new());
    };
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let candidates = candidates(kind, os, setting, &|name| std::env::var_os(name));
    Ok(existing(kind, candidates)?
        .iter()
        .map(|candidate| probe(candidate, policy, home.as_deref().map(Path::new)))
        .collect())
}

/// The candidates that are files, each file once. An explicit choice none of whose
/// candidates exists is an error (an application bundle on macOS gives one candidate per
/// name its program may have; one of them is enough).
fn existing(kind: SlicerKind, candidates: Vec<Candidate>) -> Result<Vec<Candidate>, SlicerError> {
    let mut found: Vec<(PathBuf, Candidate)> = Vec::new();
    let mut missing_choice = None;
    for candidate in candidates {
        let is_file = std::fs::metadata(&candidate.path).is_ok_and(|meta| meta.is_file());
        if !is_file || !candidate.path.is_absolute() {
            if candidate.found == Found::Override {
                missing_choice.get_or_insert(candidate.path);
            }
            continue;
        }
        // Two names for one file (a symlink in /usr/local/bin, say) are one installation.
        let real =
            std::fs::canonicalize(&candidate.path).unwrap_or_else(|_| candidate.path.clone());
        if found.iter().any(|known| known.0 == real) {
            continue;
        }
        found.push((real, candidate));
    }
    if let (true, Some(path)) = (found.is_empty(), missing_choice) {
        return Err(SlicerError::InvalidJob(format!(
            "the configured {} executable {} is not an existing file with an absolute path",
            kind.display_name(),
            path.display()
        )));
    }
    Ok(found.into_iter().map(|(_, candidate)| candidate).collect())
}

/// Reads the version of one candidate by running `--help` in the sandbox, and applies the
/// version policy.
pub fn probe(candidate: &Candidate, policy: &VersionPolicy, home: Option<&Path>) -> Installation {
    let kind = candidate.kind;
    let mut installation = Installation {
        kind,
        path: candidate.path.clone(),
        found: candidate.found,
        version: None,
        fake: false,
        read_only: Vec::new(),
        verdict: Verdict::Allow,
        note: None,
    };
    let shared_folder = match install_dirs(&candidate.path, home) {
        Ok(view) => {
            installation.read_only = view.read_only;
            view.shared_folder
        }
        Err(reason) => {
            installation.verdict = Verdict::Refuse(reason);
            return installation;
        }
    };
    if Os::current() != Some(Os::Windows) && is_windows_program(&candidate.path) {
        installation.verdict = Verdict::Refuse(format!(
            "{} is a Windows program; it cannot run inside the sandbox here (under WSL it would \
             need a socket to Windows, which the sandbox denies). Install the {} build for \
             this system",
            candidate.path.display(),
            kind.display_name()
        ));
        return installation;
    }
    match probe_banner(&installation) {
        Ok(text) => match parse_banner(&text) {
            Some(Banner::Fake(version)) if candidate.found == Found::Override => {
                installation.fake = true;
                installation.version = Some(version);
                installation.verdict = Verdict::Warn(
                    "this is Hotend's fake slicer: the sliced file is a placeholder with \
                     estimated numbers and must not be printed"
                        .to_owned(),
                );
                return installation;
            }
            Some(Banner::Fake(_)) => {
                installation.verdict = Verdict::Refuse(
                    "the fake slicer is only used when it is configured explicitly".to_owned(),
                );
                return installation;
            }
            Some(Banner::Real(banner_kind, version)) if banner_kind == kind => {
                installation.version = Some(version);
            }
            Some(Banner::Real(banner_kind, _)) => {
                installation.verdict = Verdict::Refuse(format!(
                    "{} is {}, not {}",
                    candidate.path.display(),
                    banner_kind.display_name(),
                    kind.display_name()
                ));
                return installation;
            }
            None => {
                installation.note = Some(no_version_note(&text));
            }
        },
        Err(error) => installation.note = Some(error.to_string()),
    }
    // A slicer that needs files next to it cannot start from a shared folder: say why.
    if let (Some(folder), Some(note)) = (shared_folder, installation.note.as_mut()) {
        note.push_str(&format!(
            "; the slicer may read only its executable because {} is shared with other \
             files. If it needs files next to it, move it into a folder of its own",
            folder.display()
        ));
    }
    // A program the loader cannot start is not usable at any version: say why instead of
    // letting the version policy's verdict for an unknown version stand.
    if installation.version.is_none()
        && let Some(note) = &installation.note
        && note.starts_with(CANNOT_START)
    {
        installation.verdict = Verdict::Refuse(note.clone());
        return installation;
    }
    installation.verdict = policy.verdict(kind, installation.version.as_ref());
    installation
}

/// `true` when the file starts like a Windows executable (the `MZ` header). Reads 2 bytes.
fn is_windows_program(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok()
        && &magic == b"MZ"
}

/// Start of the note of a program the dynamic loader could not start.
const CANNOT_START: &str = "it cannot start because the shared";

/// Folders directly in the home folder that hold the user's own files. A slicer found in
/// one of them is given the executable alone, never the folder.
const USER_FOLDERS: [&str; 12] = [
    "Downloads",
    "Desktop",
    "Documents",
    "Pictures",
    "Music",
    "Videos",
    "Movies",
    "Public",
    "Templates",
    "bin",
    ".cache",
    ".config",
];

/// What a slicer installed outside the system paths may read.
#[derive(Debug, PartialEq, Eq)]
struct InstallView {
    /// Folders, or the executable alone.
    read_only: Vec<PathBuf>,
    /// Set when only the executable is readable: the folder it lies in, which is shared
    /// with other things.
    shared_folder: Option<PathBuf>,
}

/// The paths a slicer installed outside the system paths needs to read.
///
/// An installation has a folder of its own: the folder of the executable and, when that
/// folder is called `bin` or `MacOS`, the install prefix above it. Those are granted as a
/// whole. A folder that is shared with other things is not: an executable that lies in
/// the user's Downloads, Desktop or Documents, in `~/bin` or `~/.local/bin`, or in the
/// temporary folder gets read access to itself and nothing next to it. Locations that
/// would expose the home folder itself are refused.
fn install_dirs(executable: &Path, home: Option<&Path>) -> Result<InstallView, String> {
    let resolve = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let real = resolve(executable);
    let home = home.map(resolve);
    let temp = resolve(&std::env::temp_dir());
    let too_wide = |dir: &Path| {
        dir.parent().is_none()
            || home.as_deref().is_some_and(|home| {
                dir == home || home.starts_with(dir) || dir == home.join(".local")
            })
    };
    let shared = |dir: &Path| {
        dir == temp
            || home.as_deref().is_some_and(|home| {
                dir == home.join(".local/bin")
                    || USER_FOLDERS.iter().any(|name| dir == home.join(name))
            })
    };
    let Some(folder) = real.parent() else {
        return Err("the executable has no parent folder".into());
    };
    if too_wide(folder) {
        return Err(format!(
            "{} lies directly in the home folder or at the top of a drive; the slicer would be \
             able to read everything there. Move it into a folder of its own",
            executable.display()
        ));
    }
    if shared(folder) {
        return Ok(InstallView {
            read_only: vec![real.clone()],
            shared_folder: Some(folder.to_path_buf()),
        });
    }
    let mut dirs = vec![folder.to_path_buf()];
    let named = |name: &str| folder.file_name() == Some(OsStr::new(name));
    if (named("bin") || named("MacOS"))
        && let Some(prefix) = folder.parent()
    {
        // `X.app/Contents/MacOS`: the bundle is two levels up.
        let prefix = match prefix.parent() {
            Some(bundle)
                if named("MacOS") && prefix.file_name() == Some(OsStr::new("Contents")) =>
            {
                bundle
            }
            _ => prefix,
        };
        if !too_wide(prefix) && !shared(prefix) {
            dirs.push(prefix.to_path_buf());
        }
    }
    Ok(InstallView {
        read_only: dirs,
        shared_folder: None,
    })
}

/// Runs `<executable> --help` in the sandbox with an empty job folder and returns what it
/// printed.
fn probe_banner(installation: &Installation) -> Result<String, SlicerError> {
    let job = tempfile::Builder::new()
        .prefix("hotend-slicer-probe-")
        .tempdir()
        .map_err(|e| SlicerError::io("create a probe folder in", std::env::temp_dir(), e))?;
    let job_dir =
        std::fs::canonicalize(job.path()).map_err(|e| SlicerError::io("resolve", job.path(), e))?;
    let limits = Limits {
        wall_seconds: PROBE_WALL_SECONDS,
        ..Profile::Slicer.default_limits()
    };
    let output = run_sandboxed(
        &job_dir,
        &installation.read_only,
        limits,
        &installation.path,
        &[OsString::from("--help")],
    )?;
    match output.code {
        Some(125) => Err(SlicerError::Sandbox(
            hotend_sandbox::SandboxError::Unavailable(crate::job::untrusted_text(
                &String::from_utf8_lossy(&output.stderr),
                400,
            )),
        )),
        Some(124) => Err(SlicerError::Timeout {
            seconds: PROBE_WALL_SECONDS,
        }),
        _ => Ok(output.text()),
    }
}

/// Why no version could be read from `--help` output, in plain words.
fn no_version_note(text: &str) -> String {
    match missing_libraries(text).as_slice() {
        [] => "its --help output does not state a version".to_owned(),
        [one] => format!(
            "{CANNOT_START} library {one} is missing on this machine; install the system \
             package that provides it"
        ),
        many => format!(
            "{CANNOT_START} libraries {} are missing on this machine; install the system \
             packages that provide them",
            many.join(", ")
        ),
    }
}

/// The shared libraries the Linux dynamic loader said it could not find, in the order it
/// named them. The loader stops at the first one, so this is usually a single name; the
/// text is untrusted, so only names made of the characters of a library file name are kept.
fn missing_libraries(text: &str) -> Vec<String> {
    const MARKER: &str = "error while loading shared libraries: ";
    let mut missing: Vec<String> = Vec::new();
    for line in text.lines().take(200) {
        let Some((_, rest)) = line.split_once(MARKER) else {
            continue;
        };
        let Some((name, _)) = rest.split_once(": ") else {
            continue;
        };
        let plausible = !name.is_empty()
            && name.len() <= 128
            && name.contains(".so")
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c));
        if plausible && !missing.iter().any(|known| known == name) {
            missing.push(name.to_owned());
        }
    }
    missing
}

#[derive(Debug, PartialEq, Eq)]
enum Banner {
    Real(SlicerKind, SlicerVersion),
    Fake(SlicerVersion),
}

/// Finds the program name and version in `--help` output: the first line that starts with
/// `PrusaSlicer-<version>`, `BambuStudio-<version>` or the fake slicer's banner.
fn parse_banner(text: &str) -> Option<Banner> {
    for line in text.lines().take(200) {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix(FAKE_BANNER) {
            return SlicerVersion::parse(rest).map(Banner::Fake);
        }
        for (prefix, kind) in [
            ("PrusaSlicer-", SlicerKind::PrusaSlicer),
            ("BambuStudio-", SlicerKind::BambuStudio),
        ] {
            if let Some(rest) = line.strip_prefix(prefix)
                && let Some(version) = SlicerVersion::parse(rest)
            {
                return Some(Banner::Real(kind, version));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    fn paths(list: &[Candidate]) -> Vec<String> {
        list.iter()
            .map(|c| c.path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn an_override_is_the_only_candidate_and_the_setting_beats_the_environment() {
        let vars = [
            ("HOTEND_BAMBU_STUDIO", "/opt/custom/bambu-studio"),
            ("PATH", "/usr/bin"),
        ];
        let from_env = candidates(SlicerKind::BambuStudio, Os::Linux, None, &env(&vars));
        assert_eq!(paths(&from_env), ["/opt/custom/bambu-studio"]);
        assert_eq!(from_env[0].found, Found::Override);
        let setting = Path::new("/srv/bs/bambu-studio");
        let from_setting = candidates(
            SlicerKind::BambuStudio,
            Os::Linux,
            Some(setting),
            &env(&vars),
        );
        assert_eq!(paths(&from_setting), ["/srv/bs/bambu-studio"]);
        // The override of one slicer does not affect the other.
        let prusa = candidates(SlicerKind::PrusaSlicer, Os::Linux, None, &env(&vars));
        assert_eq!(prusa[0].found, Found::SearchPath);
        assert_eq!(prusa[0].path, Path::new("/usr/bin/prusa-slicer"));
    }

    #[test]
    fn linux_searches_the_path_then_the_usual_folders() {
        let vars = [
            ("PATH", "/home/u/bin::relative/bin:/usr/bin"),
            ("HOME", "/home/u"),
        ];
        let list = candidates(SlicerKind::PrusaSlicer, Os::Linux, None, &env(&vars));
        assert_eq!(
            paths(&list),
            [
                "/home/u/bin/prusa-slicer",
                "/home/u/bin/PrusaSlicer",
                "/usr/bin/prusa-slicer",
                "/usr/bin/PrusaSlicer",
                "/usr/local/bin/prusa-slicer",
                "/usr/local/bin/PrusaSlicer",
                "/opt/PrusaSlicer/bin/prusa-slicer",
                "/opt/PrusaSlicer/bin/PrusaSlicer",
                "/opt/prusa-slicer/bin/prusa-slicer",
                "/opt/prusa-slicer/bin/PrusaSlicer",
                "/home/u/.local/bin/prusa-slicer",
                "/home/u/.local/bin/PrusaSlicer",
            ]
        );
        assert_eq!(list[3].found, Found::SearchPath);
        assert_eq!(list[4].found, Found::KnownLocation);
        let bambu = candidates(SlicerKind::BambuStudio, Os::Linux, None, &env(&[]));
        assert_eq!(bambu[0].path, Path::new("/usr/bin/bambu-studio"));
        assert!(paths(&bambu).contains(&"/opt/BambuStudio/bin/bambu-studio".to_owned()));
    }

    #[test]
    fn macos_searches_the_application_bundles() {
        let vars = [("PATH", "/usr/local/bin"), ("HOME", "/Users/u")];
        let bambu = candidates(SlicerKind::BambuStudio, Os::MacOs, None, &env(&vars));
        assert_eq!(
            paths(&bambu),
            [
                "/usr/local/bin/bambu-studio",
                "/usr/local/bin/BambuStudio",
                "/Applications/BambuStudio.app/Contents/MacOS/BambuStudio",
                "/Users/u/Applications/BambuStudio.app/Contents/MacOS/BambuStudio",
            ]
        );
        let prusa = candidates(SlicerKind::PrusaSlicer, Os::MacOs, None, &env(&[]));
        assert_eq!(
            paths(&prusa),
            [
                "/Applications/PrusaSlicer.app/Contents/MacOS/PrusaSlicer",
                "/Applications/Original Prusa Drivers/PrusaSlicer.app/Contents/MacOS/PrusaSlicer",
            ]
        );
    }

    #[test]
    fn windows_searches_the_path_and_the_program_folders() {
        let vars = [
            ("PATH", r"C:\Tools;D:\Slicers\Bambu"),
            ("ProgramFiles", r"C:\Program Files"),
            ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
            ("LOCALAPPDATA", r"C:\Users\u\AppData\Local"),
        ];
        let bambu = candidates(SlicerKind::BambuStudio, Os::Windows, None, &env(&vars));
        assert_eq!(
            paths(&bambu),
            [
                "C:/Tools/bambu-studio.exe",
                "D:/Slicers/Bambu/bambu-studio.exe",
                "C:/Program Files/Bambu Studio/bambu-studio.exe",
                "C:/Program Files (x86)/Bambu Studio/bambu-studio.exe",
                "C:/Users/u/AppData/Local/Programs/Bambu Studio/bambu-studio.exe",
            ]
        );
        let prusa = candidates(
            SlicerKind::PrusaSlicer,
            Os::Windows,
            None,
            &env(&vars[1..2]),
        );
        assert_eq!(
            paths(&prusa),
            [
                "C:/Program Files/Prusa3D/PrusaSlicer/prusa-slicer-console.exe",
                "C:/Program Files/Prusa3D/PrusaSlicer/prusa-slicer.exe",
            ]
        );
        let env_override = [("HOTEND_PRUSA_SLICER", r"E:\ps\prusa-slicer-console.exe")];
        let explicit = candidates(
            SlicerKind::PrusaSlicer,
            Os::Windows,
            None,
            &env(&env_override),
        );
        assert_eq!(explicit.len(), 1);
        assert_eq!(explicit[0].found, Found::Override);
    }

    /// On macOS the system's file dialog returns the application's folder, not the
    /// program inside it.
    #[test]
    fn a_macos_application_bundle_chosen_as_the_slicer_means_the_program_inside_it() {
        let none: [(&str, &str); 0] = [];
        let bundle = Path::new("/Users/me/Tools/BambuStudio.app");
        let chosen = candidates(
            SlicerKind::BambuStudio,
            Os::MacOs,
            Some(bundle),
            &env(&none),
        );
        assert_eq!(
            paths(&chosen),
            [
                "/Users/me/Tools/BambuStudio.app/Contents/MacOS/bambu-studio",
                "/Users/me/Tools/BambuStudio.app/Contents/MacOS/BambuStudio",
            ]
        );
        assert!(chosen.iter().all(|c| c.found == Found::Override));
        // Through the environment, with another spelling of the extension, too.
        let vars = [("HOTEND_PRUSA_SLICER", "/Volumes/USB/PrusaSlicer.APP/")];
        let from_env = candidates(SlicerKind::PrusaSlicer, Os::MacOs, None, &env(&vars));
        assert_eq!(
            paths(&from_env),
            [
                "/Volumes/USB/PrusaSlicer.APP/Contents/MacOS/prusa-slicer",
                "/Volumes/USB/PrusaSlicer.APP/Contents/MacOS/PrusaSlicer",
            ]
        );
        // A program chosen directly stays what it is, and so does a folder named `.app`
        // on a system where that means nothing.
        let program = Path::new("/Users/me/Tools/BambuStudio.app/Contents/MacOS/BambuStudio");
        let direct = candidates(
            SlicerKind::BambuStudio,
            Os::MacOs,
            Some(program),
            &env(&none),
        );
        assert_eq!(paths(&direct), [program.to_str().unwrap()]);
        let linux = candidates(
            SlicerKind::BambuStudio,
            Os::Linux,
            Some(bundle),
            &env(&none),
        );
        assert_eq!(paths(&linux), [bundle.to_str().unwrap()]);

        // One of the names existing in the bundle is enough; none is an error that names
        // the place looked in.
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("BambuStudio.app");
        let inside = bundle.join("Contents").join("MacOS");
        std::fs::create_dir_all(&inside).unwrap();
        let chosen = candidates(
            SlicerKind::BambuStudio,
            Os::MacOs,
            Some(&bundle),
            &env(&none),
        );
        let error = existing(SlicerKind::BambuStudio, chosen.clone()).unwrap_err();
        assert!(error.to_string().contains("Contents/MacOS"), "{error}");
        std::fs::write(inside.join("BambuStudio"), b"#!/bin/sh\n").unwrap();
        let found = existing(SlicerKind::BambuStudio, chosen).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, inside.join("BambuStudio"));
        assert_eq!(found[0].found, Found::Override);
    }

    #[test]
    fn banners_are_recognised() {
        let prusa = "PrusaSlicer-2.7.2+UNKNOWN based on Slic3r (with GUI support)\nhttps://x\n";
        assert_eq!(
            parse_banner(prusa),
            Some(Banner::Real(
                SlicerKind::PrusaSlicer,
                SlicerVersion::parse("2.7.2+UNKNOWN").unwrap()
            ))
        );
        let bambu = "\n  BambuStudio-02.05.03.00:\nUsage: bambu-studio [ OPTIONS ]";
        assert!(matches!(
            parse_banner(bambu),
            Some(Banner::Real(SlicerKind::BambuStudio, _))
        ));
        assert!(matches!(
            parse_banner("HotendFakeSlicer-0.1.0 (not a real slicer)"),
            Some(Banner::Fake(_))
        ));
        assert_eq!(
            parse_banner("Usage: something [options]\nPrusaSlicer-next"),
            None
        );
        assert_eq!(parse_banner(""), None);
    }

    #[test]
    fn a_windows_program_is_recognised_and_refused_off_windows() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Bambu Studio");
        std::fs::create_dir_all(&folder).unwrap();
        let exe = folder.join("bambu-studio.exe");
        std::fs::write(&exe, b"MZ\x90\x00 a PE header follows").unwrap();
        let elf = folder.join("bambu-studio");
        std::fs::write(&elf, b"\x7fELF").unwrap();
        assert!(is_windows_program(&exe));
        assert!(!is_windows_program(&elf));
        assert!(!is_windows_program(&folder.join("missing")));

        // Off Windows the probe refuses it before anything is started.
        #[cfg(not(windows))]
        {
            let candidate = Candidate {
                kind: SlicerKind::BambuStudio,
                path: exe,
                found: Found::Override,
            };
            let policy = VersionPolicy::builtin().unwrap();
            let installation = probe(&candidate, &policy, None);
            let Verdict::Refuse(reason) = &installation.verdict else {
                panic!("not refused: {installation:?}");
            };
            assert!(reason.contains("is a Windows program"), "{reason}");
            assert_eq!(installation.version, None);
        }
    }

    #[test]
    fn missing_shared_libraries_are_named() {
        // What the loader printed for the Bambu Studio 02.09.01.70 Ubuntu 24.04 AppImage,
        // unpacked, on an Ubuntu 24.04 without the WebKitGTK 4.1 packages (exit code 127).
        let loader = "\n/home/u/tools/bambu/bin/bambu-studio: error while loading shared \
                      libraries: libwebkit2gtk-4.1.so.0: cannot open shared object file: No \
                      such file or directory\n";
        assert_eq!(missing_libraries(loader), ["libwebkit2gtk-4.1.so.0"]);
        assert_eq!(parse_banner(loader), None);
        assert_eq!(
            no_version_note(loader),
            "it cannot start because the shared library libwebkit2gtk-4.1.so.0 is missing on \
             this machine; install the system package that provides it"
        );
        assert_eq!(
            no_version_note("Usage: x"),
            "its --help output does not state a version"
        );
        let twice =
            format!("{loader}{loader}x: error while loading shared libraries: libz.so.1: no");
        assert_eq!(
            missing_libraries(&twice),
            ["libwebkit2gtk-4.1.so.0", "libz.so.1"]
        );
        // Anything that is not a plain library file name is not repeated to the user.
        assert!(
            missing_libraries("x: error while loading shared libraries: rm -rf /: cannot")
                .is_empty()
        );
        assert!(missing_libraries("Usage: bambu-studio [ OPTIONS ]").is_empty());
    }

    #[test]
    fn install_folders_never_expose_the_home_folder() {
        let base = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(base.path()).unwrap();
        let home = base.join("home").join("u");
        let make = |relative: &str| {
            let path = base.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"").unwrap();
            path
        };
        let home = Some(home.as_path());

        let whole = |dirs: &[PathBuf]| InstallView {
            read_only: dirs.to_vec(),
            shared_folder: None,
        };
        let only_itself = |path: &Path| InstallView {
            read_only: vec![path.to_path_buf()],
            shared_folder: path.parent().map(Path::to_path_buf),
        };

        let opt = make("opt/BambuStudio/bin/bambu-studio");
        assert_eq!(
            install_dirs(&opt, home).unwrap(),
            whole(&[
                base.join("opt/BambuStudio/bin"),
                base.join("opt/BambuStudio")
            ])
        );
        let plain = make("tools/slicer/prusa-slicer");
        assert_eq!(
            install_dirs(&plain, home).unwrap(),
            whole(&[base.join("tools/slicer")])
        );
        let bundle = make("Applications/BambuStudio.app/Contents/MacOS/BambuStudio");
        assert_eq!(
            install_dirs(&bundle, home).unwrap(),
            whole(&[
                base.join("Applications/BambuStudio.app/Contents/MacOS"),
                base.join("Applications/BambuStudio.app")
            ])
        );
        // An installation in a folder of its own below the home folder keeps its folder.
        let own = make("home/u/apps/PrusaSlicer/bin/prusa-slicer");
        assert_eq!(
            install_dirs(&own, home).unwrap(),
            whole(&[
                base.join("home/u/apps/PrusaSlicer/bin"),
                base.join("home/u/apps/PrusaSlicer")
            ])
        );

        // Folders shared with the user's other files are never granted: the slicer gets
        // its executable and nothing that lies next to it.
        for relative in [
            "home/u/Downloads/prusa-slicer",
            "home/u/Desktop/BambuStudio",
            "home/u/Documents/prusa-slicer",
            "home/u/.local/bin/prusa-slicer",
            "home/u/bin/prusa-slicer",
        ] {
            let executable = make(relative);
            std::fs::write(executable.with_file_name("tax-return.pdf"), b"private").unwrap();
            let view = install_dirs(&executable, home).unwrap();
            assert_eq!(view, only_itself(&executable), "{relative}");
            assert!(
                view.read_only.iter().all(|path| path.is_file()),
                "{relative}: a folder was granted"
            );
        }
        // An installation unpacked into Downloads keeps its own folders, not Downloads.
        let unpacked = make("home/u/Downloads/PrusaSlicer/bin/prusa-slicer");
        assert_eq!(
            install_dirs(&unpacked, home).unwrap(),
            whole(&[
                base.join("home/u/Downloads/PrusaSlicer/bin"),
                base.join("home/u/Downloads/PrusaSlicer")
            ])
        );
        let direct = make("home/u/Downloads/bin/prusa-slicer");
        assert_eq!(
            install_dirs(&direct, home).unwrap(),
            whole(&[base.join("home/u/Downloads/bin")])
        );
        // Directly in the home folder, or in a folder that contains it: refused.
        let in_home = make("home/u/prusa-slicer");
        assert!(install_dirs(&in_home, home).is_err());
        let above_home = make("home/prusa-slicer");
        assert!(install_dirs(&above_home, home).is_err());
    }
}
