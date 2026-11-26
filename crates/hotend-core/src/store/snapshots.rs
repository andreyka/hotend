//! The store's table of snapshots: a name a person (or the agent, on the maker's words) gave
//! to one version of a project, so that the version can be found, exported and gone back to.
//!
//! Guarantees:
//! - A snapshot names an existing version of its own project (a foreign key), and keeps that
//!   version for as long as it exists: the row's project and version can never be changed
//!   (a trigger). Versions themselves are immutable and never deleted (`versions` triggers),
//!   so a pinned version and its model files stay exactly as they were.
//! - The print a snapshot keeps is decided when it is made: the newest job of its version
//!   whose file had left Hotend then, recorded with that file's SHA-256 (or none). The
//!   same trigger keeps both, so a later slice or print never changes what it exports.
//! - A name is one plain line of 1 to [`MAX_SNAPSHOT_NAME`] characters, without control,
//!   invisible or bidirectional characters, and unique in its project, compared without
//!   regard to ASCII case.
//! - Deleting a snapshot removes its row and nothing else: never a version, never a file.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{NewAudit, Store, StoreError, not_found};
use crate::util::{new_id, now_ms, truncate};

/// Longest snapshot name, in characters.
pub const MAX_SNAPSHOT_NAME: usize = 80;
/// Longest snapshot note, in characters.
pub const MAX_SNAPSHOT_NOTE: usize = 500;

/// A name given to one version of a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub project_id: String,
    /// One plain line, unique in the project.
    pub name: String,
    /// The version it pins.
    pub version: u32,
    /// Optional words about it; empty when none were given.
    pub note: String,
    pub created_at_ms: i64,
    /// The print job whose sliced file the snapshot keeps: the newest job of its version
    /// whose file had left Hotend when the snapshot was made; `None` when the version had
    /// not been printed then. Recorded once; a later slice or print never changes it.
    #[serde(default)]
    pub printed_job_id: Option<String>,
    /// SHA-256 (lowercase) of that job's sliced file: the exact bytes an export writes.
    #[serde(default)]
    pub printed_sha256: Option<String>,
}

/// The statuses of a print job whose file left Hotend (with a delivery recorded).
const LEFT_HOTEND: [&str; 8] = [
    "exported",
    "handed_off",
    "starting",
    "printing",
    "finished",
    "failed",
    "not_started",
    "cancelled",
];

/// Whether `c` may not appear in a snapshot name or note: control characters (line breaks,
/// tabs), whitespace other than the plain space (line and paragraph separators, wide
/// spaces), and the invisible and bidirectional format characters the confirmation dialog
/// refuses ([`hotend_printer::is_hidden_format`]).
fn is_refused_char(c: char) -> bool {
    c.is_control() || (c.is_whitespace() && c != ' ') || hotend_printer::is_hidden_format(c)
}

fn plain_line(what: &str, text: &str, min: usize, max: usize) -> Result<String, StoreError> {
    let text = text.trim();
    if text.chars().any(is_refused_char) {
        return Err(StoreError::Invalid(format!(
            "a snapshot {what} must be one plain line, without line breaks, tabs, or invisible \
             or direction-changing characters"
        )));
    }
    let count = text.chars().count();
    if count < min || count > max {
        return Err(StoreError::Invalid(if min == 0 {
            format!("a snapshot {what} has at most {max} characters")
        } else {
            format!("a snapshot {what} must have {min} to {max} characters")
        }));
    }
    Ok(text.to_owned())
}

/// The name as it is stored (trimmed), or why it cannot be a snapshot name.
pub fn snapshot_name(name: &str) -> Result<String, StoreError> {
    plain_line("name", name, 1, MAX_SNAPSHOT_NAME)
}

/// The note as it is stored (trimmed, possibly empty), or why it cannot be one.
pub fn snapshot_note(note: &str) -> Result<String, StoreError> {
    plain_line("note", note, 0, MAX_SNAPSHOT_NOTE)
}

const SNAPSHOT_COLUMNS: &str = "SELECT id, project_id, name, version, note, created_at_ms, \
     printed_job_id, printed_sha256 FROM snapshots";

fn snapshot_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Snapshot> {
    Ok(Snapshot {
        id: row.get(0)?,
        project_id: row.get(1)?,
        name: row.get(2)?,
        version: row.get(3)?,
        note: row.get(4)?,
        created_at_ms: row.get(5)?,
        printed_job_id: row.get(6)?,
        printed_sha256: row.get(7)?,
    })
}

impl Store {
    /// Names `version` of a project. Refused for a name or note that is not one plain line
    /// of the allowed length, a version the project does not have, and a name the project
    /// already uses (`Invalid`, naming the version that has it).
    pub fn create_snapshot(
        &self,
        project_id: &str,
        name: &str,
        version: u32,
        note: &str,
    ) -> Result<Snapshot, StoreError> {
        let name = snapshot_name(name)?;
        let note = snapshot_note(note)?;
        // The version's existence is the foreign key's; asked first for the plain error.
        self.version(project_id, version)?;
        if let Some(taken) = self.find_snapshot(project_id, &name)? {
            return Err(StoreError::Invalid(format!(
                "this project already has a snapshot named \"{}\" (version {}); choose another \
                 name",
                taken.name, taken.version
            )));
        }
        // The print the snapshot keeps is the one that exists now, recorded with its hash.
        let printed = self
            .list_pending_prints(project_id)?
            .into_iter()
            .filter(|job| {
                job.version == version
                    && job.delivery.is_some()
                    && LEFT_HOTEND.contains(&job.status.as_str())
            })
            .max_by(|a, b| (a.created_at_ms, &a.id).cmp(&(b.created_at_ms, &b.id)));
        let snapshot = Snapshot {
            id: new_id(),
            project_id: project_id.to_owned(),
            name,
            version,
            note,
            created_at_ms: now_ms(),
            printed_sha256: printed.as_ref().map(|job| job.slice_sha256.to_lowercase()),
            printed_job_id: printed.map(|job| job.id),
        };
        self.lock().execute(
            "INSERT INTO snapshots (id, project_id, name, version, note, created_at_ms, \
             printed_job_id, printed_sha256) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                snapshot.id,
                snapshot.project_id,
                snapshot.name,
                snapshot.version,
                snapshot.note,
                snapshot.created_at_ms,
                snapshot.printed_job_id,
                snapshot.printed_sha256
            ],
        )?;
        Ok(snapshot)
    }

    /// The snapshots of a project, oldest first.
    pub fn snapshots(&self, project_id: &str) -> Result<Vec<Snapshot>, StoreError> {
        let connection = self.lock();
        let mut statement = connection.prepare(&format!(
            "{SNAPSHOT_COLUMNS} WHERE project_id = ?1 ORDER BY created_at_ms, id"
        ))?;
        let rows = statement.query_map([project_id], snapshot_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn find_snapshot(&self, project_id: &str, name: &str) -> Result<Option<Snapshot>, StoreError> {
        Ok(self
            .lock()
            .query_row(
                &format!("{SNAPSHOT_COLUMNS} WHERE project_id = ?1 AND name = ?2"),
                params![project_id, name.trim()],
                snapshot_row,
            )
            .optional()?)
    }

    /// One snapshot of a project by its name (compared without regard to ASCII case).
    pub fn snapshot(&self, project_id: &str, name: &str) -> Result<Snapshot, StoreError> {
        self.find_snapshot(project_id, name)?
            .ok_or_else(|| not_found("snapshot", &truncate(name.trim(), MAX_SNAPSHOT_NAME)))
    }

    /// Removes the snapshot's row, and nothing else: its version and every file stay.
    /// Returns the snapshot as it was.
    pub fn delete_snapshot(&self, project_id: &str, name: &str) -> Result<Snapshot, StoreError> {
        let snapshot = self.snapshot(project_id, name)?;
        self.lock()
            .execute("DELETE FROM snapshots WHERE id = ?1", [&snapshot.id])?;
        Ok(snapshot)
    }

    /// Writes the audit line of a snapshot operation (`create`, `delete`, `export`,
    /// `restore`): kind `snapshot`, with the snapshot's name, version and `detail` merged.
    pub fn audit_snapshot(
        &self,
        snapshot: &Snapshot,
        action: &str,
        run_id: Option<&str>,
        outcome: &str,
        mut detail: serde_json::Value,
    ) {
        if !detail.is_object() {
            detail = json!({});
        }
        detail["snapshot"] = json!(snapshot.name);
        detail["version"] = json!(snapshot.version);
        // The audit log must not turn a finished operation into a failure.
        let _ = self.audit(NewAudit {
            project_id: Some(snapshot.project_id.clone()),
            run_id: run_id.map(str::to_owned),
            kind: "snapshot",
            name: action.to_owned(),
            args_sha256: None,
            outcome: outcome.to_owned(),
            detail,
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use super::*;
    use crate::paths::DataDir;
    use crate::store::{NewVersion, VersionStatus};

    fn fixture() -> (tempfile::TempDir, DataDir, Store, String) {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataDir::resolve(Some(tmp.path())).unwrap();
        let store = Store::open(&data.db_path()).unwrap();
        let project = store.create_project("snapshots").unwrap();
        for marker in ["one", "two"] {
            let dir = data.jobs_dir().join(format!("staged-{marker}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("model.py"), marker).unwrap();
            store
                .create_version(
                    &data,
                    &project.id,
                    &dir,
                    NewVersion {
                        parent: None,
                        origin: "write_model_code".into(),
                        note: marker.into(),
                        status: VersionStatus::Ok,
                        code_sha256: "00".into(),
                        params: BTreeMap::new(),
                    },
                )
                .unwrap();
        }
        (tmp, data, store, project.id)
    }

    #[test]
    fn snapshots_round_trip_and_deleting_one_removes_only_its_row() {
        let (_tmp, data, store, project) = fixture();
        let first = store
            .create_snapshot(&project, "  first fit  ", 1, "lid snaps")
            .unwrap();
        assert_eq!(
            (first.name.as_str(), first.version, first.note.as_str()),
            ("first fit", 1, "lid snaps")
        );
        let second = store.create_snapshot(&project, "wider", 2, "").unwrap();
        assert_eq!(store.snapshots(&project).unwrap(), [first.clone(), second]);
        assert_eq!(store.snapshot(&project, "FIRST FIT").unwrap(), first);
        // Another project has its own names.
        let other = store.create_project("other").unwrap();
        assert!(store.snapshots(&other.id).unwrap().is_empty());
        assert!(matches!(
            store.snapshot(&other.id, "first fit"),
            Err(StoreError::NotFound { .. })
        ));

        // Nothing the project has changes but the label.
        let versions = store.list_versions(&project).unwrap();
        let current = store.project(&project).unwrap().current_version;
        let file = data.version_dir(&project, 1).unwrap().join("model.py");
        let deleted = store.delete_snapshot(&project, "first fit").unwrap();
        assert_eq!(deleted, first);
        assert_eq!(store.list_versions(&project).unwrap(), versions);
        assert_eq!(store.project(&project).unwrap().current_version, current);
        assert_eq!(std::fs::read_to_string(file).unwrap(), "one");
        assert_eq!(store.snapshots(&project).unwrap().len(), 1);
        assert!(matches!(
            store.delete_snapshot(&project, "first fit"),
            Err(StoreError::NotFound { .. })
        ));
        // The freed name can be used again.
        store.create_snapshot(&project, "first fit", 2, "").unwrap();
    }

    #[test]
    fn a_name_is_unique_in_its_project_and_names_an_existing_version() {
        let (_tmp, _data, store, project) = fixture();
        store.create_snapshot(&project, "Lid A", 1, "").unwrap();
        for taken in ["Lid A", "lid a", " LID A "] {
            let error = store.create_snapshot(&project, taken, 2, "").unwrap_err();
            assert!(
                matches!(&error, StoreError::Invalid(text) if text.contains("version 1")),
                "{error}"
            );
        }
        assert!(matches!(
            store.create_snapshot(&project, "later", 3, ""),
            Err(StoreError::NotFound { .. })
        ));
        assert!(matches!(
            store.create_snapshot("no-such-project", "x", 1, ""),
            Err(StoreError::NotFound { .. })
        ));
        // The row keeps its version: the database refuses to move it.
        let error = store
            .lock()
            .execute("UPDATE snapshots SET version = 2", [])
            .unwrap_err();
        assert!(error.to_string().contains("keeps its version"), "{error}");
        // And the print it recorded (none here).
        let error = store
            .lock()
            .execute(
                "UPDATE snapshots SET printed_job_id = 'j', printed_sha256 = 'aa'",
                [],
            )
            .unwrap_err();
        assert!(error.to_string().contains("keeps its version"), "{error}");
        assert_eq!(
            store.snapshot(&project, "lid a").unwrap().printed_job_id,
            None
        );
        assert_eq!(store.snapshot(&project, "lid a").unwrap().version, 1);
        // And a version a snapshot names still cannot be deleted.
        assert!(
            store
                .lock()
                .execute("DELETE FROM versions WHERE number = 1", [])
                .is_err()
        );
    }

    #[test]
    fn a_name_is_one_plain_line_of_at_most_80_characters() {
        let (_tmp, _data, store, project) = fixture();
        let longest = "x".repeat(MAX_SNAPSHOT_NAME);
        assert_eq!(snapshot_name(&longest).unwrap(), longest);
        assert_eq!(
            snapshot_name("Ünïcode lid, v2 (wide)").unwrap(),
            "Ünïcode lid, v2 (wide)"
        );
        for refused in [
            String::new(),
            "   ".to_owned(),
            "x".repeat(MAX_SNAPSHOT_NAME + 1),
            "two\nlines".to_owned(),
            "tab\there".to_owned(),
            "carriage\rreturn".to_owned(),
            "zero\u{200B}width".to_owned(),
            "right\u{202E}to left".to_owned(),
            "isolate\u{2066}d".to_owned(),
            "line\u{2028}separator".to_owned(),
            "bom\u{FEFF}".to_owned(),
            "nul\u{0}".to_owned(),
        ] {
            assert!(snapshot_name(&refused).is_err(), "{refused:?}");
            assert!(
                matches!(
                    store.create_snapshot(&project, &refused, 1, ""),
                    Err(StoreError::Invalid(_))
                ),
                "{refused:?}"
            );
        }
        assert!(snapshot_note("").unwrap().is_empty());
        assert!(snapshot_note(&"n".repeat(MAX_SNAPSHOT_NOTE + 1)).is_err());
        assert!(snapshot_note("a\nb").is_err());
        assert!(store.snapshots(&project).unwrap().is_empty());
    }
}
