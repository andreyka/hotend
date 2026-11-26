//! The store's tables for web research: what a project may do and has used, the numbers
//! found on the web (notes), the pages they came from and the log of searches and fetches.
//!
//! Guarantees:
//! - Counters only grow. Nothing here lowers what a project has used.
//! - A project keeps its newest [`MAX_SOURCES`] pages and [`MAX_LOG`] log lines; a page that
//!   a note refers to is never dropped.
//! - Every text column is bounded. The texts come from web pages: they are stored as data
//!   and never interpreted.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Store, StoreError, not_found};
use crate::util::now_ms;

/// Pages kept per project.
pub const MAX_SOURCES: usize = 300;
/// Log lines kept per project.
pub const MAX_LOG: usize = 500;
/// Citations kept per page.
const MAX_CITATIONS: usize = 24;

/// A project's research row: its own settings and what it has used so far.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResearchRow {
    /// The project's settings as stored (see `research::ProjectResearch`).
    pub settings: Value,
    pub searches: u32,
    pub fetches: u32,
    /// Searches the provider reported as billed.
    pub billed_searches: u32,
    /// Bytes of web results that entered the project's conversation.
    pub result_bytes: u64,
    /// The provider said web search is switched off for the account.
    pub refused_search: bool,
    /// The provider said web fetch is switched off for the account.
    pub refused_fetch: bool,
    /// Hash of the notes block the model was last shown.
    pub notes_shown_hash: String,
    /// Hash of the instructions and tools of the last stored run.
    pub shape_hash: String,
}

/// One number found on the web, with where it came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchNote {
    /// `r1`, `r2`, ...: unique in the project.
    pub id: String,
    /// What the number is, as a short name (`phone_width`).
    pub key: String,
    pub label: String,
    pub value: f64,
    /// `mm`, `deg`, `count`, `ratio` or `g`.
    pub unit: String,
    /// The page, as Hotend cleaned the address.
    pub url: String,
    /// The page's title, as the page gave it.
    pub title: String,
    /// The sentence the number was read from, as the assistant quoted it.
    pub quote: String,
    /// Where on the page (a table, a section), in the assistant's words. May be empty.
    pub locator: String,
    /// How far Hotend could check the quote: `page_text`, `citation` or `none`.
    pub evidence: String,
    /// `unconfirmed`, `confirmed`, `rejected`, `superseded` or `expired`.
    pub state: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// What is needed to store a note; the store assigns the id.
#[derive(Debug, Clone)]
pub struct NewResearchNote {
    pub key: String,
    pub label: String,
    pub value: f64,
    pub unit: String,
    pub url: String,
    pub title: String,
    pub quote: String,
    pub locator: String,
    pub evidence: String,
}

/// A page the provider found or read for a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchSource {
    pub url: String,
    pub title: String,
    /// `search` or `fetch`: how the page was last seen.
    pub via: String,
    /// The text of the fetched page, when Hotend holds it. Only for checking quotes.
    #[serde(skip)]
    pub page_text: Option<String>,
    /// Whether Hotend holds the page's text.
    pub has_page_text: bool,
    /// Passages the provider cited from the page.
    pub citations: Vec<String>,
    pub run_id: String,
    pub seen_at_ms: i64,
}

/// One search or fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchLogEntry {
    pub at_ms: i64,
    pub run_id: String,
    /// `search` or `fetch`.
    pub kind: String,
    /// The query, or the page address without its query string.
    pub text: String,
    /// `ok`, `3 results`, `pending`, or the provider's error code.
    pub outcome: String,
}

fn note_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResearchNote> {
    Ok(ResearchNote {
        id: row.get(0)?,
        key: row.get(1)?,
        label: row.get(2)?,
        value: row.get(3)?,
        unit: row.get(4)?,
        url: row.get(5)?,
        title: row.get(6)?,
        quote: row.get(7)?,
        locator: row.get(8)?,
        evidence: row.get(9)?,
        state: row.get(10)?,
        created_at_ms: row.get(11)?,
        updated_at_ms: row.get(12)?,
    })
}

const NOTE_COLUMNS: &str = "id, key, label, value, unit, url, title, quote, locator, evidence, \
                            state, created_at_ms, updated_at_ms";
const SOURCE_COLUMNS: &str = "url, title, via, page_text, citations_json, run_id, seen_at_ms";

fn source_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResearchSource> {
    let page_text: Option<String> = row.get(3)?;
    let citations: String = row.get(4)?;
    Ok(ResearchSource {
        url: row.get(0)?,
        title: row.get(1)?,
        via: row.get(2)?,
        has_page_text: page_text.is_some(),
        page_text,
        citations: serde_json::from_str(&citations).unwrap_or_default(),
        run_id: row.get(5)?,
        seen_at_ms: row.get(6)?,
    })
}

impl Store {
    /// Makes sure the project has a research row. Fails for an unknown project.
    fn ensure_research_row(&self, project_id: &str) -> Result<(), StoreError> {
        self.project(project_id)?;
        self.lock().execute(
            "INSERT INTO project_research (project_id) VALUES (?1) \
             ON CONFLICT(project_id) DO NOTHING",
            [project_id],
        )?;
        Ok(())
    }

    /// A project's research settings and counters; the defaults for a project that never
    /// used research.
    pub fn research_row(&self, project_id: &str) -> Result<ResearchRow, StoreError> {
        self.project(project_id)?;
        let row = self
            .lock()
            .query_row(
                "SELECT settings_json, searches, fetches, billed_searches, result_bytes, \
                 refused_search, refused_fetch, notes_shown_hash, shape_hash \
                 FROM project_research WHERE project_id = ?1",
                [project_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        ResearchRow {
                            settings: Value::Null,
                            searches: row.get(1)?,
                            fetches: row.get(2)?,
                            billed_searches: row.get(3)?,
                            result_bytes: row.get::<_, i64>(4)?.max(0) as u64,
                            refused_search: row.get(5)?,
                            refused_fetch: row.get(6)?,
                            notes_shown_hash: row.get(7)?,
                            shape_hash: row.get(8)?,
                        },
                    ))
                },
            )
            .optional()?;
        let Some((settings, mut row)) = row else {
            return Ok(ResearchRow {
                settings: Value::Object(serde_json::Map::new()),
                ..ResearchRow::default()
            });
        };
        row.settings = serde_json::from_str(&settings).map_err(|source| StoreError::Corrupt {
            what: "a project's research settings",
            source,
        })?;
        Ok(row)
    }

    /// Replaces a project's research settings.
    pub fn set_research_settings(
        &self,
        project_id: &str,
        settings: &Value,
    ) -> Result<(), StoreError> {
        self.ensure_research_row(project_id)?;
        self.lock().execute(
            "UPDATE project_research SET settings_json = ?2 WHERE project_id = ?1",
            params![project_id, settings.to_string()],
        )?;
        Ok(())
    }

    /// Adds what one response used to the project's counters.
    pub fn add_research_use(
        &self,
        project_id: &str,
        searches: u32,
        fetches: u32,
        billed_searches: u32,
        result_bytes: u64,
    ) -> Result<(), StoreError> {
        self.ensure_research_row(project_id)?;
        self.lock().execute(
            "UPDATE project_research SET searches = searches + ?2, fetches = fetches + ?3, \
             billed_searches = billed_searches + ?4, result_bytes = result_bytes + ?5 \
             WHERE project_id = ?1",
            params![
                project_id,
                searches,
                fetches,
                billed_searches,
                i64::try_from(result_bytes).unwrap_or(i64::MAX)
            ],
        )?;
        Ok(())
    }

    /// Records that the provider refused (or no longer refuses) one of the two web tools.
    pub fn set_research_refused(
        &self,
        project_id: &str,
        fetch: bool,
        refused: bool,
    ) -> Result<(), StoreError> {
        self.ensure_research_row(project_id)?;
        let column = if fetch {
            "refused_fetch"
        } else {
            "refused_search"
        };
        self.lock().execute(
            &format!("UPDATE project_research SET {column} = ?2 WHERE project_id = ?1"),
            params![project_id, refused],
        )?;
        Ok(())
    }

    /// Stores the hashes of the notes block and of the request shape of the last stored run.
    /// A value that is `None` keeps what is stored.
    pub fn set_research_hashes(
        &self,
        project_id: &str,
        notes_shown: Option<&str>,
        shape: Option<&str>,
    ) -> Result<(), StoreError> {
        self.ensure_research_row(project_id)?;
        self.lock().execute(
            "UPDATE project_research SET notes_shown_hash = COALESCE(?2, notes_shown_hash), \
             shape_hash = COALESCE(?3, shape_hash) WHERE project_id = ?1",
            params![project_id, notes_shown, shape],
        )?;
        Ok(())
    }

    /// Records a page the provider found or read. A page seen again keeps its text and
    /// citations and moves to the front.
    pub fn put_research_source(
        &self,
        project_id: &str,
        run_id: &str,
        url: &str,
        title: &str,
        via: &str,
    ) -> Result<(), StoreError> {
        self.project(project_id)?;
        let connection = self.lock();
        // Replacing the row gives it a new sequence number: newest last.
        let old: Option<(Option<String>, String, String)> = connection
            .query_row(
                "SELECT page_text, citations_json, title FROM research_sources \
                 WHERE project_id = ?1 AND url = ?2",
                params![project_id, url],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (page_text, citations, old_title) =
            old.unwrap_or((None, "[]".to_owned(), String::new()));
        let title = if title.is_empty() { &old_title } else { title };
        connection.execute(
            "INSERT OR REPLACE INTO research_sources \
             (project_id, url, title, via, page_text, citations_json, run_id, seen_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                project_id,
                url,
                title,
                via,
                page_text,
                citations,
                run_id,
                now_ms()
            ],
        )?;
        connection.execute(
            "DELETE FROM research_sources WHERE project_id = ?1 AND seq NOT IN \
             (SELECT seq FROM research_sources WHERE project_id = ?1 \
              ORDER BY seq DESC LIMIT ?2) \
             AND url NOT IN (SELECT url FROM research_notes WHERE project_id = ?1)",
            params![project_id, MAX_SOURCES as i64],
        )?;
        Ok(())
    }

    /// Keeps the text of a fetched page, for checking quotes against it.
    pub fn set_research_page_text(
        &self,
        project_id: &str,
        url: &str,
        text: &str,
    ) -> Result<(), StoreError> {
        self.lock().execute(
            "UPDATE research_sources SET page_text = ?3 WHERE project_id = ?1 AND url = ?2",
            params![project_id, url, text],
        )?;
        Ok(())
    }

    /// Adds a passage the provider cited from a page.
    pub fn add_research_citation(
        &self,
        project_id: &str,
        url: &str,
        text: &str,
    ) -> Result<(), StoreError> {
        let connection = self.lock();
        let stored: Option<String> = connection
            .query_row(
                "SELECT citations_json FROM research_sources WHERE project_id = ?1 AND url = ?2",
                params![project_id, url],
                |row| row.get(0),
            )
            .optional()?;
        let Some(stored) = stored else {
            return Ok(());
        };
        let mut citations: Vec<String> = serde_json::from_str(&stored).unwrap_or_default();
        if citations.iter().any(|known| known == text) {
            return Ok(());
        }
        if citations.len() >= MAX_CITATIONS {
            citations.remove(0);
        }
        citations.push(text.to_owned());
        connection.execute(
            "UPDATE research_sources SET citations_json = ?3 WHERE project_id = ?1 AND url = ?2",
            params![project_id, url, Value::from(citations).to_string()],
        )?;
        Ok(())
    }

    /// One page of a project, with its text.
    pub fn research_source(
        &self,
        project_id: &str,
        url: &str,
    ) -> Result<Option<ResearchSource>, StoreError> {
        Ok(self
            .lock()
            .query_row(
                &format!(
                    "SELECT {SOURCE_COLUMNS} FROM research_sources \
                     WHERE project_id = ?1 AND url = ?2"
                ),
                params![project_id, url],
                source_from_row,
            )
            .optional()?)
    }

    /// The pages of a project, newest first, without their text.
    pub fn research_sources(&self, project_id: &str) -> Result<Vec<ResearchSource>, StoreError> {
        self.project(project_id)?;
        let connection = self.lock();
        let mut statement = connection.prepare(
            "SELECT url, title, via, CASE WHEN page_text IS NULL THEN NULL ELSE '' END, \
             citations_json, run_id, seen_at_ms FROM research_sources \
             WHERE project_id = ?1 ORDER BY seq DESC",
        )?;
        let rows = statement.query_map([project_id], source_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Appends a line to the project's research log.
    pub fn append_research_log(
        &self,
        project_id: &str,
        run_id: &str,
        kind: &str,
        text: &str,
        outcome: &str,
    ) -> Result<(), StoreError> {
        self.project(project_id)?;
        let connection = self.lock();
        connection.execute(
            "INSERT INTO research_log (project_id, run_id, kind, text, outcome, at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![project_id, run_id, kind, text, outcome, now_ms()],
        )?;
        connection.execute(
            "DELETE FROM research_log WHERE project_id = ?1 AND seq NOT IN \
             (SELECT seq FROM research_log WHERE project_id = ?1 ORDER BY seq DESC LIMIT ?2)",
            params![project_id, MAX_LOG as i64],
        )?;
        Ok(())
    }

    /// The project's research log, oldest first.
    pub fn research_log(&self, project_id: &str) -> Result<Vec<ResearchLogEntry>, StoreError> {
        self.project(project_id)?;
        let connection = self.lock();
        let mut statement = connection.prepare(
            "SELECT at_ms, run_id, kind, text, outcome FROM research_log \
             WHERE project_id = ?1 ORDER BY seq",
        )?;
        let rows = statement.query_map([project_id], |row| {
            Ok(ResearchLogEntry {
                at_ms: row.get(0)?,
                run_id: row.get(1)?,
                kind: row.get(2)?,
                text: row.get(3)?,
                outcome: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Stores a note under the project's next id (`r1`, `r2`, ...).
    pub fn add_research_note(
        &self,
        project_id: &str,
        note: NewResearchNote,
    ) -> Result<ResearchNote, StoreError> {
        self.ensure_research_row(project_id)?;
        let mut connection = self.lock();
        let transaction = connection.transaction()?;
        let number: u32 = transaction.query_row(
            "SELECT next_note FROM project_research WHERE project_id = ?1",
            [project_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE project_research SET next_note = next_note + 1 WHERE project_id = ?1",
            [project_id],
        )?;
        let now = now_ms();
        let stored = ResearchNote {
            id: format!("r{number}"),
            key: note.key,
            label: note.label,
            value: note.value,
            unit: note.unit,
            url: note.url,
            title: note.title,
            quote: note.quote,
            locator: note.locator,
            evidence: note.evidence,
            state: "unconfirmed".to_owned(),
            created_at_ms: now,
            updated_at_ms: now,
        };
        transaction.execute(
            &format!(
                "INSERT INTO research_notes (project_id, {NOTE_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
            ),
            params![
                project_id,
                stored.id,
                stored.key,
                stored.label,
                stored.value,
                stored.unit,
                stored.url,
                stored.title,
                stored.quote,
                stored.locator,
                stored.evidence,
                stored.state,
                stored.created_at_ms,
                stored.updated_at_ms
            ],
        )?;
        transaction.commit()?;
        Ok(stored)
    }

    /// The notes of a project in the order they were made, whatever their state.
    pub fn research_notes(&self, project_id: &str) -> Result<Vec<ResearchNote>, StoreError> {
        self.project(project_id)?;
        let connection = self.lock();
        let mut statement = connection.prepare(&format!(
            "SELECT {NOTE_COLUMNS} FROM research_notes WHERE project_id = ?1 \
             ORDER BY created_at_ms, CAST(substr(id, 2) AS INTEGER)"
        ))?;
        let rows = statement.query_map([project_id], note_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Gives one note another source (label, page, quote, evidence) and returns it. Its key,
    /// value, unit and state stay.
    pub fn set_research_note_source(
        &self,
        project_id: &str,
        id: &str,
        source: NewResearchNote,
    ) -> Result<ResearchNote, StoreError> {
        let connection = self.lock();
        let changed = connection.execute(
            "UPDATE research_notes SET label = ?3, url = ?4, title = ?5, quote = ?6, \
             locator = ?7, evidence = ?8, updated_at_ms = ?9 \
             WHERE project_id = ?1 AND id = ?2",
            params![
                project_id,
                id,
                source.label,
                source.url,
                source.title,
                source.quote,
                source.locator,
                source.evidence,
                now_ms()
            ],
        )?;
        if changed == 0 {
            return Err(not_found("research note", id));
        }
        Ok(connection.query_row(
            &format!("SELECT {NOTE_COLUMNS} FROM research_notes WHERE project_id = ?1 AND id = ?2"),
            params![project_id, id],
            note_from_row,
        )?)
    }

    /// Changes the state of one note and returns it.
    pub fn set_research_note_state(
        &self,
        project_id: &str,
        id: &str,
        state: &str,
    ) -> Result<ResearchNote, StoreError> {
        let connection = self.lock();
        let changed = connection.execute(
            "UPDATE research_notes SET state = ?3, updated_at_ms = ?4 \
             WHERE project_id = ?1 AND id = ?2",
            params![project_id, id, state, now_ms()],
        )?;
        if changed == 0 {
            return Err(not_found("research note", id));
        }
        Ok(connection.query_row(
            &format!("SELECT {NOTE_COLUMNS} FROM research_notes WHERE project_id = ?1 AND id = ?2"),
            params![project_id, id],
            note_from_row,
        )?)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::paths::DataDir;

    fn store() -> (tempfile::TempDir, Store, String) {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataDir::resolve(Some(tmp.path())).unwrap();
        let store = Store::open(&data.db_path()).unwrap();
        let project = store.create_project("stand").unwrap().id;
        (tmp, store, project)
    }

    fn note(key: &str, value: f64) -> NewResearchNote {
        NewResearchNote {
            key: key.to_owned(),
            label: "Width".to_owned(),
            value,
            unit: "mm".to_owned(),
            url: "https://maker.example/specs".to_owned(),
            title: "Specs".to_owned(),
            quote: format!("Width: {value} mm"),
            locator: String::new(),
            evidence: "page_text".to_owned(),
        }
    }

    #[test]
    fn a_project_without_research_has_an_empty_row_and_counters_only_grow() {
        let (_tmp, store, project) = store();
        let row = store.research_row(&project).unwrap();
        assert_eq!(row.settings, serde_json::json!({}));
        assert_eq!((row.searches, row.fetches, row.result_bytes), (0, 0, 0));

        store.add_research_use(&project, 2, 1, 2, 9000).unwrap();
        store.add_research_use(&project, 1, 0, 1, 500).unwrap();
        store
            .set_research_settings(&project, &serde_json::json!({"enabled": false}))
            .unwrap();
        store.set_research_refused(&project, true, true).unwrap();
        store
            .set_research_hashes(&project, Some("n1"), None)
            .unwrap();
        store
            .set_research_hashes(&project, Some("n2"), Some("s1"))
            .unwrap();
        store
            .set_research_hashes(&project, Some("n3"), None)
            .unwrap();
        // Nothing new to remember keeps both.
        store.set_research_hashes(&project, None, None).unwrap();
        let row = store.research_row(&project).unwrap();
        assert_eq!(
            (
                row.searches,
                row.fetches,
                row.billed_searches,
                row.result_bytes
            ),
            (3, 1, 3, 9500)
        );
        assert_eq!(row.settings, serde_json::json!({"enabled": false}));
        assert!(row.refused_fetch && !row.refused_search);
        assert_eq!(
            (row.notes_shown_hash.as_str(), row.shape_hash.as_str()),
            ("n3", "s1")
        );

        assert!(matches!(
            store.research_row("no-such-project"),
            Err(StoreError::NotFound { .. })
        ));
        assert!(
            store
                .add_research_use("no-such-project", 1, 0, 0, 0)
                .is_err()
        );
    }

    #[test]
    fn notes_get_ids_in_order_and_keep_their_history() {
        let (_tmp, store, project) = store();
        let first = store
            .add_research_note(&project, note("width", 80.5))
            .unwrap();
        let second = store
            .add_research_note(&project, note("depth", 8.0))
            .unwrap();
        assert_eq!((first.id.as_str(), second.id.as_str()), ("r1", "r2"));
        assert_eq!(first.state, "unconfirmed");
        let changed = store
            .set_research_note_state(&project, "r1", "confirmed")
            .unwrap();
        assert_eq!(changed.state, "confirmed");
        assert!(
            store
                .set_research_note_state(&project, "r9", "confirmed")
                .is_err()
        );
        let notes = store.research_notes(&project).unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].state, "confirmed");
        // Another project sees none of it.
        let other = store.create_project("other").unwrap().id;
        assert!(store.research_notes(&other).unwrap().is_empty());
        assert_eq!(
            store.add_research_note(&other, note("w", 1.0)).unwrap().id,
            "r1"
        );
    }

    #[test]
    fn sources_keep_text_and_citations_and_old_ones_are_dropped() {
        let (_tmp, store, project) = store();
        let url = "https://maker.example/specs";
        store
            .put_research_source(&project, "run1", url, "Specs", "search")
            .unwrap();
        store
            .add_research_citation(&project, url, "Width: 80.5 mm")
            .unwrap();
        store
            .add_research_citation(&project, url, "Width: 80.5 mm")
            .unwrap();
        store
            .set_research_page_text(&project, url, "Width: 80.5 mm\nDepth: 8 mm")
            .unwrap();
        // Seen again by a fetch without a title: text, citations and title stay.
        store
            .put_research_source(&project, "run2", url, "", "fetch")
            .unwrap();
        let source = store.research_source(&project, url).unwrap().unwrap();
        assert_eq!(source.title, "Specs");
        assert_eq!(source.via, "fetch");
        assert_eq!(source.citations, ["Width: 80.5 mm"]);
        assert_eq!(
            source.page_text.as_deref(),
            Some("Width: 80.5 mm\nDepth: 8 mm")
        );
        // A citation for a page that was never seen is not stored.
        store
            .add_research_citation(&project, "https://x.example/", "x")
            .unwrap();
        assert!(
            store
                .research_source(&project, "https://x.example/")
                .unwrap()
                .is_none()
        );

        store
            .add_research_note(&project, note("width", 80.5))
            .unwrap();
        for index in 0..MAX_SOURCES + 5 {
            let other = format!("https://pages.example/{index}");
            store
                .put_research_source(&project, "run3", &other, "t", "search")
                .unwrap();
        }
        let sources = store.research_sources(&project).unwrap();
        // The newest pages, plus the one a note refers to.
        assert_eq!(sources.len(), MAX_SOURCES + 1);
        assert!(
            sources
                .iter()
                .any(|source| source.url == url && source.has_page_text)
        );
        assert!(
            sources
                .iter()
                .all(|source| source.page_text.as_deref().is_none_or(str::is_empty))
        );
        assert_eq!(
            sources[0].url,
            format!("https://pages.example/{}", MAX_SOURCES + 4)
        );
    }

    #[test]
    fn the_log_keeps_its_newest_lines() {
        let (_tmp, store, project) = store();
        for index in 0..MAX_LOG + 3 {
            store
                .append_research_log(&project, "run", "search", &format!("q{index}"), "1 results")
                .unwrap();
        }
        let log = store.research_log(&project).unwrap();
        assert_eq!(log.len(), MAX_LOG);
        assert_eq!(log[0].text, "q3");
        assert_eq!(log.last().unwrap().text, format!("q{}", MAX_LOG + 2));
    }
}
