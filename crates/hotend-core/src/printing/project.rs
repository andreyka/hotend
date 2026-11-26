//! Editable Bambu Studio projects: made for a plate whose slice passed the G-code check,
//! handed over only on a person's confirmation.
//!
//! Guarantees:
//! - A project is made only from a plate whose slice of the same print plan and layout, by
//!   the real slicer, passes the G-code check now (the check runs again; nothing is taken
//!   from an earlier review), and only when the project's own test slice (Bambu Studio's
//!   command line slicing the project file alone) passed the proof of the settings and
//!   passes the G-code check too. A failed check never becomes a project, and a project is
//!   never handed over while either check fails.
//! - Handing a project over needs the process's [`ConfirmationAuthority`] and the text the
//!   person was shown, which must contain the project's review text in one piece
//!   ([`ConfirmationRequest::project`]): the project's SHA-256, where it goes (the hand-off
//!   folder, or the folder the review resolved, and whether anything opens it), the
//!   sentence [`PROJECT_OUTSIDE_CHECK`], and the test slice's review text. The confirmation
//!   is bound to the project's hash, to that destination and to the test slice, so a yes
//!   given for one destination cannot hand the project to another; it starts nothing and
//!   opens no Bambu Connect link.
//! - The bytes handed over are the project file's bytes with the hash the backend reported
//!   and the review named: read once, hashed, written under a name made of the job's label
//!   and the first 32 hex digits of that hash, and read again after they were written and
//!   again right before the opener is asked. Any difference stops the hand-over.
//! - Every text says that a project opened this way is sliced again by Bambu Studio and is
//!   outside Hotend's check; none says that the project was checked.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, MutexGuard};

use hotend_printer::{
    CheckedJob, ConfirmationAuthority, ConfirmationRequest, HandOffFolder, PROJECT_OUTSIDE_CHECK,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    HandOffTarget, LintFinding, OPENER_TASK_FAILED, PrintError, PrintJobDetail, PrintService,
    SliceDeclaration, confirmation, delivery_error, job_name, layout_time, open_error_text,
    opener_audit, rejection, shown_review,
};
use crate::gateway::{
    MAX_SLICE_BYTES, PROJECT_DIR, ProjectExportSummary, ProjectKeys, ProjectPlan, SliceSummary,
};
use crate::import;
use crate::store::{NewAudit, ProjectExportRecord, SliceRecord};
use crate::util::{new_id, now_ms, sha256_hex, truncate};

/// The `delivery` of a project placed in the hand-off folder and given to the opener.
pub const PROJECT_TO_BAMBU_STUDIO: &str = "bambu_studio";
/// The `delivery` of a project written into a folder, with nothing opened.
pub const PROJECT_TO_FOLDER: &str = "folder";

/// Where an editable project goes when it is handed over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectDestination {
    /// Placed in the Bambu Connect hand-off folder, then the opener is asked to open it with
    /// the program the system associates with `.3mf` files (Bambu Studio). The core's
    /// opener is a stand-in that opens nothing until the application sets the real one.
    BambuStudio,
    /// Written into a folder; nothing is opened.
    Folder { folder: PathBuf },
}

impl ProjectDestination {
    /// The name stored with the project when it was handed over this way.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BambuStudio => PROJECT_TO_BAMBU_STUDIO,
            Self::Folder { .. } => PROJECT_TO_FOLDER,
        }
    }
}

/// An editable Bambu Studio project Hotend made for one plate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditableProject {
    pub id: String,
    pub project_id: String,
    pub version: u32,
    /// The plate of the version's layout, from 1.
    pub plate: u32,
    /// The plate's print plan the project carries, by its hash.
    pub plan_sha256: String,
    /// SHA-256 of the sliced file of the same plan that passed the G-code check.
    pub slice_sha256: String,
    /// The project file where Hotend keeps it:
    /// `versions/<n>/slicer-project/<plate>-<plan hash>/...`.
    pub path: PathBuf,
    /// SHA-256 of the project file.
    pub sha256: String,
    /// The keys the project lists as changed from the system presets.
    pub keys: ProjectKeys,
    /// The project sliced once on its own by Bambu Studio's command line (the test slice).
    pub test_slice: SliceSummary,
    /// The proof's verdict on the test slice (`passed`).
    pub proof: String,
    /// [`PROJECT_OUTSIDE_CHECK`]: what every interface shows with a project.
    pub notice: String,
    /// `bambu_studio` or `folder` once it was handed over.
    pub delivery: Option<String>,
    /// The last hand-over: `message`, `path` (as Hotend sees it), `windows_path`,
    /// `opened` and `opener`.
    pub detail: PrintJobDetail,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// An editable project as it is put before the person who confirms handing it over.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectReview {
    pub project: EditableProject,
    pub destination: ProjectDestination,
    /// The name the file gets where it is placed: the job's label and the first 32 hex
    /// digits of its SHA-256.
    pub file_name: String,
    /// For [`ProjectDestination::BambuStudio`]: the hand-off folder and what opens the
    /// file.
    pub handoff: Option<HandOffTarget>,
    /// The text a person confirms, written by the printer layer: the project's SHA-256,
    /// where it goes (a line starting `Where it goes:`), [`PROJECT_OUTSIDE_CHECK`] and the
    /// test slice's review text. Show it as it is, in
    /// one piece, and hand what was shown to [`crate::Core::print_project_deliver`].
    pub review_text: String,
    /// What the G-code check found in the test slice that does not block.
    pub warnings: Vec<LintFinding>,
    /// [`PROJECT_OUTSIDE_CHECK`].
    pub notice: String,
}

/// A reviewed project, kept for its confirmation.
pub(super) struct ProjectReviewState {
    test: Arc<CheckedJob>,
    /// Where it goes; a folder as it was resolved when it was reviewed.
    destination: ProjectDestination,
    /// The destination as the review text says it; the confirmation is bound to it.
    destination_text: String,
    handoff: Option<HandOffFolder>,
    /// For the hand-off folder: whether the review text said that the system is asked to
    /// open the project (the opener was not the stand-in when it was reviewed).
    opens: bool,
    sha256: String,
    path: PathBuf,
    version_dir: PathBuf,
    layout: Option<i64>,
}

/// The reviewed projects, by id.
pub(super) type ProjectReviews = HashMap<String, ProjectReviewState>;

/// The file name of a project placed for a person: the test slice's label (the job's name,
/// cleaned when it was checked) and the first 32 hex digits of the project's SHA-256, as a
/// sliced file's name is built.
fn project_file_name(test: &CheckedJob, sha256: &str) -> String {
    format!(
        "{}-{}.3mf",
        test.name().replace(' ', "_"),
        &sha256[..32.min(sha256.len())]
    )
}

/// Writes `bytes` (whose SHA-256 is `sha256`) as `dir/name`, through a partial file that is
/// renamed into place, and reads the result again. Anything but those bytes is removed and
/// refused.
fn place_bytes(dir: &Path, name: &str, bytes: &[u8], sha256: &str) -> Result<PathBuf, PrintError> {
    let target = dir.join(name);
    let partial = dir.join(format!(".{name}.part"));
    // A leftover from an interrupted copy is removed; `create_new` then refuses anything that
    // appeared in its place, a symbolic link included.
    remove_in(dir, &partial);
    let written = (|| -> std::io::Result<()> {
        let mut out = std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&partial)?;
        out.write_all(bytes)?;
        out.sync_all()
    })();
    if let Err(error) = written {
        remove_in(dir, &partial);
        return Err(delivery_error(format!("cannot write `{name}`: {error}")));
    }
    if let Err(error) = std::fs::rename(&partial, &target) {
        remove_in(dir, &partial);
        return Err(delivery_error(format!("cannot write `{name}`: {error}")));
    }
    if !holds(&target, sha256) {
        remove_in(dir, &target);
        return Err(PrintError::ProjectChanged);
    }
    Ok(target)
}

/// Removes `path`, a file written directly inside `dir` (a canonical path), only while
/// `dir` is still that very directory: a folder that became a symbolic link since it was
/// checked is never removed through. (A link named `path` itself is removed as a link.)
fn remove_in(dir: &Path, path: &Path) {
    if is_canonical(dir) && path.parent() == Some(dir) {
        let _ = std::fs::remove_file(path);
    }
}

/// Whether `dir` is, now, its own canonical path (so no component of it is a link), with
/// or without the `\\?\` prefix Windows gives a canonical path.
fn is_canonical(dir: &Path) -> bool {
    dir.canonicalize()
        .is_ok_and(|canonical| plain_path(&canonical) == plain_path(dir))
}

/// `text` without the `\\?\` prefix Windows gives a canonical path, as a person writes the
/// path: `\\?\D:\Models` is `D:\Models`, and `\\?\UNC\server\share` is `\\server\share`.
/// Any other text is returned as it is. Written on text, so that it is tested everywhere.
fn without_verbatim(text: &str) -> String {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        text.to_owned()
    }
}

/// `path` without the `\\?\` prefix ([`without_verbatim`]); a path that is not text is
/// returned as it is (only Windows writes the prefix, and its paths are text).
fn plain_path(path: &Path) -> PathBuf {
    path.to_str().map_or_else(
        || path.to_path_buf(),
        |text| PathBuf::from(without_verbatim(text)),
    )
}

/// Whether `path` is, now, a regular file (not a link) whose SHA-256 is `sha256`.
fn holds(path: &Path, sha256: &str) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file())
        && import::sha256_file(path).is_ok_and(|hash| hash.eq_ignore_ascii_case(sha256))
}

impl PrintService {
    fn project_reviews(&self) -> MutexGuard<'_, ProjectReviews> {
        self.project_reviews
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// The project as the interface shows it.
    fn editable(&self, record: ProjectExportRecord) -> Result<EditableProject, PrintError> {
        let summary: ProjectExportSummary =
            serde_json::from_value(record.summary).map_err(|_| PrintError::ProjectChanged)?;
        let path = self
            .data
            .version_dir(&record.project_id, record.version)
            .map_err(|_| PrintError::ProjectStale)?
            .join(PROJECT_DIR)
            .join(&record.folder)
            .join(&summary.project);
        Ok(EditableProject {
            id: record.id,
            project_id: record.project_id,
            version: record.version,
            plate: record.plate,
            plan_sha256: record.plan_sha256,
            slice_sha256: record.slice_sha256,
            path,
            sha256: record.sha256,
            keys: summary.keys,
            test_slice: summary.slice,
            proof: summary.proof,
            notice: PROJECT_OUTSIDE_CHECK.to_owned(),
            delivery: record.delivery,
            detail: serde_json::from_value(record.detail).unwrap_or_default(),
            created_at_ms: record.created_at_ms,
            updated_at_ms: record.updated_at_ms,
        })
    }

    /// One editable project.
    pub(crate) fn project(&self, id: &str) -> Result<EditableProject, PrintError> {
        let record = self.shared.store.project_export(id)?;
        self.editable(record)
    }

    /// The editable projects of a project, newest first.
    pub(crate) fn projects(&self, project_id: &str) -> Result<Vec<EditableProject>, PrintError> {
        self.shared
            .store
            .project_exports(project_id)?
            .into_iter()
            .map(|record| self.editable(record))
            .collect()
    }

    /// Runs the G-code check over the file of the slice `slice` (report only: no job is
    /// marked) and refuses unless it passes: the file is the one the record names, of the
    /// layout the version has now, for the plate the settings name now.
    pub(crate) async fn check_plan_slice(&self, slice: &SliceRecord) -> Result<(), PrintError> {
        let summary: SliceSummary =
            serde_json::from_value(slice.summary.clone()).map_err(|_| PrintError::ProjectStale)?;
        let version_dir = self
            .data
            .version_dir(&slice.project_id, slice.version)
            .map_err(|_| PrintError::ProjectStale)?;
        let sliced_layout = slice.job.get("prepared_at_ms").and_then(Value::as_i64);
        if sliced_layout.is_none() || sliced_layout != layout_time(&version_dir) {
            return Err(PrintError::LayoutChanged);
        }
        let SliceDeclaration { declared, .. } = self.declaration(slice, &summary)?;
        let project = self.shared.store.project(&slice.project_id)?;
        let name = job_name(&project.name, slice.version);
        let checked = self
            .check_file(
                version_dir.join("slice").join(&slice.profile_hash),
                summary.output.clone(),
                name,
                declared,
            )
            .await?
            .map_err(|report| {
                let (first, findings) = rejection(&report);
                PrintError::Rejected {
                    first: format!(
                        "the sliced file of this print plan did not pass, so no editable \
                         project is made or handed over: {first}"
                    ),
                    findings,
                }
            })?;
        if !checked.sha256().eq_ignore_ascii_case(&summary.sha256) {
            return Err(PrintError::ProjectStale);
        }
        Ok(())
    }

    /// The test slice of a project, checked: the G-code check over the file Bambu Studio's
    /// command line made from the project alone, held to the job the plan's slice was made
    /// for. Refused like a slice when the check refuses it.
    async fn check_test_slice(
        &self,
        slice: &SliceRecord,
        test: &SliceSummary,
        project_dir: PathBuf,
    ) -> Result<CheckedJob, PrintError> {
        let SliceDeclaration { declared, .. } = self.declaration(slice, test)?;
        let project = self.shared.store.project(&slice.project_id)?;
        let name = job_name(&project.name, slice.version);
        let checked = self
            .check_file(project_dir, test.output.clone(), name, declared)
            .await?
            .map_err(|report| {
                let (first, findings) = rejection(&report);
                PrintError::Rejected {
                    first: format!(
                        "Bambu Studio's own slice of the editable project did not pass, so \
                         the project is not used: {first}"
                    ),
                    findings,
                }
            })?;
        if !checked.sha256().eq_ignore_ascii_case(&test.sha256) {
            return Err(PrintError::ProjectChanged);
        }
        Ok(checked)
    }

    /// Records the project the backend made for `plan` after checking its test slice, and
    /// returns it. The caller checked the plan's slice before the project was made.
    pub(crate) async fn record_project(
        &self,
        plan: &ProjectPlan,
        export: ProjectExportSummary,
    ) -> Result<EditableProject, PrintError> {
        let folder = plan
            .request
            .output_dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(PrintError::ProjectStale)?
            .to_owned();
        self.check_test_slice(&plan.slice, &export.slice, plan.request.output_dir.clone())
            .await?;
        let slice_sha256 = plan
            .slice
            .summary
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let now = now_ms();
        let record = ProjectExportRecord {
            id: new_id(),
            project_id: plan.request.project_id.clone(),
            version: plan.request.version,
            plate: plan.request.plate,
            plan_sha256: plan.plan_sha256.clone(),
            profile_hash: plan.slice.profile_hash.clone(),
            slice_sha256,
            folder,
            sha256: export.sha256.to_ascii_lowercase(),
            summary: json!(export),
            delivery: None,
            detail: json!({}),
            created_at_ms: now,
            updated_at_ms: now,
        };
        self.shared.store.put_project_export(&record)?;
        let _ = self.shared.store.audit(NewAudit {
            project_id: Some(record.project_id.clone()),
            run_id: None,
            kind: "print",
            name: "project_made".to_owned(),
            args_sha256: Some(record.sha256.clone()),
            outcome: "ok".to_owned(),
            detail: json!({
                "project_export": record.id,
                "version": record.version,
                "plate": record.plate,
                "plan_sha256": record.plan_sha256,
                "slice_sha256": record.slice_sha256,
            }),
        });
        self.editable(record)
    }

    /// Checks a project again and returns what a person must see before it is handed
    /// over to `destination`. The caller checked that the project's plan is still the
    /// plate's plan. A new review replaces the last one, also when it fails.
    pub(crate) async fn project_review(
        &self,
        id: &str,
        destination: ProjectDestination,
    ) -> Result<ProjectReview, PrintError> {
        self.project_reviews().remove(id);
        let store = &self.shared.store;
        let record = store.project_export(id)?;
        let summary: ProjectExportSummary = serde_json::from_value(record.summary.clone())
            .map_err(|_| PrintError::ProjectChanged)?;
        // The slice of the same plan must still be the one the project was made after, and
        // pass the check now.
        let slice = store
            .slice(&record.project_id, record.version, &record.profile_hash)?
            .ok_or(PrintError::ProjectStale)?;
        if slice.summary.get("sha256").and_then(Value::as_str) != Some(record.slice_sha256.as_str())
        {
            return Err(PrintError::ProjectStale);
        }
        self.check_plan_slice(&slice).await?;
        let version_dir = self
            .data
            .version_dir(&record.project_id, record.version)
            .map_err(|_| PrintError::ProjectStale)?;
        let project_dir = version_dir.join(PROJECT_DIR).join(&record.folder);
        let test = self
            .check_test_slice(&slice, &summary.slice, project_dir.clone())
            .await?;
        // The project file is the one that was made, sliced and proven.
        let path = import::resolve_inside(&project_dir, &summary.project, MAX_SLICE_BYTES)
            .map_err(|_| PrintError::ProjectChanged)?;
        {
            let (path, sha256) = (path.clone(), record.sha256.clone());
            let same = tokio::task::spawn_blocking(move || holds(&path, &sha256))
                .await
                .map_err(delivery_error)?;
            if !same {
                return Err(PrintError::ProjectChanged);
            }
        }
        let opener = self.opener();
        // The destination as the person confirms it: a folder given by the caller is
        // resolved to the folder it is now, and that folder is what the review names and
        // what the confirmation is bound to.
        let (destination, handoff) = match destination {
            ProjectDestination::BambuStudio => (
                ProjectDestination::BambuStudio,
                Some(self.handoff_folder()?),
            ),
            ProjectDestination::Folder { folder } => {
                // As a person writes it: without the `\\?\` prefix of a canonical path on
                // Windows (deliver compares it the same way).
                let resolved = folder
                    .canonicalize()
                    .ok()
                    .filter(|dir| dir.is_dir())
                    .map(|dir| plain_path(&dir))
                    .ok_or_else(|| {
                        delivery_error(format!(
                            "the folder `{}` does not exist",
                            truncate(&folder.to_string_lossy(), 200)
                        ))
                    })?;
                (ProjectDestination::Folder { folder: resolved }, None)
            }
        };
        let opens = !opener.is_stand_in();
        let destination_text = match (&destination, &handoff) {
            (ProjectDestination::Folder { folder }, _) => format!(
                "saved in the folder {}; nothing is opened.",
                folder.display()
            ),
            (ProjectDestination::BambuStudio, Some(folder)) => {
                let shown = folder.windows_path().map_or_else(
                    || plain_path(folder.path()).display().to_string(),
                    without_verbatim,
                );
                if !opens {
                    format!("saved in the hand-off folder {shown}; nothing is opened.")
                } else {
                    format!(
                        "saved in the hand-off folder {shown}, then the system is asked to \
                         open it with the program for .3mf files (Bambu Studio)."
                    )
                }
            }
            (ProjectDestination::BambuStudio, None) => return Err(PrintError::NotReviewed),
        };
        let handoff_target = handoff.as_ref().map(|folder| HandOffTarget {
            folder: folder.path().to_path_buf(),
            windows_folder: folder.windows_path().map(str::to_owned),
            opener: opener.describe().to_owned(),
            opens,
        });
        // The folder's name goes into the text the person confirms, which the dialog shows
        // as it is: it must read as what it is, on one line. A name with a line break, a
        // tab, another kind of space, or an invisible or direction-changing character is
        // refused here, before any dialog.
        if !hotend_printer::is_plain_line(&destination_text) {
            const UNSHOWABLE: &str = "holds a line break, a tab, an unusual space, or an \
                                      invisible or direction-changing character, so it cannot \
                                      be shown truthfully in the confirmation; nothing was \
                                      handed over.";
            return Err(match destination {
                ProjectDestination::Folder { .. } => delivery_error(format!(
                    "the destination folder's path {UNSHOWABLE} Choose or rename the folder"
                )),
                ProjectDestination::BambuStudio => PrintError::HandOffFolder(format!(
                    "the hand-off folder's path {UNSHOWABLE} Choose another hand-off folder \
                     in the settings"
                )),
            });
        }
        // The confirmation keeps where the project goes in at most this many bytes; a longer
        // path is refused as too long, not as a changed project.
        if destination_text.len() > hotend_printer::MAX_DESTINATION_BYTES {
            const TOO_LONG: &str = "is too long to be named in the confirmation; nothing was \
                                    handed over.";
            return Err(match destination {
                ProjectDestination::Folder { .. } => delivery_error(format!(
                    "the destination folder's path {TOO_LONG} Choose a folder with a shorter path"
                )),
                ProjectDestination::BambuStudio => PrintError::HandOffFolder(format!(
                    "the hand-off folder's path {TOO_LONG} Choose a hand-off folder with a \
                     shorter path in the settings"
                )),
            });
        }
        let request = ConfirmationRequest::project(&test, &record.sha256, &destination_text)
            .ok_or(PrintError::ProjectChanged)?;
        let review = ProjectReview {
            file_name: project_file_name(&test, &record.sha256),
            handoff: handoff_target,
            review_text: request.review_text(),
            warnings: test.lint().findings.iter().map(Into::into).collect(),
            notice: PROJECT_OUTSIDE_CHECK.to_owned(),
            destination: destination.clone(),
            project: self.editable(record.clone())?,
        };
        let layout = slice.job.get("prepared_at_ms").and_then(Value::as_i64);
        self.project_reviews().insert(
            id.to_owned(),
            ProjectReviewState {
                test: Arc::new(test),
                destination,
                destination_text,
                handoff,
                opens,
                sha256: record.sha256,
                path,
                version_dir,
                layout,
            },
        );
        Ok(review)
    }

    /// Hands a reviewed project over, on a person's confirmation. `shown` is everything
    /// the dialog or the terminal displayed for it; it must contain the project's review
    /// text in one piece, word for word ([`ProjectReview::review_text`]).
    pub(crate) async fn project_deliver(
        &self,
        authority: &ConfirmationAuthority,
        id: &str,
        shown: &str,
    ) -> Result<EditableProject, PrintError> {
        let review = self
            .project_reviews()
            .remove(id)
            .ok_or(PrintError::NotReviewed)?;
        let record = self.shared.store.project_export(id)?;
        if !record.sha256.eq_ignore_ascii_case(&review.sha256) {
            return Err(PrintError::ProjectChanged);
        }
        self.plate_still_chosen(&review.test.declared().plate_type)?;
        if layout_time(&review.version_dir) != review.layout {
            return Err(PrintError::LayoutChanged);
        }
        let request =
            ConfirmationRequest::project(&review.test, &review.sha256, &review.destination_text)
                .ok_or(PrintError::ProjectChanged)?;
        let review_text = request.review_text();
        let shown = shown_review(shown, &review_text).ok_or(PrintError::SummaryMismatch)?;
        // The review text said whether the system is asked to open the project; the opener
        // that would be asked now must be of the same kind, or the text was not true.
        let opener = self.opener();
        if review.destination == ProjectDestination::BambuStudio
            && review.opens == opener.is_stand_in()
        {
            return Err(delivery_error(if review.opens {
                "the review said that the system is asked to open the project, and now \
                 nothing would open it; nothing was handed over. Review the project again"
            } else {
                "the review said that nothing opens the project, and now the system would \
                 be asked to open it; nothing was handed over. Review the project again"
            }));
        }
        let confirmed = confirmation(authority, request, shown)?;
        let kind = review.destination.as_str();
        let name = project_file_name(&review.test, &review.sha256);
        let ProjectReviewState {
            test,
            destination,
            destination_text,
            handoff,
            sha256,
            path,
            ..
        } = review;
        // Redeemed, read once, placed and read again, off the async threads.
        let placed = {
            let (sha256, name) = (sha256.clone(), name.clone());
            tokio::task::spawn_blocking(move || {
                confirmed
                    .redeem_for_project(&test, &sha256, &destination_text)
                    .map_err(delivery_error)?;
                let bytes = std::fs::read(&path).map_err(|_| PrintError::ProjectChanged)?;
                if !sha256_hex(&bytes).eq_ignore_ascii_case(&sha256) {
                    return Err(PrintError::ProjectChanged);
                }
                match (destination, handoff) {
                    (ProjectDestination::BambuStudio, Some(folder)) => {
                        // The folder is still the directory that was chosen and shown.
                        if !is_canonical(folder.path()) {
                            return Err(PrintError::HandOffFolder(
                                "the hand-off folder is no longer the folder that was chosen; \
                                 nothing was placed"
                                    .to_owned(),
                            ));
                        }
                        let written = place_bytes(folder.path(), &name, &bytes, &sha256)?;
                        match folder.placed_project(&written) {
                            Ok(placed) => Ok((written, Some(placed))),
                            Err(error) => {
                                remove_in(folder.path(), &written);
                                Err(delivery_error(error))
                            }
                        }
                    }
                    (ProjectDestination::Folder { folder }, _) => {
                        // The folder is still the directory that was reviewed and shown.
                        let dir = folder.canonicalize().map_err(|_| {
                            delivery_error(format!(
                                "the folder `{}` does not exist",
                                truncate(&folder.to_string_lossy(), 200)
                            ))
                        })?;
                        if plain_path(&dir) != folder || !dir.is_dir() {
                            return Err(delivery_error(
                                "the destination is no longer the folder that was shown; \
                                 nothing was written",
                            ));
                        }
                        Ok((place_bytes(&dir, &name, &bytes, &sha256)?, None))
                    }
                    (ProjectDestination::BambuStudio, None) => Err(PrintError::NotReviewed),
                }
            })
            .await
            .map_err(delivery_error)??
        };
        let (written, placed) = placed;
        let mut opener_fields = None;
        let detail = match placed {
            None => PrintJobDetail {
                message: Some(format!(
                    "written to the folder; nothing was opened. {PROJECT_OUTSIDE_CHECK}"
                )),
                path: Some(plain_path(&written)),
                ..PrintJobDetail::default()
            },
            Some(placed) => {
                let described = opener.describe().to_owned();
                let stand_in = opener.is_stand_in();
                // Read again right before it is opened: what opens is what was confirmed.
                let asked = {
                    let (placed, sha256) = (placed.clone(), sha256.clone());
                    tokio::task::spawn_blocking(move || {
                        if !holds(&placed.path().local, &sha256) {
                            let text = "the placed project changed before it was opened, so it \
                                        was not opened";
                            return Err((text.to_owned(), text));
                        }
                        opener
                            .open_project(&placed)
                            .map_err(|error| (error.to_string(), open_error_text(&error)))
                    })
                    .await
                    .map_err(|error| (error.to_string(), OPENER_TASK_FAILED))
                    .and_then(|asked| asked)
                };
                opener_fields = Some(opener_audit(&described, stand_in, &asked));
                let asked = asked.map_err(|(error, _)| error);
                let (opened, message) = match asked {
                    Ok(()) if !stand_in => (
                        true,
                        format!(
                            "saved to the hand-off folder, and the system was asked to open it \
                             with the program for .3mf files (Bambu Studio). \
                             {PROJECT_OUTSIDE_CHECK}"
                        ),
                    ),
                    Ok(()) => (
                        false,
                        format!(
                            "saved to the hand-off folder; nothing was opened ({described}). \
                             Open the file in Bambu Studio yourself. {PROJECT_OUTSIDE_CHECK}"
                        ),
                    ),
                    Err(error) => (
                        false,
                        format!(
                            "saved to the hand-off folder, but it could not be opened: {}. Open \
                             the file in Bambu Studio yourself. {PROJECT_OUTSIDE_CHECK}",
                            truncate(&error, 300)
                        ),
                    ),
                };
                PrintJobDetail {
                    message: Some(message),
                    path: Some(plain_path(&written)),
                    windows_path: placed.path().windows.clone(),
                    opened: Some(opened),
                    opener: Some(described),
                    ..PrintJobDetail::default()
                }
            }
        };
        let record = self
            .shared
            .store
            .set_project_export_delivery(id, kind, &json!(detail))?;
        let mut audited = json!({
            "project_export": record.id,
            "delivery": kind,
            "opened": detail.opened,
            "message": detail.message,
        });
        if let (Some(fields), Some(audited)) = (opener_fields, audited.as_object_mut()) {
            audited.extend(fields);
        }
        let _ = self.shared.store.audit(NewAudit {
            project_id: Some(record.project_id.clone()),
            run_id: None,
            kind: "print",
            name: "project_handed_over".to_owned(),
            args_sha256: Some(record.sha256.clone()),
            outcome: "ok".to_owned(),
            detail: audited,
        });
        self.editable(record)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn a_project_is_placed_only_with_the_bytes_of_its_hash() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"PK\x03\x04 a project".to_vec();
        let sha = sha256_hex(&bytes);
        let name = format!("holder-v3-{}.3mf", &sha[..32]);
        let placed = place_bytes(dir.path(), &name, &bytes, &sha).unwrap();
        assert_eq!(std::fs::read(&placed).unwrap(), bytes);
        assert_eq!(placed.file_name().unwrap().to_str().unwrap(), name);
        // No partial file is left behind.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        // Bytes that are not those of the hash are removed and refused.
        let refused = place_bytes(dir.path(), "other.3mf", b"other", &sha).unwrap_err();
        assert_eq!(refused.code(), "project_changed");
        assert!(!dir.path().join("other.3mf").exists());
        assert!(holds(&placed, &sha) && holds(&placed, &sha.to_ascii_uppercase()));
        assert!(!holds(&dir.path().join("missing.3mf"), &sha));
        // A link is not a placed file, even to the same bytes.
        #[cfg(unix)]
        {
            let link = dir.path().join("link.3mf");
            std::os::unix::fs::symlink(&placed, &link).unwrap();
            assert!(!holds(&link, &sha));
        }
    }

    #[test]
    fn the_stored_destination_names_are_stable() {
        assert_eq!(ProjectDestination::BambuStudio.as_str(), "bambu_studio");
        assert_eq!(
            ProjectDestination::Folder {
                folder: PathBuf::from("/x")
            }
            .as_str(),
            "folder"
        );
        let json = serde_json::to_value(ProjectDestination::BambuStudio).unwrap();
        assert_eq!(json, json!({"kind": "bambu_studio"}));
    }

    /// A canonical path on Windows carries `\\?\`; a person reads it without.
    #[test]
    fn the_verbatim_prefix_of_a_windows_path_is_not_shown() {
        assert_eq!(
            without_verbatim(r"\\?\D:\Models\Hotend"),
            r"D:\Models\Hotend"
        );
        assert_eq!(
            without_verbatim(r"\\?\UNC\server\share\Prints"),
            r"\\server\share\Prints"
        );
        assert_eq!(without_verbatim(r"D:\Models"), r"D:\Models");
        assert_eq!(without_verbatim("/home/me/Prints"), "/home/me/Prints");
        assert_eq!(without_verbatim(r"\\server\share"), r"\\server\share");
        assert_eq!(
            plain_path(Path::new(r"\\?\C:\Users\me")),
            PathBuf::from(r"C:\Users\me")
        );
    }

    /// A file is removed only while its folder is still the directory that was checked: a
    /// folder swapped for a link to another folder is never removed through (as
    /// `hotend_printer::delivery` does for its own files).
    #[cfg(unix)]
    #[test]
    fn nothing_is_removed_through_a_folder_that_became_a_link() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let folder = root.join("inbox");
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir(&folder).unwrap();
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(folder.join("a.3mf"), b"x").unwrap();
        std::fs::write(elsewhere.join("a.3mf"), b"keep").unwrap();
        remove_in(&folder, &folder.join("a.3mf"));
        assert!(!folder.join("a.3mf").exists());
        // Only a file directly inside the folder.
        remove_in(&folder, &elsewhere.join("a.3mf"));
        assert!(elsewhere.join("a.3mf").exists());
        // The folder becomes a link to another one: nothing is removed there.
        std::fs::remove_dir(&folder).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &folder).unwrap();
        remove_in(&folder, &folder.join("a.3mf"));
        assert_eq!(std::fs::read(elsewhere.join("a.3mf")).unwrap(), b"keep");
        // Nor through a link further up the path.
        let parent_link = root.join("up");
        std::os::unix::fs::symlink(&root, &parent_link).unwrap();
        let through = parent_link.join("elsewhere");
        remove_in(&through, &through.join("a.3mf"));
        assert_eq!(std::fs::read(elsewhere.join("a.3mf")).unwrap(), b"keep");
    }

    use std::collections::BTreeMap;
    use std::sync::OnceLock;

    use hotend_printer::HostKind;

    use crate::paths::DataDir;
    use crate::printing::PrinterOptions;
    use crate::settings::{SETTINGS_KEY, Settings};
    use crate::store::{NewVersion, Store, VersionStatus};

    /// The process's confirmation authority, standing in for the confirmation dialog.
    fn authority() -> &'static ConfirmationAuthority {
        static AUTHORITY: OnceLock<ConfirmationAuthority> = OnceLock::new();
        AUTHORITY.get_or_init(|| ConfirmationAuthority::take().expect("taken once"))
    }

    /// A small print the G-code check accepts for the Cool Plate, PLA on the main nozzle:
    /// three layers, bed at 35 C, nozzle at 220 C, every heater off at the end.
    const GCODE: &str = "\
; HEADER_BLOCK_START
; total layer number: 3
; HEADER_BLOCK_END
; printer_model = Bambu Lab X2D
; curr_bed_type = Cool Plate
; nozzle_diameter = 0.4,0.4
M73 P0 R12
M140 S35
M104 S220
G90
M83
G28
M190 S35
M109 S220
M1002 gcode_claim_action : 2
T0
G1 Z0.2 F600
G1 X50 Y50 F9000
G1 X100 Y50 E2.5 F1800
G1 X110 Y100 E1.9
G1 Z0.4 F600
G1 X50 Y100 E2.85 F1800
G1 Z0.6 F600
G1 X50 Y50 E2.4 F1800
G92 E0
M73 P100 R0
M104 S0
M140 S0
";

    const SLICE_INFO: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<config>\n  <plate>\n    \
        <metadata key=\"index\" value=\"1\"/>\n    <metadata key=\"prediction\" value=\"1200\"/>\n    \
        <metadata key=\"filament_maps\" value=\"1\"/>\n    \
        <filament id=\"1\" type=\"PLA\" color=\"#FFFFFF\" used_m=\"0.01\" used_g=\"0.03\"/>\n  \
        </plate>\n</config>\n";

    /// A `.gcode.3mf` with `gcode` for plate 1.
    fn sliced(gcode: &str) -> Vec<u8> {
        use std::io::Write as _;
        let md5 = md5_upper(gcode.as_bytes());
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, content) in [
            ("[Content_Types].xml", b"<Types/>".as_slice()),
            ("Metadata/plate_1.gcode", gcode.as_bytes()),
            ("Metadata/plate_1.gcode.md5", md5.as_bytes()),
            ("Metadata/slice_info.config", SLICE_INFO.as_bytes()),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    /// MD5 of `bytes` as the slicer writes it into `Metadata/plate_<n>.gcode.md5`: 32
    /// upper-case hex digits (RFC 1321).
    fn md5_upper(bytes: &[u8]) -> String {
        let shifts: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
        let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
        let mut message = bytes.to_vec();
        message.push(0x80);
        while message.len() % 64 != 56 {
            message.push(0);
        }
        message.extend_from_slice(&((bytes.len() as u64) * 8).to_le_bytes());
        for block in message.chunks_exact(64) {
            let word = |at: usize| {
                u32::from_le_bytes([
                    block[4 * at],
                    block[4 * at + 1],
                    block[4 * at + 2],
                    block[4 * at + 3],
                ])
            };
            let [mut a, mut b, mut c, mut d] = state;
            for step in 0..64usize {
                let (f, g) = match step / 16 {
                    0 => ((b & c) | (!b & d), step),
                    1 => ((d & b) | (!d & c), (5 * step + 1) % 16),
                    2 => (b ^ c ^ d, (3 * step + 5) % 16),
                    _ => (c ^ (b | !d), (7 * step) % 16),
                };
                let k = (f64::from(step as u32 + 1).sin().abs() * 4_294_967_296.0) as u32;
                let turned = a
                    .wrapping_add(f)
                    .wrapping_add(k)
                    .wrapping_add(word(g))
                    .rotate_left(shifts[step / 16 * 4 + step % 4]);
                (a, d, c, b) = (d, c, b, b.wrapping_add(turned));
            }
            for (kept, add) in state.iter_mut().zip([a, b, c, d]) {
                *kept = kept.wrapping_add(add);
            }
        }
        state
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .map(|byte| format!("{byte:02X}"))
            .collect()
    }

    fn summary(output: &str, bytes: &[u8]) -> Value {
        json!({
            "status": "ok", "output": output, "sha256": sha256_hex(bytes),
            "time_s": 1200.0, "filament": [{"slot": 1, "grams": 0.03, "metres": 0.01, "nozzle_id": 1}],
            "layers": 3,
            "bed": {"id": "cool", "plate_type": "Cool Plate", "temperature_c": 35.0},
            "warnings": [], "slicer": {"name": "Bambu Studio", "version": "02.09.01.70"}
        })
    }

    struct World {
        _tmp: tempfile::TempDir,
        data: DataDir,
        store: Arc<Store>,
        service: PrintService,
        id: String,
        project: Vec<u8>,
        project_path: PathBuf,
        test_slice: PathBuf,
        plan_slice: PathBuf,
    }

    /// A version with a slice of its plan that passes the check, and the editable project
    /// made after it, recorded as `print_project_make` records one.
    fn world() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataDir::resolve(Some(&tmp.path().join("data"))).unwrap();
        let store = Arc::new(Store::open(&data.db_path()).unwrap());
        store
            .set_setting(
                SETTINGS_KEY,
                &json!(Settings {
                    plate_type: Some("cool".to_owned()),
                    ..Settings::default()
                }),
            )
            .unwrap();
        let project = store.create_project("Planter holder").unwrap();
        let staged = data.versions_dir(&project.id).unwrap().join(".staging-1");
        std::fs::create_dir_all(&staged).unwrap();
        store
            .create_version(
                &data,
                &project.id,
                &staged,
                NewVersion {
                    parent: None,
                    origin: "test".to_owned(),
                    note: String::new(),
                    status: VersionStatus::Ok,
                    code_sha256: "0".repeat(64),
                    params: BTreeMap::new(),
                },
            )
            .unwrap();
        let version_dir = data.version_dir(&project.id, 1).unwrap();
        std::fs::create_dir_all(version_dir.join("prepare")).unwrap();
        std::fs::write(
            version_dir.join("prepare/prepare.json"),
            json!({"created_at_ms": 1000}).to_string(),
        )
        .unwrap();
        // The slice of the plan.
        let slice_dir = version_dir.join("slice/abcdef0123456789");
        std::fs::create_dir_all(&slice_dir).unwrap();
        let plan_bytes = sliced(GCODE);
        let plan_slice = slice_dir.join("plate_1.gcode.3mf");
        std::fs::write(&plan_slice, &plan_bytes).unwrap();
        let plan_sha256 = "a".repeat(64);
        let record = SliceRecord {
            project_id: project.id.clone(),
            version: 1,
            profile_hash: "abcdef0123456789".to_owned(),
            profile: "standard-0.20mm".to_owned(),
            summary: summary("plate_1.gcode.3mf", &plan_bytes),
            job: json!({
                "plate": 1, "printer_id": "bambu-x2d",
                "material": {"id": "pla-generic", "family": "PLA"},
                "plate_type": "cool",
                "nozzles": [{"id": 1, "diameter_mm": 0.4}, {"id": 0, "diameter_mm": 0.4}],
                "pieces": ["holder"], "prepared_at_ms": 1000,
                "plan_sha256": plan_sha256, "plan_blocked": null,
                "nozzle_diameter_mm": 0.4, "nozzle_size_set": true
            }),
            created_at_ms: 1,
        };
        store.put_slice(&record).unwrap();
        // The project and its test slice.
        let folder = format!("1-{}", &plan_sha256[..16]);
        let project_dir = version_dir.join(PROJECT_DIR).join(&folder);
        std::fs::create_dir_all(project_dir.join("proof/input")).unwrap();
        std::fs::create_dir_all(project_dir.join("proof/output")).unwrap();
        let project_bytes = b"PK\x03\x04 the editable project".to_vec();
        let project_path = project_dir.join("proof/input/project.3mf");
        std::fs::write(&project_path, &project_bytes).unwrap();
        let test_bytes = sliced(&GCODE.replace("G1 X50 Y50 E2.4", "G1 X50 Y50 E2.3"));
        let test_slice = project_dir.join("proof/output/sliced.gcode.3mf");
        std::fs::write(&test_slice, &test_bytes).unwrap();
        let export = ProjectExportSummary {
            project: "proof/input/project.3mf".to_owned(),
            sha256: sha256_hex(&project_bytes),
            keys: ProjectKeys {
                process: vec!["brim_type".to_owned()],
                filament: vec!["textured_plate_temp".to_owned()],
                printer: Vec::new(),
            },
            slice: serde_json::from_value(summary("proof/output/sliced.gcode.3mf", &test_bytes))
                .unwrap(),
            proof: "passed".to_owned(),
            stand_in: false,
        };
        let id = new_id();
        store
            .put_project_export(&ProjectExportRecord {
                id: id.clone(),
                project_id: project.id.clone(),
                version: 1,
                plate: 1,
                plan_sha256,
                profile_hash: "abcdef0123456789".to_owned(),
                slice_sha256: sha256_hex(&plan_bytes),
                folder,
                sha256: sha256_hex(&project_bytes),
                summary: json!(export),
                delivery: None,
                detail: json!({}),
                created_at_ms: 2,
                updated_at_ms: 2,
            })
            .unwrap();
        let service = PrintService::new(
            data.clone(),
            Arc::clone(&store),
            PrinterOptions::default(),
            tokio::sync::broadcast::channel(16).0,
        )
        .unwrap();
        World {
            _tmp: tmp,
            data,
            store,
            service,
            id,
            project: project_bytes,
            project_path,
            test_slice,
            plan_slice,
        }
    }

    /// What a dialog shows: lines of its own around the review text.
    fn dialog(review: &ProjectReview) -> String {
        format!(
            "Open this project in Bambu Studio?\n\n{}\n\nPress Save.",
            review.review_text
        )
    }

    #[tokio::test]
    async fn a_reviewed_project_is_handed_over_with_its_own_bytes_on_a_confirmation() {
        let w = world();
        let opener = Arc::new(hotend_printer::StandInOpener::new());
        w.service.set_link_opener(opener.clone());
        let review = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap();
        let sha = sha256_hex(&w.project);
        let handoff = w.data.root().join("handoff").canonicalize().unwrap();
        assert!(
            review.review_text.starts_with(&format!(
                "Editable Bambu Studio project. SHA-256 of the project file: {sha}.\n\
                 Where it goes: saved in the hand-off folder {}; nothing is opened.\n\
                 {PROJECT_OUTSIDE_CHECK}\n",
                handoff.display()
            )),
            "{}",
            review.review_text
        );
        assert_eq!(review.notice, PROJECT_OUTSIDE_CHECK);
        assert_eq!(review.project.notice, PROJECT_OUTSIDE_CHECK);
        assert_eq!(
            review.file_name,
            format!("Planter_holder-v1-{}.3mf", &sha[..32])
        );
        let target = review.handoff.clone().unwrap();
        assert!(!target.opens);
        assert_eq!(target.folder, w.data.root().join("handoff"));

        // A text without the review text, or with one character changed, confirms nothing.
        let wrong = w
            .service
            .project_deliver(
                authority(),
                &w.id,
                &review.review_text.replace("SHA", "Sha"),
            )
            .await
            .unwrap_err();
        assert_eq!(wrong.code(), "summary_mismatch");
        // And that used up the review.
        let again = w
            .service
            .project_deliver(authority(), &w.id, &dialog(&review))
            .await
            .unwrap_err();
        assert_eq!(again.code(), "not_reviewed");

        let review = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap();
        let handed = w
            .service
            .project_deliver(authority(), &w.id, &dialog(&review))
            .await
            .unwrap();
        assert_eq!(handed.delivery.as_deref(), Some("bambu_studio"));
        let placed = handed.detail.path.clone().unwrap();
        assert_eq!(placed, target.folder.join(&review.file_name));
        assert_eq!(std::fs::read(&placed).unwrap(), w.project);
        assert_eq!(handed.detail.opened, Some(false));
        assert_eq!(
            handed.detail.opener.as_deref(),
            Some("stand-in opener: nothing was opened")
        );
        assert_eq!(handed.detail.windows_path, None);
        let message = handed.detail.message.clone().unwrap();
        assert!(message.contains(PROJECT_OUTSIDE_CHECK), "{message}");
        assert!(!message.contains("checked"), "{message}");
        assert_eq!(
            opener.asked(),
            [format!("open-project {}", placed.display())]
        );
        // The record keeps it.
        let stored = w.service.project(&w.id).unwrap();
        assert_eq!(stored.detail, handed.detail);
        let audit = w.store.list_audit(None, 50).unwrap();
        let handed_over: Vec<_> = audit
            .iter()
            .filter(|entry| entry.name == "project_handed_over")
            .collect();
        assert_eq!(handed_over.len(), 1);
        let entry = &handed_over[0].detail;
        assert_eq!(entry["opener"], "stand-in opener: nothing was opened");
        assert_eq!(entry["opener_started"], false);
        assert!(entry.get("opener_error").is_none(), "{entry}");
        let text = entry.to_string();
        assert!(!text.contains(&review.file_name), "{text}");
        assert!(!text.contains("Planter"), "{text}");
        assert!(!text.contains("open-project"), "{text}");

        // Into a folder nothing is opened.
        let out = tempfile::tempdir().unwrap();
        let destination = ProjectDestination::Folder {
            folder: out.path().to_path_buf(),
        };
        let review = w.service.project_review(&w.id, destination).await.unwrap();
        assert!(review.handoff.is_none());
        let saved = w
            .service
            .project_deliver(authority(), &w.id, &dialog(&review))
            .await
            .unwrap();
        assert_eq!(saved.delivery.as_deref(), Some("folder"));
        let written = saved.detail.path.clone().unwrap();
        assert_eq!(
            written.parent().unwrap(),
            out.path().canonicalize().unwrap()
        );
        assert_eq!(std::fs::read(&written).unwrap(), w.project);
        assert_eq!(saved.detail.opened, None);
        assert_eq!(opener.asked().len(), 1);
    }

    /// On every host, not only under WSL, a hand-off folder chosen in the settings that
    /// cannot be used makes Bambu Connect unavailable in the printer list, with the reason.
    #[test]
    fn an_unusable_hand_off_folder_makes_bambu_connect_unavailable_on_every_host() {
        let w = world();
        let connect = |service: &PrintService| {
            service.printers().unwrap()[0]
                .deliveries
                .iter()
                .find(|delivery| delivery.kind == "bambu_connect")
                .cloned()
                .unwrap()
        };
        let choose = |folder: Option<PathBuf>| {
            w.store
                .set_setting(
                    SETTINGS_KEY,
                    &json!(Settings {
                        plate_type: Some("cool".to_owned()),
                        handoff_folder: folder,
                        ..Settings::default()
                    }),
                )
                .unwrap();
        };
        let file = w.data.root().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        for host in [HostKind::Linux, HostKind::MacOs, HostKind::Windows] {
            w.service.set_handoff_host(host);
            // No folder chosen: Hotend's own folder is used off WSL.
            choose(None);
            assert!(connect(&w.service).enabled, "{host:?}");
            for unusable in [w.data.root().join("missing"), file.clone()] {
                choose(Some(unusable.clone()));
                let way = connect(&w.service);
                assert!(!way.enabled, "{host:?} {}", unusable.display());
                let reason = way.disabled_reason.unwrap();
                assert!(
                    reason.starts_with("The hand-off folder chosen in Settings cannot be used"),
                    "{reason}"
                );
            }
        }
        // A usable folder off WSL.
        w.service.set_handoff_host(HostKind::Linux);
        choose(Some(w.data.root().to_path_buf()));
        assert!(connect(&w.service).enabled);
    }

    #[tokio::test]
    async fn a_yes_for_one_destination_does_not_hand_the_project_to_another() {
        let w = world();
        let out = tempfile::tempdir().unwrap();
        let to_folder = w
            .service
            .project_review(
                &w.id,
                ProjectDestination::Folder {
                    folder: out.path().to_path_buf(),
                },
            )
            .await
            .unwrap();
        let folder = out.path().canonicalize().unwrap();
        assert!(
            to_folder.review_text.contains(&format!(
                "Where it goes: saved in the folder {}; nothing is opened.",
                folder.display()
            )),
            "{}",
            to_folder.review_text
        );
        assert_eq!(
            to_folder.destination,
            ProjectDestination::Folder {
                folder: folder.clone()
            }
        );
        // Reviewed again for the hand-off folder: the yes shown for the folder is no yes
        // for the hand-off.
        let to_handoff = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap();
        assert!(
            to_handoff
                .review_text
                .contains("Where it goes: saved in the hand-off folder "),
            "{}",
            to_handoff.review_text
        );
        assert_ne!(to_folder.review_text, to_handoff.review_text);
        let refused = w
            .service
            .project_deliver(authority(), &w.id, &dialog(&to_folder))
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "summary_mismatch");
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);

        // The other way round.
        let to_handoff = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap();
        let other = tempfile::tempdir().unwrap();
        let to_other = w
            .service
            .project_review(
                &w.id,
                ProjectDestination::Folder {
                    folder: other.path().to_path_buf(),
                },
            )
            .await
            .unwrap();
        let refused = w
            .service
            .project_deliver(authority(), &w.id, &dialog(&to_handoff))
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "summary_mismatch");
        assert!(
            !w.data
                .root()
                .join("handoff")
                .join(&to_handoff.file_name)
                .exists()
        );
        assert_ne!(to_other.review_text, to_handoff.review_text);
    }

    #[tokio::test]
    async fn a_project_whose_files_changed_or_whose_check_fails_is_not_handed_over() {
        // The project file changed after the review: nothing is placed.
        let w = world();
        let review = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap();
        std::fs::write(&w.project_path, b"PK another project").unwrap();
        let changed = w
            .service
            .project_deliver(authority(), &w.id, &dialog(&review))
            .await
            .unwrap_err();
        assert_eq!(changed.code(), "project_changed");
        assert!(
            !w.data.root().join("handoff").exists()
                || std::fs::read_dir(w.data.root().join("handoff"))
                    .unwrap()
                    .next()
                    .is_none()
        );
        // ... and is not reviewed again either.
        let refused = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "project_changed");

        // The test slice changed after the review: the confirmation is not redeemed.
        let w = world();
        let review = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap();
        let bytes = std::fs::read(&w.test_slice).unwrap();
        std::fs::write(&w.test_slice, [bytes.as_slice(), b" "].concat()).unwrap();
        let refused = w
            .service
            .project_deliver(authority(), &w.id, &dialog(&review))
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "delivery_failed", "{refused}");

        // A test slice the G-code check refuses: no review, nothing to confirm.
        let w = world();
        std::fs::write(&w.test_slice, sliced(&GCODE.replace("M104 S0\n", ""))).unwrap();
        let refused = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "gcode_rejected", "{refused}");
        assert!(refused.to_string().contains("did not pass"), "{refused}");

        // The plan's own slice is refused by the check: no project is reviewed either.
        let w = world();
        std::fs::write(&w.plan_slice, sliced(&GCODE.replace("M104 S0\n", ""))).unwrap();
        let refused = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "gcode_rejected", "{refused}");

        // Another plate in the settings since the slice: refused.
        let w = world();
        w.store
            .set_setting(
                SETTINGS_KEY,
                &json!(Settings {
                    plate_type: Some("textured-pei".to_owned()),
                    ..Settings::default()
                }),
            )
            .unwrap();
        let refused = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "plate_changed", "{refused}");
    }

    /// A folder whose name holds a line or paragraph separator, a direction override or
    /// another character that would not show as what it is is refused when the project is
    /// reviewed, before any dialog: its path would go into the text the person confirms.
    #[tokio::test]
    async fn a_folder_whose_name_does_not_show_as_it_is_is_refused_at_review() {
        let w = world();
        let out = tempfile::tempdir().unwrap();
        for name in [
            "in\u{2028}box",
            "in\u{2029}box",
            "in\u{202E}xob.3mf",
            "in\u{200B}box",
            "in\tbox",
            "in\u{3000}box",
        ] {
            let folder = out.path().join(name);
            std::fs::create_dir(&folder).unwrap();
            let refused = w
                .service
                .project_review(&w.id, ProjectDestination::Folder { folder })
                .await
                .unwrap_err();
            assert_eq!(refused.code(), "delivery_failed", "{name:?}: {refused}");
            assert!(
                refused.to_string().contains("cannot be shown truthfully"),
                "{refused}"
            );
            // Nothing is held to be confirmed.
            let none = w
                .service
                .project_deliver(authority(), &w.id, "anything")
                .await
                .unwrap_err();
            assert_eq!(none.code(), "not_reviewed");
        }
        // The same for the hand-off folder chosen in the settings.
        let handoff = out.path().join("hand\u{2028}off");
        std::fs::create_dir_all(&handoff).unwrap();
        w.store
            .set_setting(
                SETTINGS_KEY,
                &json!(Settings {
                    plate_type: Some("cool".to_owned()),
                    handoff_folder: Some(handoff),
                    ..Settings::default()
                }),
            )
            .unwrap();
        w.service.set_handoff_host(HostKind::Linux);
        let refused = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "handoff_folder", "{refused}");
        // A plain name with a space is fine.
        let plain = out.path().join("my prints");
        std::fs::create_dir(&plain).unwrap();
        w.service
            .project_review(&w.id, ProjectDestination::Folder { folder: plain })
            .await
            .unwrap();
    }

    /// An opener that says it opens what it is asked to, and only notes it.
    #[derive(Default)]
    struct NotingOpener(std::sync::Mutex<Vec<String>>);

    impl hotend_printer::LinkOpener for NotingOpener {
        fn open(
            &self,
            _link: &hotend_printer::BambuConnectLink,
        ) -> Result<(), hotend_printer::OpenError> {
            self.0.lock().unwrap().push("link".to_owned());
            Ok(())
        }
        fn open_project(
            &self,
            project: &hotend_printer::PlacedProject,
        ) -> Result<(), hotend_printer::OpenError> {
            self.0
                .lock()
                .unwrap()
                .push(project.path().local.display().to_string());
            Ok(())
        }
        fn describe(&self) -> &'static str {
            "noting opener"
        }
    }

    /// The review text says whether anything opens the project; a hand-over whose opener is
    /// of the other kind by then is refused, with nothing placed and nothing opened.
    #[tokio::test]
    async fn a_hand_over_whose_opener_changed_since_the_review_is_refused() {
        let w = world();
        let noting = Arc::new(NotingOpener::default());
        let handoff = w.data.root().join("handoff");
        let placed = || {
            std::fs::read_dir(&handoff)
                .map(|entries| entries.count())
                .unwrap_or(0)
        };

        // Reviewed with the stand-in ("nothing is opened"), handed over with a real opener.
        w.service
            .set_link_opener(Arc::new(hotend_printer::StandInOpener::new()));
        let review = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap();
        assert!(review.review_text.contains("nothing is opened"));
        w.service.set_link_opener(noting.clone());
        let refused = w
            .service
            .project_deliver(authority(), &w.id, &dialog(&review))
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "delivery_failed", "{refused}");
        assert!(
            refused.to_string().contains("Review the project again"),
            "{refused}"
        );
        assert_eq!(placed(), 0);
        assert!(noting.0.lock().unwrap().is_empty());

        // Reviewed with a real opener ("the system is asked to open it"), handed over with
        // the stand-in.
        let review = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap();
        assert!(
            review
                .review_text
                .contains("the system is asked to open it"),
            "{}",
            review.review_text
        );
        assert!(review.handoff.as_ref().unwrap().opens);
        w.service
            .set_link_opener(Arc::new(hotend_printer::StandInOpener::new()));
        let refused = w
            .service
            .project_deliver(authority(), &w.id, &dialog(&review))
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "delivery_failed", "{refused}");
        assert_eq!(placed(), 0);

        // The same kind of opener at both ends: handed over and opened.
        w.service.set_link_opener(noting.clone());
        let review = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap();
        let handed = w
            .service
            .project_deliver(authority(), &w.id, &dialog(&review))
            .await
            .unwrap();
        assert_eq!(handed.detail.opened, Some(true));
        assert_eq!(noting.0.lock().unwrap().len(), 1);
        assert_eq!(placed(), 1);
        // The audit entry says that the opener started, and names neither the file nor
        // its folder. The refused hand-overs above wrote none.
        let audit = w.store.list_audit(None, 50).unwrap();
        let handed_over: Vec<_> = audit
            .iter()
            .filter(|entry| entry.name == "project_handed_over")
            .collect();
        assert_eq!(handed_over.len(), 1);
        let entry = &handed_over[0].detail;
        assert_eq!(entry["opener"], "noting opener");
        assert_eq!(entry["opener_started"], true);
        assert!(entry.get("opener_error").is_none(), "{entry}");
        let text = entry.to_string();
        assert!(!text.contains(&review.file_name), "{text}");
        assert!(!text.contains("handoff"), "{text}");
    }

    /// An opener that cannot start anything.
    struct FailingOpener;

    impl hotend_printer::LinkOpener for FailingOpener {
        fn open(
            &self,
            _link: &hotend_printer::BambuConnectLink,
        ) -> Result<(), hotend_printer::OpenError> {
            Err(hotend_printer::OpenError::Unsupported)
        }
        fn open_project(
            &self,
            _project: &hotend_printer::PlacedProject,
        ) -> Result<(), hotend_printer::OpenError> {
            Err(hotend_printer::OpenError::Spawn {
                program: "/nonexistent/opener".to_owned(),
                reason: "no such program".to_owned(),
            })
        }
        fn describe(&self) -> &'static str {
            "failing opener"
        }
    }

    /// An opener that fails is audited as not started, with a fixed short text in place of
    /// its own message.
    #[tokio::test]
    async fn an_opener_that_fails_is_audited_without_its_message() {
        let w = world();
        w.service.set_link_opener(Arc::new(FailingOpener));
        let review = w
            .service
            .project_review(&w.id, ProjectDestination::BambuStudio)
            .await
            .unwrap();
        let handed = w
            .service
            .project_deliver(authority(), &w.id, &dialog(&review))
            .await
            .unwrap();
        assert_eq!(handed.detail.opened, Some(false));
        let audit = w.store.list_audit(None, 50).unwrap();
        let entry = &audit
            .iter()
            .find(|entry| entry.name == "project_handed_over")
            .unwrap()
            .detail;
        assert_eq!(entry["opener"], "failing opener");
        assert_eq!(entry["opener_started"], false);
        assert_eq!(
            entry["opener_error"],
            "the opener's program could not be started"
        );
        assert_eq!(
            open_error_text(&hotend_printer::OpenError::Unsupported),
            "there is nothing to open it with on this system"
        );
        // The opener's own words go to the record's message for the person, not into the
        // opener's fields; the entry names neither the file nor its folder.
        let audited = entry["opener_error"].to_string();
        assert!(!audited.contains("no such program"), "{audited}");
        let text = entry.to_string();
        assert!(!text.contains(&review.file_name), "{text}");
        assert!(!text.contains("handoff"), "{text}");
    }
}
