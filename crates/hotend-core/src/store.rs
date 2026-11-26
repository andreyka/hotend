//! The project store: one SQLite file with projects, versions, messages, the audit log,
//! settings, slice records, pending print jobs, snapshots and what web research found.
//!
//! Guarantees:
//! - The schema is created and upgraded by numbered migrations (`PRAGMA user_version`).
//! - A version row can be inserted but never updated or deleted (enforced by triggers), and
//!   its folder is moved into place in the same transaction that inserts the row.
//! - Nothing secret is stored here. Audit entries hold argument hashes, not arguments.

mod research;
mod snapshots;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::paths::{DataDir, PathError};
use crate::util::{new_id, now_ms, truncate};

pub use research::{
    MAX_LOG, MAX_SOURCES, NewResearchNote, ResearchLogEntry, ResearchNote, ResearchRow,
    ResearchSource,
};
pub use snapshots::{MAX_SNAPSHOT_NAME, MAX_SNAPSHOT_NOTE, Snapshot, snapshot_name, snapshot_note};

/// Longest project name, in characters.
pub const MAX_PROJECT_NAME: usize = 120;
/// Largest stored agent history per project.
pub const MAX_HISTORY_BYTES: usize = 32 * 1024 * 1024;
/// Largest JSON detail of one audit entry.
const MAX_AUDIT_DETAIL_BYTES: usize = 16 * 1024;

const MIGRATIONS: &[&str] = &[
    // 1: the first schema.
    "
    CREATE TABLE projects (
        id              TEXT PRIMARY KEY,
        name            TEXT NOT NULL,
        created_at_ms   INTEGER NOT NULL,
        updated_at_ms   INTEGER NOT NULL,
        current_version INTEGER
    );
    CREATE TABLE versions (
        project_id    TEXT NOT NULL REFERENCES projects(id),
        number        INTEGER NOT NULL,
        created_at_ms INTEGER NOT NULL,
        parent        INTEGER,
        origin        TEXT NOT NULL,
        note          TEXT NOT NULL,
        status        TEXT NOT NULL,
        code_sha256   TEXT NOT NULL,
        params_json   TEXT NOT NULL,
        PRIMARY KEY (project_id, number)
    );
    CREATE TRIGGER versions_are_immutable_update BEFORE UPDATE ON versions
        BEGIN SELECT RAISE(ABORT, 'versions are immutable'); END;
    CREATE TRIGGER versions_are_immutable_delete BEFORE DELETE ON versions
        BEGIN SELECT RAISE(ABORT, 'versions are immutable'); END;
    CREATE TABLE messages (
        project_id    TEXT PRIMARY KEY REFERENCES projects(id),
        provider      TEXT NOT NULL,
        history_json  TEXT NOT NULL,
        updated_at_ms INTEGER NOT NULL
    );
    CREATE TABLE transcript (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        project_id TEXT NOT NULL REFERENCES projects(id),
        run_id     TEXT NOT NULL,
        role       TEXT NOT NULL,
        text       TEXT NOT NULL,
        at_ms      INTEGER NOT NULL
    );
    CREATE INDEX transcript_by_project ON transcript(project_id, id);
    CREATE TABLE audit (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        at_ms       INTEGER NOT NULL,
        project_id  TEXT,
        run_id      TEXT,
        kind        TEXT NOT NULL,
        name        TEXT NOT NULL,
        args_sha256 TEXT,
        outcome     TEXT NOT NULL,
        detail_json TEXT NOT NULL
    );
    CREATE INDEX audit_by_project ON audit(project_id, id);
    CREATE TABLE settings (
        key        TEXT PRIMARY KEY,
        value_json TEXT NOT NULL
    );
    CREATE TABLE slices (
        project_id    TEXT NOT NULL,
        version       INTEGER NOT NULL,
        profile_hash  TEXT NOT NULL,
        profile       TEXT NOT NULL,
        summary_json  TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY (project_id, version, profile_hash),
        FOREIGN KEY (project_id, version) REFERENCES versions(project_id, number)
    );
    CREATE TABLE pending_print_jobs (
        id            TEXT PRIMARY KEY,
        project_id    TEXT NOT NULL,
        version       INTEGER NOT NULL,
        profile_hash  TEXT NOT NULL,
        slice_sha256  TEXT NOT NULL,
        note          TEXT NOT NULL,
        status        TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        FOREIGN KEY (project_id, version, profile_hash)
            REFERENCES slices(project_id, version, profile_hash)
    );
    ",
    // 2: transcript lines of a run whose conversation was not stored are marked.
    "
    ALTER TABLE transcript ADD COLUMN remembered INTEGER NOT NULL DEFAULT 1;
    ",
    // 3: a slice records what it was made for (plate, printer, filaments), and a print
    // job its delivery and progress.
    "
    ALTER TABLE slices ADD COLUMN job_json TEXT NOT NULL DEFAULT '{}';
    ALTER TABLE pending_print_jobs ADD COLUMN delivery TEXT;
    ALTER TABLE pending_print_jobs ADD COLUMN detail_json TEXT NOT NULL DEFAULT '{}';
    ALTER TABLE pending_print_jobs ADD COLUMN updated_at_ms INTEGER NOT NULL DEFAULT 0;
    ",
    // 4: web research: per project its settings and what it used, the numbers found on the
    // web (notes), the pages they came from and a log of the searches and fetches.
    "
    CREATE TABLE project_research (
        project_id       TEXT PRIMARY KEY REFERENCES projects(id),
        settings_json    TEXT NOT NULL DEFAULT '{}',
        searches         INTEGER NOT NULL DEFAULT 0,
        fetches          INTEGER NOT NULL DEFAULT 0,
        billed_searches  INTEGER NOT NULL DEFAULT 0,
        result_bytes     INTEGER NOT NULL DEFAULT 0,
        refused_search   INTEGER NOT NULL DEFAULT 0,
        refused_fetch    INTEGER NOT NULL DEFAULT 0,
        notes_shown_hash TEXT NOT NULL DEFAULT '',
        shape_hash       TEXT NOT NULL DEFAULT '',
        next_note        INTEGER NOT NULL DEFAULT 1
    );
    CREATE TABLE research_notes (
        project_id    TEXT NOT NULL REFERENCES projects(id),
        id            TEXT NOT NULL,
        key           TEXT NOT NULL,
        label         TEXT NOT NULL,
        value         REAL NOT NULL,
        unit          TEXT NOT NULL,
        url           TEXT NOT NULL,
        title         TEXT NOT NULL,
        quote         TEXT NOT NULL,
        locator       TEXT NOT NULL,
        evidence      TEXT NOT NULL,
        state         TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY (project_id, id)
    );
    CREATE TABLE research_sources (
        seq            INTEGER PRIMARY KEY AUTOINCREMENT,
        project_id     TEXT NOT NULL REFERENCES projects(id),
        url            TEXT NOT NULL,
        title          TEXT NOT NULL,
        via            TEXT NOT NULL,
        page_text      TEXT,
        citations_json TEXT NOT NULL DEFAULT '[]',
        run_id         TEXT NOT NULL,
        seen_at_ms     INTEGER NOT NULL,
        UNIQUE (project_id, url)
    );
    CREATE TABLE research_log (
        seq        INTEGER PRIMARY KEY AUTOINCREMENT,
        project_id TEXT NOT NULL REFERENCES projects(id),
        run_id     TEXT NOT NULL,
        kind       TEXT NOT NULL,
        text       TEXT NOT NULL,
        outcome    TEXT NOT NULL,
        at_ms      INTEGER NOT NULL
    );
    CREATE INDEX research_log_project ON research_log(project_id, seq);
    ",
    // 5: editable slicer projects: what each was made from, its hash, and its last
    // hand-over.
    "
    CREATE TABLE project_exports (
        id            TEXT PRIMARY KEY,
        project_id    TEXT NOT NULL,
        version       INTEGER NOT NULL,
        plate         INTEGER NOT NULL,
        plan_sha256   TEXT NOT NULL,
        profile_hash  TEXT NOT NULL,
        slice_sha256  TEXT NOT NULL,
        folder        TEXT NOT NULL,
        sha256        TEXT NOT NULL,
        summary_json  TEXT NOT NULL,
        delivery      TEXT,
        detail_json   TEXT NOT NULL DEFAULT '{}',
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        FOREIGN KEY (project_id, version) REFERENCES versions(project_id, number)
    );
    CREATE INDEX project_exports_by_project ON project_exports(project_id, id);
    ",
    // 6: snapshots: a name for one version of a project. The row keeps its version; the
    // version itself is immutable and never deleted, so a pinned version stays as it was.
    // The print job whose sliced file the snapshot keeps, and that file's SHA-256, are
    // recorded when it is made (both NULL when the version had not been printed then) and
    // never change either.
    "
    CREATE TABLE snapshots (
        id             TEXT PRIMARY KEY,
        project_id     TEXT NOT NULL REFERENCES projects(id),
        name           TEXT NOT NULL COLLATE NOCASE,
        version        INTEGER NOT NULL,
        note           TEXT NOT NULL DEFAULT '',
        created_at_ms  INTEGER NOT NULL,
        printed_job_id TEXT,
        printed_sha256 TEXT,
        UNIQUE (project_id, name),
        FOREIGN KEY (project_id, version) REFERENCES versions(project_id, number),
        CHECK ((printed_job_id IS NULL) = (printed_sha256 IS NULL))
    );
    CREATE TRIGGER snapshots_keep_their_version
        BEFORE UPDATE OF project_id, version, printed_job_id, printed_sha256 ON snapshots
        BEGIN SELECT RAISE(ABORT, 'a snapshot keeps its version'); END;
    ",
];

/// Why a store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite reported an error.
    #[error("the project database reported an error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A stored JSON column does not parse; the database was edited or is damaged.
    #[error("the project database holds unreadable data in {what}: {source}")]
    Corrupt {
        what: &'static str,
        source: serde_json::Error,
    },
    /// The database was written by a newer Hotend.
    #[error(
        "the project database has schema version {found}, newer than this build understands \
         ({supported}); update Hotend"
    )]
    TooNew { found: i64, supported: i64 },
    /// No row with that id.
    #[error("{what} `{id}` does not exist")]
    NotFound { what: &'static str, id: String },
    /// A value is too long or empty.
    #[error("{0}")]
    Invalid(String),
    /// A data path could not be built.
    #[error(transparent)]
    Path(#[from] PathError),
    /// A version folder could not be moved into place.
    #[error("cannot store the version folder {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// One product being designed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// The version the tools and the UI act on; `None` until the first build succeeds.
    pub current_version: Option<u32>,
}

/// How a build ended when it produced a version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    /// Built and every error-severity check passed.
    Ok,
    /// Built, but an error-severity check failed.
    CheckError,
}

impl VersionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::CheckError => "check_error",
        }
    }
}

/// One immutable build of a project's model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Version {
    pub project_id: String,
    pub number: u32,
    pub created_at_ms: i64,
    /// The version this one was derived from, if any.
    pub parent: Option<u32>,
    /// What created it: `write_model_code`, `set_parameter` or `parameter_set`.
    pub origin: String,
    pub note: String,
    pub status: VersionStatus,
    pub code_sha256: String,
    pub params: BTreeMap<String, f64>,
}

/// What a caller supplies to create a version; the store assigns the number.
#[derive(Debug, Clone)]
pub struct NewVersion {
    pub parent: Option<u32>,
    pub origin: String,
    pub note: String,
    pub status: VersionStatus,
    pub code_sha256: String,
    pub params: BTreeMap<String, f64>,
}

/// One line of the audit log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: i64,
    pub at_ms: i64,
    pub project_id: Option<String>,
    pub run_id: Option<String>,
    /// `tool`, `job`, `egress`, `secret`, `agent` or `version`.
    pub kind: String,
    /// The tool, op, provider or key name.
    pub name: String,
    /// SHA-256 of the canonical JSON arguments, when the entry is about a call.
    pub args_sha256: Option<String>,
    /// `ok`, or a short error code.
    pub outcome: String,
    /// Small, secret-free facts: sizes, durations, version numbers, error text.
    pub detail: Value,
}

/// An audit entry to append.
#[derive(Debug, Clone, Default)]
pub struct NewAudit {
    pub project_id: Option<String>,
    pub run_id: Option<String>,
    pub kind: &'static str,
    pub name: String,
    pub args_sha256: Option<String>,
    pub outcome: String,
    pub detail: Value,
}

/// A line of the conversation as the user saw it (the provider-native history is separate
/// and opaque).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub id: i64,
    pub run_id: String,
    /// `user` or `assistant`.
    pub role: String,
    pub text: String,
    pub at_ms: i64,
    /// False for the lines of a run that failed or ended with `stop_reason: error`: the
    /// agent's stored history does not contain them, so the next message continues without
    /// them. The UI shows such lines as not remembered.
    #[serde(default = "remembered_by_default")]
    pub remembered: bool,
}

fn remembered_by_default() -> bool {
    true
}

/// An audit entry that is written when the value is dropped, whatever ends the operation
/// it describes. It starts with the outcome `abandoned`, which is what gets recorded when
/// the operation's future is dropped before it finished (a cancelled run, a closed
/// application); a finished operation sets its real outcome first.
#[derive(Debug)]
pub struct PendingAudit<'a> {
    store: &'a Store,
    entry: NewAudit,
    started: std::time::Instant,
    discarded: bool,
}

impl<'a> PendingAudit<'a> {
    /// The outcome recorded for an operation that never finished.
    pub const ABANDONED: &'static str = "abandoned";

    /// Prepares `entry`; its `outcome` is replaced by `abandoned` until [`Self::finish`].
    pub fn new(store: &'a Store, mut entry: NewAudit) -> Self {
        entry.outcome = Self::ABANDONED.to_owned();
        if !entry.detail.is_object() {
            entry.detail = Value::Object(serde_json::Map::new());
        }
        Self {
            store,
            entry,
            started: std::time::Instant::now(),
            discarded: false,
        }
    }

    /// The entry's detail object, to add facts as they become known.
    pub fn detail(&mut self) -> &mut Value {
        &mut self.entry.detail
    }

    /// Sets the outcome the entry is written with.
    pub fn finish(&mut self, outcome: impl Into<String>) {
        self.entry.outcome = outcome.into();
    }

    /// Replaces the entry's name.
    pub fn name(&mut self, name: impl Into<String>) {
        self.entry.name = name.into();
    }

    /// Keeps the entry from being written, for a caller that records it another way.
    pub fn suppress(&mut self) {
        self.discarded = true;
    }
}

impl Drop for PendingAudit<'_> {
    fn drop(&mut self) {
        if self.discarded {
            return;
        }
        let mut entry = std::mem::take(&mut self.entry);
        entry.detail["duration_ms"] = Value::from(self.started.elapsed().as_millis() as u64);
        // The audit log must not turn a finished operation into a failure; a full disk
        // shows up on the next store write.
        let _ = self.store.audit(entry);
    }
}

/// The stored result of slicing one version with one profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SliceRecord {
    pub project_id: String,
    pub version: u32,
    pub profile_hash: String,
    pub profile: String,
    pub summary: Value,
    /// What the slice was made for: plate, printer, material and filament slots.
    pub job: Value,
    pub created_at_ms: i64,
}

/// An editable slicer project Hotend made for one plate of a version (`project_exports`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectExportRecord {
    pub id: String,
    pub project_id: String,
    pub version: u32,
    pub plate: u32,
    /// The plate's print plan the project carries, by its hash.
    pub plan_sha256: String,
    /// The slice of the same plan whose file passed the G-code check, by its profile hash
    /// and the SHA-256 of its file.
    pub profile_hash: String,
    pub slice_sha256: String,
    /// The project's folder below the version's `slicer-project/`.
    pub folder: String,
    /// SHA-256 of the project file.
    pub sha256: String,
    /// What the slicer backend answered (`ProjectExportSummary`).
    pub summary: Value,
    /// How it was last handed over, if it was.
    pub delivery: Option<String>,
    pub detail: Value,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// A print the agent proposed. It is only a record for the user to review: nothing in this
/// crate can start a print from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPrintJob {
    pub id: String,
    pub project_id: String,
    pub version: u32,
    pub profile_hash: String,
    /// SHA-256 of the sliced file the proposal refers to.
    pub slice_sha256: String,
    pub note: String,
    /// `pending` until the user confirms it; then where the delivery got to: `rejected`,
    /// `exported`, `handed_off`, `starting`, `not_started`, `printing`, `finished`,
    /// `failed` or `cancelled`.
    pub status: String,
    pub created_at_ms: i64,
    /// How the confirmed job left: `simulated_printer`, `bambu_connect` or
    /// `export_to_folder`.
    pub delivery: Option<String>,
    /// The last state reported for the job, for display.
    pub detail: Value,
    pub updated_at_ms: i64,
}

/// The SQLite store. Methods are short synchronous calls behind one mutex.
#[derive(Debug)]
pub struct Store {
    connection: Mutex<Connection>,
}

impl Store {
    /// Opens (and if needed creates or upgrades) the database at `path`.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let mut connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock leaves SQLite itself consistent, so keep going.
        self.connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ---- projects -------------------------------------------------------------------

    /// Creates a project with a new id.
    pub fn create_project(&self, name: &str) -> Result<Project, StoreError> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_PROJECT_NAME {
            return Err(StoreError::Invalid(format!(
                "a project name must have 1 to {MAX_PROJECT_NAME} characters"
            )));
        }
        let project = Project {
            id: new_id(),
            name: name.to_owned(),
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
            current_version: None,
        };
        self.lock().execute(
            "INSERT INTO projects (id, name, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?4)",
            params![
                project.id,
                project.name,
                project.created_at_ms,
                project.updated_at_ms
            ],
        )?;
        Ok(project)
    }

    /// All projects, most recently changed first.
    pub fn list_projects(&self) -> Result<Vec<Project>, StoreError> {
        let connection = self.lock();
        let mut statement = connection.prepare(
            "SELECT id, name, created_at_ms, updated_at_ms, current_version FROM projects \
             ORDER BY updated_at_ms DESC, id DESC",
        )?;
        let rows = statement.query_map([], project_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// One project.
    pub fn project(&self, id: &str) -> Result<Project, StoreError> {
        self.lock()
            .query_row(
                "SELECT id, name, created_at_ms, updated_at_ms, current_version FROM projects \
                 WHERE id = ?1",
                [id],
                project_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found("project", id))
    }

    /// Makes an existing version the project's current one (revert or redo).
    pub fn set_current_version(&self, project_id: &str, number: u32) -> Result<(), StoreError> {
        let connection = self.lock();
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM versions WHERE project_id = ?1 AND number = ?2)",
            params![project_id, number],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(not_found("version", &format!("{project_id}/{number}")));
        }
        connection.execute(
            "UPDATE projects SET current_version = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![project_id, number, now_ms()],
        )?;
        Ok(())
    }

    // ---- versions -------------------------------------------------------------------

    /// Stores a finished build as the next version of a project and makes it current.
    ///
    /// `staged` is a complete folder on the same filesystem as the data directory. In one
    /// transaction the next number is taken, the folder is renamed to `versions/<n>` and the
    /// row is inserted; if any step fails nothing is left behind except `staged`.
    pub fn create_version(
        &self,
        data: &DataDir,
        project_id: &str,
        staged: &Path,
        new: NewVersion,
    ) -> Result<Version, StoreError> {
        let mut connection = self.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let known: Option<i64> = transaction
            .query_row("SELECT 1 FROM projects WHERE id = ?1", [project_id], |r| {
                r.get(0)
            })
            .optional()?;
        if known.is_none() {
            return Err(not_found("project", project_id));
        }
        let number: u32 = transaction.query_row(
            "SELECT COALESCE(MAX(number), 0) + 1 FROM versions WHERE project_id = ?1",
            [project_id],
            |row| row.get(0),
        )?;
        let version = Version {
            project_id: project_id.to_owned(),
            number,
            created_at_ms: now_ms(),
            parent: new.parent,
            origin: new.origin,
            note: new.note,
            status: new.status,
            code_sha256: new.code_sha256,
            params: new.params,
        };
        let params_json =
            serde_json::to_string(&version.params).map_err(|source| StoreError::Corrupt {
                what: "version parameters",
                source,
            })?;
        transaction.execute(
            "INSERT INTO versions (project_id, number, created_at_ms, parent, origin, note, \
             status, code_sha256, params_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                version.project_id,
                version.number,
                version.created_at_ms,
                version.parent,
                version.origin,
                version.note,
                version.status.as_str(),
                version.code_sha256,
                params_json
            ],
        )?;
        transaction.execute(
            "UPDATE projects SET current_version = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![project_id, number, version.created_at_ms],
        )?;
        let target = data.version_dir(project_id, number)?;
        let io = |source| StoreError::Io {
            path: target.clone(),
            source,
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        if target.exists() {
            return Err(io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "a folder with this version number already exists",
            )));
        }
        std::fs::rename(staged, &target).map_err(io)?;
        if let Err(error) = transaction.commit() {
            // Put the folder back so the caller can clean it up; the row was not stored.
            let _ = std::fs::rename(&target, staged);
            return Err(error.into());
        }
        Ok(version)
    }

    /// One version of a project.
    pub fn version(&self, project_id: &str, number: u32) -> Result<Version, StoreError> {
        self.lock()
            .query_row(
                &format!("{VERSION_COLUMNS} WHERE project_id = ?1 AND number = ?2"),
                params![project_id, number],
                version_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found("version", &format!("{project_id}/{number}")))?
    }

    /// All versions of a project, oldest first.
    pub fn list_versions(&self, project_id: &str) -> Result<Vec<Version>, StoreError> {
        let connection = self.lock();
        let mut statement = connection.prepare(&format!(
            "{VERSION_COLUMNS} WHERE project_id = ?1 ORDER BY number"
        ))?;
        let rows = statement.query_map([project_id], version_from_row)?;
        rows.map(|row| row?).collect()
    }

    // ---- messages -------------------------------------------------------------------

    /// The provider-native history of a project and the provider kind it belongs to.
    /// The history is opaque to the core: it is stored and handed back to the agent as is.
    pub fn history(&self, project_id: &str) -> Result<Option<(String, Vec<Value>)>, StoreError> {
        let row: Option<(String, String)> = self
            .lock()
            .query_row(
                "SELECT provider, history_json FROM messages WHERE project_id = ?1",
                [project_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(provider, json)| {
            serde_json::from_str(&json)
                .map(|history| (provider, history))
                .map_err(|source| StoreError::Corrupt {
                    what: "message history",
                    source,
                })
        })
        .transpose()
    }

    /// Replaces a project's history.
    pub fn set_history(
        &self,
        project_id: &str,
        provider: &str,
        history: &[Value],
    ) -> Result<(), StoreError> {
        let json = serde_json::to_string(history).map_err(|source| StoreError::Corrupt {
            what: "message history",
            source,
        })?;
        if json.len() > MAX_HISTORY_BYTES {
            return Err(StoreError::Invalid(format!(
                "the conversation history is larger than {MAX_HISTORY_BYTES} bytes; start a new \
                 project"
            )));
        }
        self.lock().execute(
            "INSERT INTO messages (project_id, provider, history_json, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(project_id) DO UPDATE SET \
             provider = excluded.provider, history_json = excluded.history_json, \
             updated_at_ms = excluded.updated_at_ms",
            params![project_id, provider, json, now_ms()],
        )?;
        Ok(())
    }

    /// Appends a line to the readable transcript.
    pub fn append_transcript(
        &self,
        project_id: &str,
        run_id: &str,
        role: &str,
        text: &str,
    ) -> Result<(), StoreError> {
        self.lock().execute(
            "INSERT INTO transcript (project_id, run_id, role, text, at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![project_id, run_id, role, text, now_ms()],
        )?;
        Ok(())
    }

    /// The readable transcript of a project, oldest first.
    pub fn transcript(&self, project_id: &str) -> Result<Vec<TranscriptEntry>, StoreError> {
        let connection = self.lock();
        let mut statement = connection.prepare(
            "SELECT id, run_id, role, text, at_ms, remembered FROM transcript \
             WHERE project_id = ?1 ORDER BY id",
        )?;
        let rows = statement.query_map([project_id], |row| {
            Ok(TranscriptEntry {
                id: row.get(0)?,
                run_id: row.get(1)?,
                role: row.get(2)?,
                text: row.get(3)?,
                at_ms: row.get(4)?,
                remembered: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Marks every transcript line of a run as not remembered: the run's conversation was
    /// not stored in the agent's history.
    pub fn mark_run_not_remembered(
        &self,
        project_id: &str,
        run_id: &str,
    ) -> Result<(), StoreError> {
        self.lock().execute(
            "UPDATE transcript SET remembered = 0 WHERE project_id = ?1 AND run_id = ?2",
            params![project_id, run_id],
        )?;
        Ok(())
    }

    /// The numbers of the versions a run made, in order, from its `version` audit entries.
    pub fn versions_made_in_run(
        &self,
        project_id: &str,
        run_id: &str,
    ) -> Result<Vec<u32>, StoreError> {
        let connection = self.lock();
        let mut statement = connection.prepare(
            "SELECT detail_json FROM audit WHERE project_id = ?1 AND run_id = ?2 \
             AND kind = 'version' ORDER BY id",
        )?;
        let rows =
            statement.query_map(params![project_id, run_id], |row| row.get::<_, String>(0))?;
        let mut numbers = Vec::new();
        for detail in rows {
            let number = serde_json::from_str::<Value>(&detail?)
                .ok()
                .and_then(|detail| detail.get("version").and_then(Value::as_u64))
                .and_then(|number| u32::try_from(number).ok());
            if let Some(number) = number
                && !numbers.contains(&number)
            {
                numbers.push(number);
            }
        }
        Ok(numbers)
    }

    // ---- audit ----------------------------------------------------------------------

    /// Appends an audit entry. Oversized detail is replaced by a note, never stored.
    pub fn audit(&self, entry: NewAudit) -> Result<(), StoreError> {
        let mut detail = entry.detail.to_string();
        if detail.len() > MAX_AUDIT_DETAIL_BYTES {
            detail = serde_json::json!({"dropped": "detail too large", "bytes": detail.len()})
                .to_string();
        }
        self.lock().execute(
            "INSERT INTO audit (at_ms, project_id, run_id, kind, name, args_sha256, outcome, \
             detail_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                now_ms(),
                entry.project_id,
                entry.run_id,
                entry.kind,
                entry.name,
                entry.args_sha256,
                entry.outcome,
                detail
            ],
        )?;
        Ok(())
    }

    /// The newest `limit` audit entries (of one project when given), newest first.
    pub fn list_audit(
        &self,
        project_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, StoreError> {
        let connection = self.lock();
        let mut statement = connection.prepare(
            "SELECT id, at_ms, project_id, run_id, kind, name, args_sha256, outcome, detail_json \
             FROM audit WHERE (?1 IS NULL OR project_id = ?1) ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![project_id, limit], |row| {
            let detail: String = row.get(8)?;
            Ok(AuditEntry {
                id: row.get(0)?,
                at_ms: row.get(1)?,
                project_id: row.get(2)?,
                run_id: row.get(3)?,
                kind: row.get(4)?,
                name: row.get(5)?,
                args_sha256: row.get(6)?,
                outcome: row.get(7)?,
                detail: serde_json::from_str(&detail).unwrap_or(Value::Null),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    // ---- settings -------------------------------------------------------------------

    /// One stored setting, or `None` when it was never set.
    pub fn setting(&self, key: &str) -> Result<Option<Value>, StoreError> {
        let json: Option<String> = self
            .lock()
            .query_row(
                "SELECT value_json FROM settings WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| {
            serde_json::from_str(&json).map_err(|source| StoreError::Corrupt {
                what: "a setting",
                source,
            })
        })
        .transpose()
    }

    /// Stores a setting.
    pub fn set_setting(&self, key: &str, value: &Value) -> Result<(), StoreError> {
        self.lock().execute(
            "INSERT INTO settings (key, value_json) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
            params![key, value.to_string()],
        )?;
        Ok(())
    }

    // ---- slices and pending print jobs ----------------------------------------------

    /// Records (or replaces) the slice of a version with a profile.
    pub fn put_slice(&self, record: &SliceRecord) -> Result<(), StoreError> {
        self.lock().execute(
            "INSERT OR REPLACE INTO slices (project_id, version, profile_hash, profile, \
             summary_json, created_at_ms, job_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.project_id,
                record.version,
                record.profile_hash,
                record.profile,
                record.summary.to_string(),
                record.created_at_ms,
                record.job.to_string()
            ],
        )?;
        Ok(())
    }

    /// The newest slice of a version, if it has been sliced.
    pub fn latest_slice(
        &self,
        project_id: &str,
        version: u32,
    ) -> Result<Option<SliceRecord>, StoreError> {
        self.find_slice(project_id, version, None)
    }

    /// One slice of a version by its profile hash.
    pub fn slice(
        &self,
        project_id: &str,
        version: u32,
        profile_hash: &str,
    ) -> Result<Option<SliceRecord>, StoreError> {
        self.find_slice(project_id, version, Some(profile_hash))
    }

    fn find_slice(
        &self,
        project_id: &str,
        version: u32,
        profile_hash: Option<&str>,
    ) -> Result<Option<SliceRecord>, StoreError> {
        let row: Option<(String, String, String, i64, String)> = self
            .lock()
            .query_row(
                "SELECT profile_hash, profile, summary_json, created_at_ms, job_json FROM slices \
                 WHERE project_id = ?1 AND version = ?2 AND (?3 IS NULL OR profile_hash = ?3) \
                 ORDER BY created_at_ms DESC LIMIT 1",
                params![project_id, version, profile_hash],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let corrupt = |source| StoreError::Corrupt {
            what: "a slice summary",
            source,
        };
        row.map(|(profile_hash, profile, summary, created_at_ms, job)| {
            Ok(SliceRecord {
                project_id: project_id.to_owned(),
                version,
                profile_hash,
                profile,
                summary: serde_json::from_str(&summary).map_err(corrupt)?,
                job: serde_json::from_str(&job).map_err(corrupt)?,
                created_at_ms,
            })
        })
        .transpose()
    }

    /// Every slice of a version, newest first.
    pub fn slices_of(
        &self,
        project_id: &str,
        version: u32,
    ) -> Result<Vec<SliceRecord>, StoreError> {
        let connection = self.lock();
        let mut statement = connection.prepare(
            "SELECT profile_hash, profile, summary_json, created_at_ms, job_json FROM slices \
             WHERE project_id = ?1 AND version = ?2 ORDER BY created_at_ms DESC",
        )?;
        let rows = statement.query_map(params![project_id, version], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let corrupt = |source| StoreError::Corrupt {
            what: "a slice summary",
            source,
        };
        rows.map(|row| {
            let (profile_hash, profile, summary, created_at_ms, job) = row?;
            Ok(SliceRecord {
                project_id: project_id.to_owned(),
                version,
                profile_hash,
                profile,
                summary: serde_json::from_str(&summary).map_err(corrupt)?,
                job: serde_json::from_str(&job).map_err(corrupt)?,
                created_at_ms,
            })
        })
        .collect()
    }

    /// Records an editable slicer project.
    pub fn put_project_export(&self, record: &ProjectExportRecord) -> Result<(), StoreError> {
        self.lock().execute(
            "INSERT INTO project_exports (id, project_id, version, plate, plan_sha256, \
             profile_hash, slice_sha256, folder, sha256, summary_json, delivery, detail_json, \
             created_at_ms, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                record.id,
                record.project_id,
                record.version,
                record.plate,
                record.plan_sha256,
                record.profile_hash,
                record.slice_sha256,
                record.folder,
                record.sha256,
                record.summary.to_string(),
                record.delivery,
                bounded_detail(&record.detail),
                record.created_at_ms,
                record.updated_at_ms
            ],
        )?;
        Ok(())
    }

    /// One editable slicer project.
    pub fn project_export(&self, id: &str) -> Result<ProjectExportRecord, StoreError> {
        let row = self
            .lock()
            .query_row(
                &format!("{PROJECT_EXPORT_COLUMNS} WHERE id = ?1"),
                [id],
                project_export_row,
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound {
                what: "editable project",
                id: truncate(id, 80),
            })?;
        project_export(row)
    }

    /// The editable slicer projects of a project, newest first.
    pub fn project_exports(
        &self,
        project_id: &str,
    ) -> Result<Vec<ProjectExportRecord>, StoreError> {
        let connection = self.lock();
        let mut statement = connection.prepare(&format!(
            "{PROJECT_EXPORT_COLUMNS} WHERE project_id = ?1 ORDER BY id DESC"
        ))?;
        let rows = statement.query_map([project_id], project_export_row)?;
        rows.map(|row| row.map_err(StoreError::from).and_then(project_export))
            .collect()
    }

    /// Stores how an editable slicer project was handed over, and returns it as it is
    /// afterwards.
    pub fn set_project_export_delivery(
        &self,
        id: &str,
        delivery: &str,
        detail: &Value,
    ) -> Result<ProjectExportRecord, StoreError> {
        let changed = self.lock().execute(
            "UPDATE project_exports SET delivery = ?2, detail_json = ?3, updated_at_ms = ?4 \
             WHERE id = ?1",
            params![id, delivery, bounded_detail(detail), now_ms()],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound {
                what: "editable project",
                id: truncate(id, 80),
            });
        }
        self.project_export(id)
    }

    /// Creates a pending print job for a stored slice.
    pub fn create_pending_print(
        &self,
        slice: &SliceRecord,
        slice_sha256: &str,
        note: &str,
    ) -> Result<PendingPrintJob, StoreError> {
        let job = PendingPrintJob {
            id: new_id(),
            project_id: slice.project_id.clone(),
            version: slice.version,
            profile_hash: slice.profile_hash.clone(),
            slice_sha256: slice_sha256.to_owned(),
            note: note.to_owned(),
            status: "pending".to_owned(),
            created_at_ms: now_ms(),
            delivery: None,
            detail: Value::Object(Default::default()),
            updated_at_ms: now_ms(),
        };
        self.lock().execute(
            "INSERT INTO pending_print_jobs (id, project_id, version, profile_hash, \
             slice_sha256, note, status, created_at_ms, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                job.id,
                job.project_id,
                job.version,
                job.profile_hash,
                job.slice_sha256,
                job.note,
                job.status,
                job.created_at_ms
            ],
        )?;
        Ok(job)
    }

    /// The print jobs of a project, newest first.
    pub fn list_pending_prints(
        &self,
        project_id: &str,
    ) -> Result<Vec<PendingPrintJob>, StoreError> {
        let connection = self.lock();
        let mut statement = connection.prepare(&format!(
            "{PRINT_JOB_COLUMNS} WHERE project_id = ?1 ORDER BY id DESC"
        ))?;
        let rows = statement.query_map([project_id], print_job_row)?;
        rows.map(|row| row.map_err(StoreError::from).and_then(print_job))
            .collect()
    }

    /// The print jobs in `status`, of every project, newest first, at most `limit`.
    pub fn print_jobs_with_status(
        &self,
        status: &str,
        limit: u32,
    ) -> Result<Vec<PendingPrintJob>, StoreError> {
        let connection = self.lock();
        let mut statement = connection.prepare(&format!(
            "{PRINT_JOB_COLUMNS} WHERE status = ?1 ORDER BY id DESC LIMIT ?2"
        ))?;
        let rows = statement.query_map(params![status, limit], print_job_row)?;
        rows.map(|row| row.map_err(StoreError::from).and_then(print_job))
            .collect()
    }

    /// One print job.
    pub fn print_job(&self, id: &str) -> Result<PendingPrintJob, StoreError> {
        let row = self
            .lock()
            .query_row(
                &format!("{PRINT_JOB_COLUMNS} WHERE id = ?1"),
                [id],
                print_job_row,
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound {
                what: "print job",
                id: truncate(id, 80),
            })?;
        print_job(row)
    }

    /// Ends every print job that is still `starting` or `printing` as `failed`, with
    /// `message` as the reason, and returns those jobs as they are afterwards. For a core
    /// that is alone in the data directory: the printer such a job ran on is gone.
    pub fn close_interrupted_prints(
        &self,
        message: &str,
    ) -> Result<Vec<PendingPrintJob>, StoreError> {
        const RUNNING: [&str; 2] = ["starting", "printing"];
        let running: Vec<PendingPrintJob> = {
            let connection = self.lock();
            let mut statement = connection.prepare(&format!(
                "{PRINT_JOB_COLUMNS} WHERE status IN (?1, ?2) ORDER BY id"
            ))?;
            let rows = statement.query_map(RUNNING, print_job_row)?;
            rows.map(|row| row.map_err(StoreError::from).and_then(print_job))
                .collect::<Result<_, _>>()?
        };
        let mut closed = Vec::new();
        for job in running {
            let mut detail = if job.detail.is_object() {
                job.detail.clone()
            } else {
                Value::Object(serde_json::Map::new())
            };
            detail["message"] = Value::from(message);
            detail["phase"] = Value::Null;
            detail["headline"] = Value::Null;
            detail["remaining_minutes"] = Value::Null;
            let (job, changed) =
                self.update_print_job(&job.id, &RUNNING, "failed", None, &detail)?;
            if changed {
                closed.push(job);
            }
        }
        Ok(closed)
    }

    /// Moves a print job to `status` if it is in one of the states `from`, and stores the
    /// delivery (when given) and the detail with it. Returns the job as it is afterwards
    /// and whether this call changed it.
    pub fn update_print_job(
        &self,
        id: &str,
        from: &[&str],
        status: &str,
        delivery: Option<&str>,
        detail: &Value,
    ) -> Result<(PendingPrintJob, bool), StoreError> {
        let current = self.print_job(id)?;
        if !from.contains(&current.status.as_str()) {
            return Ok((current, false));
        }
        let changed = self.lock().execute(
            "UPDATE pending_print_jobs SET status = ?2, delivery = COALESCE(?3, delivery), \
             detail_json = ?4, updated_at_ms = ?5 WHERE id = ?1 AND status = ?6",
            params![
                id,
                status,
                delivery,
                bounded_detail(detail),
                now_ms(),
                current.status
            ],
        )?;
        Ok((self.print_job(id)?, changed == 1))
    }

    /// Puts a job that a delivery claimed (`starting`) back to `pending`, with no delivery
    /// and no detail, as it was before the claim. Returns whether this call changed it.
    pub fn release_print_job(&self, id: &str) -> Result<bool, StoreError> {
        let changed = self.lock().execute(
            "UPDATE pending_print_jobs SET status = 'pending', delivery = NULL, \
             detail_json = '{}', updated_at_ms = ?2 WHERE id = ?1 AND status = 'starting'",
            params![id, now_ms()],
        )?;
        Ok(changed == 1)
    }
}

const PRINT_JOB_COLUMNS: &str = "SELECT id, project_id, version, profile_hash, slice_sha256, \
                                 note, status, created_at_ms, delivery, detail_json, \
                                 updated_at_ms FROM pending_print_jobs";

type PrintJobRow = (PendingPrintJob, String);

fn print_job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PrintJobRow> {
    Ok((
        PendingPrintJob {
            id: row.get(0)?,
            project_id: row.get(1)?,
            version: row.get(2)?,
            profile_hash: row.get(3)?,
            slice_sha256: row.get(4)?,
            note: row.get(5)?,
            status: row.get(6)?,
            created_at_ms: row.get(7)?,
            delivery: row.get(8)?,
            detail: Value::Null,
            updated_at_ms: row.get(10)?,
        },
        row.get(9)?,
    ))
}

fn print_job((mut job, detail): PrintJobRow) -> Result<PendingPrintJob, StoreError> {
    job.detail = serde_json::from_str(&detail).map_err(|source| StoreError::Corrupt {
        what: "a print job",
        source,
    })?;
    Ok(job)
}

const PROJECT_EXPORT_COLUMNS: &str = "SELECT id, project_id, version, plate, plan_sha256, \
                                      profile_hash, slice_sha256, folder, sha256, \
                                      summary_json, delivery, detail_json, created_at_ms, \
                                      updated_at_ms FROM project_exports";

type ProjectExportRow = (ProjectExportRecord, String, String);

fn project_export_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectExportRow> {
    Ok((
        ProjectExportRecord {
            id: row.get(0)?,
            project_id: row.get(1)?,
            version: row.get(2)?,
            plate: row.get(3)?,
            plan_sha256: row.get(4)?,
            profile_hash: row.get(5)?,
            slice_sha256: row.get(6)?,
            folder: row.get(7)?,
            sha256: row.get(8)?,
            summary: Value::Null,
            delivery: row.get(10)?,
            detail: Value::Null,
            created_at_ms: row.get(12)?,
            updated_at_ms: row.get(13)?,
        },
        row.get(9)?,
        row.get(11)?,
    ))
}

fn project_export(
    (mut record, summary, detail): ProjectExportRow,
) -> Result<ProjectExportRecord, StoreError> {
    let corrupt = |source| StoreError::Corrupt {
        what: "an editable project",
        source,
    };
    record.summary = serde_json::from_str(&summary).map_err(corrupt)?;
    record.detail = serde_json::from_str(&detail).map_err(corrupt)?;
    Ok(record)
}

/// A print job's detail as stored: dropped when it is unreasonably large.
fn bounded_detail(detail: &Value) -> String {
    let text = detail.to_string();
    if text.len() > 256 * 1024 {
        "{}".to_owned()
    } else {
        text
    }
}

const VERSION_COLUMNS: &str = "SELECT project_id, number, created_at_ms, parent, origin, note, \
                               status, code_sha256, params_json FROM versions";

fn migrate(connection: &mut Connection) -> Result<(), StoreError> {
    let supported = MIGRATIONS.len() as i64;
    let found: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if found > supported {
        return Err(StoreError::TooNew { found, supported });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(found.max(0) as usize) {
        let transaction = connection.transaction()?;
        transaction.execute_batch(sql)?;
        transaction.pragma_update(None, "user_version", index as i64 + 1)?;
        transaction.commit()?;
    }
    Ok(())
}

fn not_found(what: &'static str, id: &str) -> StoreError {
    StoreError::NotFound {
        what,
        id: truncate(id, 80),
    }
}

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at_ms: row.get(2)?,
        updated_at_ms: row.get(3)?,
        current_version: row.get(4)?,
    })
}

fn version_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Version, StoreError>> {
    let status: String = row.get(6)?;
    let params: String = row.get(8)?;
    let version = Version {
        project_id: row.get(0)?,
        number: row.get(1)?,
        created_at_ms: row.get(2)?,
        parent: row.get(3)?,
        origin: row.get(4)?,
        note: row.get(5)?,
        status: if status == "ok" {
            VersionStatus::Ok
        } else {
            VersionStatus::CheckError
        },
        code_sha256: row.get(7)?,
        params: BTreeMap::new(),
    };
    Ok(serde_json::from_str(&params)
        .map(|params| Version { params, ..version })
        .map_err(|source| StoreError::Corrupt {
            what: "version parameters",
            source,
        }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, DataDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataDir::resolve(Some(tmp.path())).unwrap();
        let store = Store::open(&data.db_path()).unwrap();
        (tmp, data, store)
    }

    #[test]
    fn a_pending_audit_entry_is_written_however_the_operation_ends() {
        let (_tmp, _data, store) = fixture();
        let entry = |name: &str| NewAudit {
            project_id: None,
            run_id: Some("run".to_owned()),
            kind: "tool",
            name: name.to_owned(),
            args_sha256: None,
            outcome: String::new(),
            detail: json!({}),
        };
        {
            let mut finished = PendingAudit::new(&store, entry("finished"));
            finished.detail()["status"] = json!("ok");
            finished.finish("ok");
        }
        // Dropped half-way, as when the future of a call is dropped.
        drop(PendingAudit::new(&store, entry("dropped")));
        {
            let mut suppressed = PendingAudit::new(&store, entry("suppressed"));
            suppressed.finish("rate_limited");
            suppressed.suppress();
        }
        let audit = store.list_audit(None, 10).unwrap();
        let mut rows: Vec<(&str, &str)> = audit
            .iter()
            .map(|a| (a.name.as_str(), a.outcome.as_str()))
            .collect();
        rows.sort_unstable();
        assert_eq!(rows, [("dropped", "abandoned"), ("finished", "ok")]);
        let finished = audit.iter().find(|a| a.name == "finished").unwrap();
        assert_eq!(finished.detail["status"], "ok");
        assert!(finished.detail["duration_ms"].is_u64());
    }

    fn new_version(note: &str) -> NewVersion {
        NewVersion {
            parent: None,
            origin: "write_model_code".into(),
            note: note.into(),
            status: VersionStatus::Ok,
            code_sha256: "00".into(),
            params: BTreeMap::from([("width".to_owned(), 90.0)]),
        }
    }

    fn staged(data: &DataDir, marker: &str) -> PathBuf {
        let dir = data.jobs_dir().join(format!("staged-{marker}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.py"), marker).unwrap();
        dir
    }

    #[test]
    fn migrations_run_once_and_a_newer_database_is_refused() {
        let (_tmp, data, store) = fixture();
        drop(store);
        let store = Store::open(&data.db_path()).unwrap();
        let version: i64 = store
            .lock()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        store
            .lock()
            .pragma_update(None, "user_version", 99)
            .unwrap();
        drop(store);
        assert!(matches!(
            Store::open(&data.db_path()),
            Err(StoreError::TooNew { found: 99, .. })
        ));
    }

    #[test]
    fn projects_round_trip_and_names_are_bounded() {
        let (_tmp, _data, store) = fixture();
        let project = store.create_project("  Cable box ").unwrap();
        assert_eq!(project.name, "Cable box");
        assert_eq!(store.project(&project.id).unwrap(), project);
        assert_eq!(store.list_projects().unwrap(), vec![project]);
        assert!(matches!(
            store.create_project(" "),
            Err(StoreError::Invalid(_))
        ));
        assert!(
            store
                .create_project(&"x".repeat(MAX_PROJECT_NAME + 1))
                .is_err()
        );
        assert!(matches!(
            store.project("missing"),
            Err(StoreError::NotFound {
                what: "project",
                ..
            })
        ));
    }

    #[test]
    fn versions_are_numbered_moved_into_place_and_immutable() {
        let (_tmp, data, store) = fixture();
        let project = store.create_project("p").unwrap();
        let first = store
            .create_version(
                &data,
                &project.id,
                &staged(&data, "one"),
                new_version("first"),
            )
            .unwrap();
        let second = store
            .create_version(
                &data,
                &project.id,
                &staged(&data, "two"),
                new_version("second"),
            )
            .unwrap();
        assert_eq!((first.number, second.number), (1, 2));
        let folder = data.version_dir(&project.id, 2).unwrap();
        assert_eq!(
            std::fs::read_to_string(folder.join("model.py")).unwrap(),
            "two"
        );
        assert!(!data.jobs_dir().join("staged-two").exists());
        assert_eq!(store.project(&project.id).unwrap().current_version, Some(2));
        assert_eq!(store.version(&project.id, 1).unwrap(), first);
        assert_eq!(
            store.list_versions(&project.id).unwrap(),
            vec![first, second]
        );

        let connection = store.lock();
        let update = connection.execute("UPDATE versions SET note = 'changed'", []);
        assert!(update.unwrap_err().to_string().contains("immutable"));
        let delete = connection.execute("DELETE FROM versions", []);
        assert!(delete.unwrap_err().to_string().contains("immutable"));
    }

    #[test]
    fn a_version_for_an_unknown_project_leaves_nothing_behind() {
        let (_tmp, data, store) = fixture();
        let dir = staged(&data, "orphan");
        let error = store
            .create_version(&data, &crate::util::new_id(), &dir, new_version("x"))
            .unwrap_err();
        assert!(matches!(error, StoreError::NotFound { .. }));
        assert!(
            dir.exists(),
            "the staged folder must stay for the caller to remove"
        );
    }

    #[test]
    fn current_version_can_point_at_any_existing_version_only() {
        let (_tmp, data, store) = fixture();
        let project = store.create_project("p").unwrap();
        for marker in ["a", "b"] {
            store
                .create_version(
                    &data,
                    &project.id,
                    &staged(&data, marker),
                    new_version(marker),
                )
                .unwrap();
        }
        store.set_current_version(&project.id, 1).unwrap();
        assert_eq!(store.project(&project.id).unwrap().current_version, Some(1));
        assert!(store.set_current_version(&project.id, 7).is_err());
    }

    #[test]
    fn history_transcript_settings_and_audit_round_trip() {
        let (_tmp, _data, store) = fixture();
        let project = store.create_project("p").unwrap();
        assert_eq!(store.history(&project.id).unwrap(), None);
        let history = vec![json!({"role": "user", "content": "hi"})];
        store
            .set_history(&project.id, "anthropic", &history)
            .unwrap();
        store
            .set_history(&project.id, "anthropic", &history)
            .unwrap();
        assert_eq!(
            store.history(&project.id).unwrap(),
            Some(("anthropic".to_owned(), history))
        );
        store
            .append_transcript(&project.id, "r1", "user", "hi")
            .unwrap();
        assert_eq!(store.transcript(&project.id).unwrap()[0].text, "hi");

        assert_eq!(store.setting("k").unwrap(), None);
        store.set_setting("k", &json!({"a": 1})).unwrap();
        store.set_setting("k", &json!({"a": 2})).unwrap();
        assert_eq!(store.setting("k").unwrap(), Some(json!({"a": 2})));

        for outcome in ["ok", "denied"] {
            store
                .audit(NewAudit {
                    project_id: Some(project.id.clone()),
                    kind: "tool",
                    name: "measure".into(),
                    args_sha256: Some("ab".into()),
                    outcome: outcome.into(),
                    detail: json!({"n": 1}),
                    ..NewAudit::default()
                })
                .unwrap();
        }
        store
            .audit(NewAudit {
                kind: "job",
                name: "big".into(),
                outcome: "ok".into(),
                detail: json!({"blob": "x".repeat(MAX_AUDIT_DETAIL_BYTES)}),
                ..NewAudit::default()
            })
            .unwrap();
        let mine = store.list_audit(Some(&project.id), 10).unwrap();
        assert_eq!(mine.len(), 2);
        assert_eq!(mine[0].outcome, "denied");
        let all = store.list_audit(None, 10).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].detail["dropped"], "detail too large");
    }

    #[test]
    fn pending_print_jobs_need_a_stored_slice() {
        let (_tmp, data, store) = fixture();
        let project = store.create_project("p").unwrap();
        store
            .create_version(&data, &project.id, &staged(&data, "s"), new_version("s"))
            .unwrap();
        assert_eq!(store.latest_slice(&project.id, 1).unwrap(), None);
        let record = SliceRecord {
            project_id: project.id.clone(),
            version: 1,
            profile_hash: "abc".into(),
            profile: "0.20mm".into(),
            summary: json!({"time_s": 60}),
            job: json!({"plate": 1}),
            created_at_ms: 1,
        };
        assert!(store.create_pending_print(&record, "ff", "note").is_err());
        store.put_slice(&record).unwrap();
        assert_eq!(
            store.latest_slice(&project.id, 1).unwrap(),
            Some(record.clone())
        );
        let job = store.create_pending_print(&record, "ff", "note").unwrap();
        assert_eq!(job.status, "pending");
        assert_eq!(
            store.list_pending_prints(&project.id).unwrap(),
            vec![job.clone()]
        );
        // A job moves on only from a state the caller names.
        let detail = json!({"phase": "printing"});
        let (unchanged, changed) = store
            .update_print_job(&job.id, &["starting"], "printing", None, &detail)
            .unwrap();
        assert!(!changed && unchanged.status == "pending");
        let (moved, changed) = store
            .update_print_job(
                &job.id,
                &["pending"],
                "starting",
                Some("simulated_printer"),
                &detail,
            )
            .unwrap();
        assert!(changed);
        assert_eq!(moved.status, "starting");
        assert_eq!(moved.delivery.as_deref(), Some("simulated_printer"));
        assert_eq!(store.print_job(&job.id).unwrap(), moved);
        assert!(store.print_job("missing").is_err());

        // A job that was running when its core went away is ended as failed, once; a job
        // nobody confirmed stays as it is.
        let waiting = store.create_pending_print(&record, "ff", "later").unwrap();
        let closed = store
            .close_interrupted_prints("the printer is gone")
            .unwrap();
        assert_eq!(closed.len(), 1);
        assert_eq!(
            (closed[0].id.as_str(), closed[0].status.as_str()),
            (job.id.as_str(), "failed")
        );
        assert_eq!(closed[0].detail["message"], "the printer is gone");
        assert_eq!(closed[0].detail["phase"], Value::Null);
        assert_eq!(closed[0].delivery.as_deref(), Some("simulated_printer"));
        assert_eq!(store.print_job(&waiting.id).unwrap().status, "pending");
        assert!(store.close_interrupted_prints("again").unwrap().is_empty());
    }
}
