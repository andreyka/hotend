//! Snapshots: a name for one version of a project, so the maker can take the model as it is
//! at any time, keep iterating, export that state's files later and go back to it.
//!
//! The names live in the store ([`crate::store::Snapshot`]); this module holds what the
//! facade's `snapshot_*` commands do with the files.
//!
//! Guarantees:
//! - Nothing here changes a version, its folder or the current version (going back is
//!   [`crate::Core::version_revert`], which only moves the current version).
//! - The printed file of a snapshot is decided when the snapshot is made: the store records
//!   the newest print job of its version whose file had left Hotend then (it was exported,
//!   handed over to Bambu Connect or sent to the simulated printer) with that file's SHA-256,
//!   or none. Making a snapshot (the person's `snapshot_create` and the agent's
//!   `snapshot_model` alike) copies that exact file, checked against the recorded hash, into
//!   `projects/<id>/snapshots/<snapshot id>/`, where nothing else writes. An export writes
//!   exactly the recorded bytes: the kept copy first, then the places the job's file was;
//!   a later slice or print of the same version never changes what a snapshot exports.
//! - An export writes into a new folder of its own below the folder the person chose, and
//!   refuses before writing anything when that folder (or one whose name differs only in
//!   letter case) already exists. It writes only files of the snapshot, each created new
//!   (never replacing a file, never writing through a link): STEP and 3MF (the version's own
//!   stored file, copied byte for byte and hash checked, where the version stores that
//!   format; `export_run`'s output for a format it does not), the printed file (the recorded
//!   bytes, checked by SHA-256) and `snapshot.json`. Sources are read without following a
//!   link and only when they are plain files.
//! - When the recorded printed file cannot be found anywhere with its hash, the export still
//!   writes the model files and `snapshot.json`; the manifest and the result say "printed
//!   file missing", with the job and the hash that were looked for.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::gateway::{MAX_SLICE_BYTES, SliceSummary};
use crate::import;
use crate::paths::DataDir;
use crate::store::{PendingPrintJob, Snapshot, Store, StoreError};

/// The manifest an export writes next to the files.
pub const MANIFEST: &str = "snapshot.json";
/// The model files an export writes; a printed file never takes one of these names.
const MODEL_FILES: [&str; 2] = ["model.step", "model.3mf"];
/// The ending every printed (sliced) file's name has.
const PRINTED_SUFFIX: &str = ".gcode.3mf";
/// Longest export folder name before the ` (vN)` suffix, in UTF-8 bytes and in UTF-16
/// units (Windows counts the latter).
const MAX_FOLDER_STEM: usize = 200;

/// The sliced file of the print of a snapshot's version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintedFile {
    /// The print job whose file it is: the one recorded when the snapshot was made.
    pub job_id: String,
    /// How the job left: `simulated_printer`, `bambu_connect` or `export_to_folder`.
    pub delivery: Option<String>,
    /// The job's status when the file was looked up.
    pub status: String,
    /// The file's name (ending `.gcode.3mf`).
    pub file_name: String,
    /// SHA-256 of the file: the hash recorded with the snapshot.
    pub sha256: String,
}

/// The recorded printed file of a snapshot, when no copy with its hash was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingPrinted {
    /// The print job recorded when the snapshot was made.
    pub job_id: String,
    /// The SHA-256 that was looked for.
    pub sha256: String,
    /// What happened, in words; starts with "printed file missing".
    pub message: String,
}

/// One file of a snapshot export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotFile {
    /// `step`, `3mf`, `printed` (the sliced file of the print) or `manifest`
    /// (`snapshot.json`).
    pub kind: String,
    /// `copied` (byte for byte from the version's stored file or the print's file, hash
    /// checked), `exported` (made by `export_run`: a format the version does not store) or
    /// `written` (the manifest).
    pub source: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

/// What `snapshot_export` wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotExport {
    pub snapshot: Snapshot,
    /// The folder the files are in: below the folder the person chose.
    pub folder: PathBuf,
    pub files: Vec<SnapshotFile>,
    /// The print whose file was exported, when the snapshot recorded one and it was found.
    pub printed: Option<PrintedFile>,
    /// The print the snapshot recorded, when its file was found nowhere: the export has the
    /// model files and the manifest only.
    #[serde(default)]
    pub printed_missing: Option<MissingPrinted>,
}

/// Where a printed file of a snapshot is, if anywhere.
#[derive(Debug)]
pub(crate) enum Printed {
    /// The snapshot recorded no print: its version had not been printed when it was made.
    Never,
    /// A file with the recorded hash (normally the copy kept with the snapshot), and what
    /// it is.
    Kept { path: PathBuf, file: PrintedFile },
    /// The snapshot recorded a print, and no file with its hash was found.
    Missing(MissingPrinted),
}

/// The folder that keeps a snapshot's printed file.
pub(crate) fn kept_dir(data: &DataDir, snapshot: &Snapshot) -> Result<PathBuf, StoreError> {
    Ok(data
        .project_dir(&snapshot.project_id)?
        .join("snapshots")
        .join(&snapshot.id))
}

/// Opens `path` for reading without following a link at its last component, and only when
/// it is a plain file.
pub(crate) fn open_regular(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: open a link itself, which is then not a plain file.
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(std::io::Error::other(format!(
            "{} is not a plain file",
            path.display()
        )));
    }
    Ok(file)
}

/// SHA-256 (lowercase hex) of the plain file at `path`, refusing one over `max_bytes`.
pub(crate) fn hash_regular(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    let mut file = open_regular(path)?;
    if file.metadata()?.len() > max_bytes {
        return Err(std::io::Error::other(format!(
            "{} is larger than {max_bytes} bytes",
            path.display()
        )));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(crate::util::hex(&hasher.finalize()))
}

/// A plain file (not a link) at `path` whose SHA-256 is `sha256`.
fn has_hash(path: &Path, sha256: &str) -> bool {
    hash_regular(path, MAX_SLICE_BYTES).is_ok_and(|hash| hash.eq_ignore_ascii_case(sha256))
}

/// Copies the plain file `source` (never through a link) to a new file `target`; fails when
/// anything is at `target` already. Returns the number of bytes copied.
pub(crate) fn copy_new(source: &Path, target: &Path) -> std::io::Result<u64> {
    let mut from = open_regular(source)?;
    let mut to = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let copied = std::io::copy(&mut from, &mut to).and_then(|bytes| to.sync_all().map(|()| bytes));
    if copied.is_err() {
        drop(to);
        let _ = std::fs::remove_file(target);
    }
    copied
}

/// Writes `bytes` to a new file `target`; fails when anything is at `target` already.
pub(crate) fn write_new(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut to = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let written = to.write_all(bytes).and_then(|()| to.sync_all());
    if written.is_err() {
        drop(to);
        let _ = std::fs::remove_file(target);
    }
    written
}

/// The places the file of `job` may still be, in the order they are tried: where the
/// delivery placed it (the hand-off or export folder: the name the person saw), then the
/// version's slice folder. Every candidate is used only with the recorded hash.
fn candidates(data: &DataDir, store: &Store, job: &PendingPrintJob) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Some(path) = job.detail.get("path").and_then(|p| p.as_str()) {
        found.push(PathBuf::from(path));
    }
    let summary = store
        .slice(&job.project_id, job.version, &job.profile_hash)
        .ok()
        .flatten()
        .and_then(|record| serde_json::from_value::<SliceSummary>(record.summary).ok());
    if let (Some(summary), Ok(dir)) = (summary, data.version_dir(&job.project_id, job.version))
        && let Ok(path) = import::resolve_inside(
            &dir.join("slice").join(&job.profile_hash),
            &summary.output,
            MAX_SLICE_BYTES,
        )
    {
        found.push(path);
    }
    found
}

/// The safe name of a printed file with SHA-256 `sha256`: `printed-<first 16 hex>.gcode.3mf`.
fn safe_name(sha256: &str) -> String {
    let hex: String = sha256
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(16)
        .collect::<String>()
        .to_ascii_lowercase();
    format!("printed-{hex}{PRINTED_SUFFIX}")
}

/// The name a printed file is kept and exported under: the found file's own name when it is
/// a plain one ending `.gcode.3mf` (in any letter case) that no other export file has, else
/// [`safe_name`].
fn file_name_of(path: &Path, sha256: &str) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            lower.len() > PRINTED_SUFFIX.len()
                && lower.ends_with(PRINTED_SUFFIX)
                && name.len() <= 200
                && lower != MANIFEST
                && !MODEL_FILES.contains(&lower.as_str())
                && !name.starts_with('.')
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._- ()".contains(c))
        })
        .map_or_else(|| safe_name(sha256), str::to_owned)
}

/// The words of a missing printed file.
fn missing(snapshot: &Snapshot, job_id: &str, sha256: &str) -> MissingPrinted {
    MissingPrinted {
        job_id: job_id.to_owned(),
        sha256: sha256.to_owned(),
        message: format!(
            "printed file missing: version {} was printed (print job {job_id}) when this \
             snapshot was made, and no copy of that sliced file with SHA-256 {sha256} was found \
             (not kept with the snapshot, not where the print left it, not in the slice \
             folder); the model files and snapshot.json were exported without it",
            snapshot.version
        ),
    }
}

/// The printed file the snapshot recorded: the kept copy when there is one; otherwise the
/// file found where the job's file was (hash checked), copied into the snapshot's folder and
/// checked again (when it cannot be kept, the found file itself). Blocking (file copies and
/// hashing).
pub(crate) fn printed(
    data: &DataDir,
    store: &Store,
    snapshot: &Snapshot,
) -> Result<Printed, StoreError> {
    let (Some(job_id), Some(sha256)) = (&snapshot.printed_job_id, &snapshot.printed_sha256) else {
        return Ok(Printed::Never);
    };
    let job = store.print_job(job_id).ok();
    let describe = |name: String| PrintedFile {
        job_id: job_id.clone(),
        delivery: job.as_ref().and_then(|job| job.delivery.clone()),
        status: job
            .as_ref()
            .map_or_else(|| "unknown".to_owned(), |job| job.status.clone()),
        file_name: name,
        sha256: sha256.to_lowercase(),
    };
    let dir = kept_dir(data, snapshot)?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort_unstable();
        if let Some(path) = paths.into_iter().find(|path| has_hash(path, sha256)) {
            let name = file_name_of(&path, sha256);
            return Ok(Printed::Kept {
                path,
                file: describe(name),
            });
        }
    }
    let sources = job
        .as_ref()
        .map(|job| candidates(data, store, job))
        .unwrap_or_default();
    for source in sources {
        if !has_hash(&source, sha256) {
            continue;
        }
        let name = file_name_of(&source, sha256);
        if std::fs::create_dir_all(&dir).is_err() {
            return Ok(Printed::Kept {
                path: source,
                file: describe(name),
            });
        }
        for kept_name in [name.clone(), safe_name(sha256)] {
            let target = dir.join(&kept_name);
            match copy_new(&source, &target) {
                Ok(_) if has_hash(&target, sha256) => {
                    return Ok(Printed::Kept {
                        path: target,
                        file: describe(kept_name),
                    });
                }
                // The source changed while it was copied: not this one.
                Ok(_) => {
                    let _ = std::fs::remove_file(&target);
                    break;
                }
                // A file of another hash has the name: try the safe name.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => break,
            }
        }
        // It could not be kept; the found file itself (hash checked again when exported).
        if has_hash(&source, sha256) {
            return Ok(Printed::Kept {
                path: source,
                file: describe(name),
            });
        }
    }
    Ok(Printed::Missing(missing(snapshot, job_id, sha256)))
}

/// Keeps the snapshot's recorded printed file (see [`printed`]) and says what happened, for
/// the audit line: the hash when kept, `null` when the snapshot recorded no print, `"not
/// found"` otherwise. Blocking.
pub(crate) fn keep_printed(
    data: &DataDir,
    store: &Store,
    snapshot: &Snapshot,
) -> serde_json::Value {
    match printed(data, store, snapshot) {
        Ok(Printed::Kept { file, .. }) => json!(file.sha256),
        Ok(Printed::Never) => json!(null),
        _ => json!("not found"),
    }
}

/// The first characters of `text` that fit in `max` UTF-8 bytes and `max` UTF-16 units, cut
/// on a character boundary.
fn cap(text: &str, max: usize) -> &str {
    let (mut bytes, mut units, mut end) = (0, 0, 0);
    for c in text.chars() {
        bytes += c.len_utf8();
        units += c.len_utf16();
        if bytes > max || units > max {
            break;
        }
        end += c.len_utf8();
    }
    &text[..end]
}

/// The export folder's name: the project's and the snapshot's name with the version, made of
/// characters every file system takes, at most [`MAX_FOLDER_STEM`] bytes before the version.
/// `Cable box - first fit (v3)`.
pub(crate) fn folder_name(project: &str, snapshot: &Snapshot) -> String {
    fn plain(text: &str, max: usize) -> String {
        let mut out = String::new();
        for c in text.chars() {
            let keep = c.is_alphanumeric() || " -_.,()+".contains(c);
            let c = if keep && !c.is_control() { c } else { '_' };
            if !(c == ' ' && out.ends_with(' ')) {
                out.push(c);
            }
        }
        let out: String = out.chars().take(max).collect();
        out.trim_matches([' ', '.']).to_owned()
    }
    let project = plain(project, 60);
    let name = plain(&snapshot.name, 80);
    let name = if name.is_empty() {
        "snapshot".to_owned()
    } else {
        name
    };
    let stem = if project.is_empty() {
        name
    } else {
        format!("{project} - {name}")
    };
    let mut stem = cap(&stem, MAX_FOLDER_STEM).trim_end_matches([' ', '.']);
    // Cut right after the project's name: drop the dangling separator.
    if let Some(head) = stem.strip_suffix(" -") {
        stem = head.trim_end_matches([' ', '.']);
    }
    format!("{stem} (v{})", snapshot.version)
}

/// The manifest of an export.
pub(crate) fn manifest(
    project_name: &str,
    snapshot: &Snapshot,
    files: &[SnapshotFile],
    printed: Option<&PrintedFile>,
    printed_missing: Option<&MissingPrinted>,
) -> Vec<u8> {
    let created = jiff::Timestamp::from_millisecond(snapshot.created_at_ms)
        .map(|at| at.strftime("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();
    let value = json!({
        "schema": 1,
        "project": {"id": snapshot.project_id, "name": project_name},
        "snapshot": {
            "name": snapshot.name,
            "version": snapshot.version,
            "note": snapshot.note,
            "created": created,
            "created_at_ms": snapshot.created_at_ms,
            "printed_job_id": snapshot.printed_job_id,
            "printed_sha256": snapshot.printed_sha256,
        },
        "exported": crate::util::now_rfc3339(),
        "files": files.iter().map(|file| json!({
            "name": file.path.file_name().map(|n| n.to_string_lossy().into_owned()),
            "kind": file.kind,
            "source": file.source,
            "bytes": file.bytes,
            "sha256": file.sha256,
        })).collect::<Vec<_>>(),
        "printed": printed,
        "printed_missing": printed_missing,
    });
    let mut bytes = serde_json::to_vec_pretty(&value).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn snapshot(name: &str) -> Snapshot {
        Snapshot {
            id: "id".into(),
            project_id: "p".into(),
            name: name.into(),
            version: 3,
            note: String::new(),
            created_at_ms: 0,
            printed_job_id: None,
            printed_sha256: None,
        }
    }

    #[test]
    fn the_export_folder_name_is_plain() {
        assert_eq!(
            folder_name("Cable box", &snapshot("first fit")),
            "Cable box - first fit (v3)"
        );
        assert_eq!(
            folder_name("a/b\\c:d", &snapshot("..  x*?\"<>|  ..")),
            "a_b_c_d - x______ (v3)"
        );
        assert_eq!(folder_name("", &snapshot("***")), "___ (v3)");
        assert_eq!(folder_name("..", &snapshot("..")), "snapshot (v3)");
    }

    #[test]
    fn the_export_folder_name_fits_in_200_bytes_before_the_version() {
        // 60 + 80 letters of four bytes each (two UTF-16 units) would be 563 bytes.
        let wide = "\u{20000}".repeat(100);
        for (project, name) in [
            (wide.as_str(), wide.as_str()),
            (
                "\u{00DC}".repeat(60).as_str(),
                "\u{00FC}".repeat(80).as_str(),
            ),
            ("p".repeat(60).as_str(), "\u{20AC}".repeat(80).as_str()),
        ] {
            let folder = folder_name(project, &snapshot(name));
            let stem = folder.strip_suffix(" (v3)").unwrap();
            assert!(stem.len() <= 200, "{} bytes: {stem}", stem.len());
            assert!(stem.encode_utf16().count() <= 200, "{stem}");
            assert!(!stem.ends_with([' ', '-', '.']), "{stem}");
            assert!(project.starts_with(stem.split(" - ").next().unwrap()));
        }
        // Cut right after the project's name: no dangling separator.
        // (49 four-byte characters and "ab": 198 bytes, then " -" makes 200.)
        let project = format!("{}ab", "\u{20000}".repeat(49));
        assert_eq!(
            folder_name(&project, &snapshot("lid")),
            format!("{project} (v3)")
        );
        assert_eq!(cap("a\u{20000}", 4), "a");
        // Short names stay whole.
        assert_eq!(
            folder_name("\u{00DC}ber", &snapshot("\u{00FC}ber")),
            "\u{00DC}ber - \u{00FC}ber (v3)"
        );
    }

    #[test]
    fn a_kept_file_has_a_plain_printed_name() {
        let hash = "ABCDEF0123456789ffff0000";
        assert_eq!(
            file_name_of(Path::new("/x/plate_1.gcode.3mf"), hash),
            "plate_1.gcode.3mf"
        );
        assert_eq!(
            file_name_of(Path::new("/x/Plate_1.GCODE.3MF"), hash),
            "Plate_1.GCODE.3MF"
        );
        for odd in [
            "/x/.hidden.gcode.3mf",
            "/x/snapshot.json",
            "/x/model.step",
            "/x/MODEL.3MF",
            "/x/model.3mf",
            "/x/plate.3mf",
            "/x/.gcode.3mf",
            "/x/a;b.gcode.3mf",
            "/",
        ] {
            assert_eq!(
                file_name_of(Path::new(odd), hash),
                "printed-abcdef0123456789.gcode.3mf",
                "{odd}"
            );
        }
    }

    #[test]
    fn files_are_created_new_and_sources_are_plain_files() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        std::fs::write(&source, b"bytes").unwrap();
        let target = tmp.path().join("target");
        assert_eq!(copy_new(&source, &target).unwrap(), 5);
        // Never replaced: a second copy or write over it fails and leaves it as it was.
        let error = copy_new(&source, &target).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        let error = write_new(&target, b"other").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&target).unwrap(), b"bytes");
        // A folder is not a source.
        assert!(copy_new(tmp.path(), &tmp.path().join("t2")).is_err());
        assert!(!tmp.path().join("t2").exists());
        #[cfg(unix)]
        {
            // A link is neither read through nor written through.
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(&source, &link).unwrap();
            assert!(copy_new(&link, &tmp.path().join("t3")).is_err());
            assert!(hash_regular(&link, u64::MAX).is_err());
            assert!(!has_hash(&link, &crate::util::sha256_hex(b"bytes")));
            let dangling = tmp.path().join("dangling");
            std::os::unix::fs::symlink(tmp.path().join("nowhere"), &dangling).unwrap();
            assert!(write_new(&dangling, b"x").is_err());
            assert!(!tmp.path().join("nowhere").exists());
        }
        assert!(has_hash(&source, &crate::util::sha256_hex(b"bytes")));
    }
}
