//! Following a real printer's own status reports, read over the local network
//! ([`crate::printer_lan`]), and the handed-off job they belong to.
//!
//! Guarantees:
//! - A report is tied to a Hotend job only when the name the printer reports for its job
//!   (`subtask_name` when it gives one, else the stem of the file it prints) is exactly,
//!   case included, the name of the file a Bambu Connect hand-off placed, and exactly one
//!   handed-off job has that name. The file's name carries its hash, so two jobs share it
//!   only when they hand off the same file. Anything else is the printer's status, shown as
//!   such and never claimed as Hotend's job.
//! - Once a session's follower matched a job it stays with it: a later report naming
//!   another job is shown as the printer's status only and neither updates that job nor
//!   moves the match to another one.
//! - A followed job keeps the status `handed_off`: Hotend did not start it and cannot tell
//!   from a name alone that the print is the file it checked. Its detail carries the phase,
//!   progress, time left and problems the printer reports, and a message that says where
//!   they come from; the job's events (`CoreEvent::Print`) are the ones a simulated print
//!   has.
//! - Nothing here talks to a printer; it reads [`PrinterStatus`] values only.

use std::sync::Arc;

use hotend_printer::{JobEvent, JobOutcome, JobTracker, PrinterStatus};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    CoreEvent, PrintEvent, PrintJob, PrintJobDetail, PrinterProblem, PrinterState, Shared,
    phase_name, state_of,
};
use crate::util::truncate;

/// The status a handed-off job has while it is followed (and afterwards).
const HANDED_OFF: &str = "handed_off";
/// Most handed-off jobs looked at for a match, newest first.
const MATCH_WINDOW: u32 = 200;

/// A temperature and its target, °C.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TemperatureReading {
    pub current_c: Option<f64>,
    /// 0 means the heater is off.
    pub target_c: Option<f64>,
}

/// One nozzle's temperature, by the printer's extruder id.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NozzleReading {
    pub id: u8,
    pub current_c: Option<f64>,
    pub target_c: Option<f64>,
}

/// A real printer's status as read over the local network: what the person sees in
/// `hotend printer status` and what the window receives as `CoreEvent::Printer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LanPrinterStatus {
    /// Phase, headline, progress, time left, layers, job name and problems (HMS codes and
    /// the print error, explained where the meaning is known), as for the simulated printer.
    pub state: PrinterState,
    pub nozzles: Vec<NozzleReading>,
    pub bed: TemperatureReading,
    pub chamber: TemperatureReading,
    /// How many reports were merged. Hotend never asks for a full report, so until the
    /// printer sends one on its own some values may still be missing.
    pub reports: u64,
    /// The handed-off job these reports were matched to; `None`: printer status only.
    pub job_id: Option<String>,
    /// The name that matched, when one did.
    pub matched_name: Option<String>,
    /// Whether the session with the printer is up.
    pub connected: bool,
    /// Why it ended, once it did.
    pub ended: Option<String>,
}

/// The name a job is known by on both sides: the file name without its folders and its
/// `.gcode.3mf`, `.3mf` or `.gcode` ending, case kept (names are compared exactly). `None`
/// when nothing is left.
pub(crate) fn job_key(text: &str) -> Option<String> {
    let name = text.rsplit(['/', '\\']).next()?.trim();
    let stem = [".gcode.3mf", ".3mf", ".gcode"]
        .iter()
        .find_map(|ending| name.strip_suffix(ending))
        .unwrap_or(name)
        .trim();
    (!stem.is_empty()).then(|| stem.to_owned())
}

/// The name the printer reports for its job: `subtask_name` when it gives one, else the
/// stem of `gcode_file`. Never both: a report is matched by one name only.
fn reported_key(status: &PrinterStatus) -> Option<String> {
    match status.job_name.as_deref().and_then(job_key) {
        Some(name) => Some(name),
        None => status.gcode_file.as_deref().and_then(job_key),
    }
}

/// The handed-off job being followed.
struct Matched {
    job_id: String,
    name: String,
    tracker: JobTracker,
    ended: bool,
}

/// Turns a printer's reports into [`LanPrinterStatus`] values and, for a report matched to
/// a handed-off job, into that job's detail and events. Every observation is also sent as
/// `CoreEvent::Printer`.
pub(crate) struct Follower {
    shared: Arc<Shared>,
    matched: Option<Matched>,
}

impl Follower {
    pub(super) fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            matched: None,
        }
    }

    /// Takes the status now (and whether the session is up) and reports it.
    pub(crate) fn observe(
        &mut self,
        status: &PrinterStatus,
        connected: bool,
        ended: Option<String>,
    ) -> LanPrinterStatus {
        let name = reported_key(status);
        let mut newly = false;
        if self.matched.is_none()
            && let Some(name) = &name
        {
            self.matched = self.find(name);
            newly = self.matched.is_some();
        }
        // Once matched, this follower stays with that job for the session: a report naming
        // another job (or none) is the printer's status only. It neither changes the matched
        // job nor moves the match to another job.
        let ours = self
            .matched
            .as_ref()
            .is_some_and(|matched| name.as_deref() == Some(matched.name.as_str()));
        let state = state_of(status, &self.shared.profile);
        let mut shown = None;
        if ours && let Some(mut matched) = self.matched.take() {
            self.follow(&mut matched, status, &state, newly, ended.as_deref());
            shown = Some((matched.job_id.clone(), matched.name.clone()));
            self.matched = Some(matched);
        }
        let view = LanPrinterStatus {
            nozzles: status
                .nozzles
                .iter()
                .map(|(id, reading)| NozzleReading {
                    id: *id,
                    current_c: reading.current,
                    target_c: reading.target,
                })
                .collect(),
            bed: TemperatureReading {
                current_c: status.bed.current,
                target_c: status.bed.target,
            },
            chamber: TemperatureReading {
                current_c: status.chamber.current,
                target_c: status.chamber.target,
            },
            reports: status.reports,
            matched_name: shown.as_ref().map(|(_, name)| name.clone()),
            job_id: shown.map(|(job_id, _)| job_id),
            connected,
            ended,
            state,
        };
        let _ = self.shared.events.send(CoreEvent::Printer {
            status: Box::new(view.clone()),
        });
        view
    }

    /// The one handed-off job whose placed file has exactly the name `name` (case
    /// included); none when no job or more than one has.
    fn find(&self, name: &str) -> Option<Matched> {
        let jobs = self
            .shared
            .store
            .print_jobs_with_status(HANDED_OFF, MATCH_WINDOW)
            .ok()?;
        let mut found = jobs.into_iter().filter_map(|job| {
            let detail: PrintJobDetail = serde_json::from_value(job.detail.clone()).ok()?;
            let placed = detail.path.as_ref()?.file_name()?.to_str()?.to_owned();
            let placed = job_key(&placed)?;
            (placed == name).then_some((job.id, placed))
        });
        let (job_id, name) = found.next()?;
        if found.next().is_some() {
            return None;
        }
        Some(Matched {
            job_id,
            name,
            tracker: JobTracker::new(),
            ended: false,
        })
    }

    fn follow(
        &self,
        matched: &mut Matched,
        status: &PrinterStatus,
        state: &PrinterState,
        newly: bool,
        session_ended: Option<&str>,
    ) {
        let Ok(job) = self.shared.store.print_job(&matched.job_id) else {
            return;
        };
        let mut detail: PrintJobDetail =
            serde_json::from_value(job.detail.clone()).unwrap_or_default();
        let mut outcome = None;
        for event in matched.tracker.observe(status, &self.shared.profile) {
            match event {
                JobEvent::Phase { phase, headline } => self.shared.emit(
                    &job,
                    PrintEvent::Phase {
                        phase: phase_name(phase),
                        headline,
                    },
                ),
                JobEvent::Progress {
                    percent,
                    remaining_minutes,
                } => self.shared.emit(
                    &job,
                    PrintEvent::Progress {
                        percent,
                        remaining_minutes,
                    },
                ),
                JobEvent::Problem(explanation) => {
                    let problem = PrinterProblem::from(explanation);
                    if !detail.problems.contains(&problem) {
                        detail.problems.push(problem.clone());
                    }
                    self.shared.emit(&job, PrintEvent::Problem { problem });
                }
                JobEvent::Ended(ended) => {
                    if let JobOutcome::Failed { problems } = &ended {
                        for problem in problems.iter().cloned().map(PrinterProblem::from) {
                            if !detail.problems.contains(&problem) {
                                detail.problems.push(problem);
                            }
                        }
                    }
                    outcome = Some(ended);
                }
            }
        }
        // Once the printer said how the job ended, that stays the job's last word.
        if matched.ended && outcome.is_none() {
            return;
        }
        detail.phase = Some(state.phase.clone());
        detail.headline = Some(state.headline.clone());
        detail.progress_percent = state.progress_percent;
        detail.remaining_minutes = state.remaining_minutes;
        let source = format!(
            "followed from the printer's own status reports, matched by the job name `{}`; \
             Hotend did not start this print and sends the printer nothing",
            truncate(&matched.name, 120)
        );
        detail.message = Some(match (&outcome, session_ended) {
            (Some(JobOutcome::Finished), _) => {
                format!("the printer reports this job finished ({source})")
            }
            (Some(JobOutcome::Cancelled), _) => {
                format!("the printer reports this job cancelled ({source})")
            }
            (Some(JobOutcome::Failed { .. }), _) => {
                format!("the printer reports this job failed ({source})")
            }
            (None, Some(why)) => format!(
                "no longer followed: the printer status connection ended ({}); {source}",
                truncate(why, 300)
            ),
            (None, None) => source,
        });
        if outcome.is_some() {
            matched.ended = true;
        }
        let updated = self.shared.store.update_print_job(
            &job.id,
            &[HANDED_OFF],
            HANDED_OFF,
            None,
            &json!(detail),
        );
        if newly && let Ok((job, true)) = updated {
            self.shared.emit(
                &job,
                PrintEvent::Status {
                    job: Box::new(PrintJob::from(job.clone())),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_is_known_by_its_file_name_without_folders_and_ending() {
        assert_eq!(
            job_key("/mnt/d/Models/Hotend/Box-v3-0123abcd.gcode.3mf").as_deref(),
            Some("Box-v3-0123abcd"),
            "case is kept"
        );
        assert_eq!(job_key(r"D:\Models\box.3mf").as_deref(), Some("box"));
        assert_eq!(job_key("box-v3").as_deref(), Some("box-v3"));
        assert_eq!(
            job_key("/data/Metadata/plate_1.gcode").as_deref(),
            Some("plate_1")
        );
        assert_eq!(job_key("  "), None);
        assert_eq!(job_key(".gcode.3mf"), None);
        assert_eq!(job_key("dir/"), None);
    }

    #[test]
    fn the_reported_name_is_subtask_name_when_given_else_the_gcode_file_stem() {
        let status = |subtask: Option<&str>, file: Option<&str>| PrinterStatus {
            job_name: subtask.map(str::to_owned),
            gcode_file: file.map(str::to_owned),
            ..PrinterStatus::default()
        };
        assert_eq!(
            reported_key(&status(
                Some("Box-v3-0123abcd"),
                Some("/data/Other.gcode.3mf")
            )),
            Some("Box-v3-0123abcd".to_owned()),
            "subtask_name wins; the file is not a second chance"
        );
        assert_eq!(
            reported_key(&status(None, Some("/data/Box-v3-0123abcd.gcode.3mf"))),
            Some("Box-v3-0123abcd".to_owned())
        );
        assert_eq!(
            reported_key(&status(Some("  "), Some("/data/Box.gcode.3mf"))),
            Some("Box".to_owned())
        );
        assert_eq!(reported_key(&status(None, None)), None);
        assert_ne!(
            reported_key(&status(Some("BOX-V3-0123ABCD"), None)),
            job_key("Box-v3-0123abcd.gcode.3mf"),
            "compared case-sensitively"
        );
    }
}
