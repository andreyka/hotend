//! After a start: did the print really begin, and how does the job go?
//!
//! A printer can accept the start command and stay idle (the X2D does while its AMS is
//! drying filament, for example). [`StartWatchdog`] therefore never trusts the command's
//! acceptance: it watches the status reports and says [`StartVerdict::Started`] only when
//! the printer reports work on this job, and after a deadline says clearly that nothing
//! started and why that may be.
//!
//! Both types are pure state machines: the caller feeds them the status it polled and, for
//! the watchdog, how long it has waited. They own no clock and no connection, so they work
//! the same on the simulated printer's accelerated time and on a real one.
//!
//! [`JobTracker`] follows a job to its end and turns reports into events for the interface.

use std::time::Duration;

use serde::Serialize;

use crate::explain::{Explanation, problems};
use crate::profile::PrinterProfile;
use crate::protocol::hms::HmsCode;
use crate::status::{Phase, PrinterStatus};

/// What the watchdog concludes from the reports so far.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum StartVerdict {
    /// Nothing conclusive yet; keep polling.
    Waiting,
    /// The printer reports work on the job.
    Started {
        /// What it is doing.
        phase: Phase,
    },
    /// The deadline passed and the printer did not begin.
    NotStarted {
        /// Why, as far as the reports say.
        reason: NotStartedReason,
        /// What to tell the user.
        message: String,
    },
    /// The printer reported a failure instead of starting.
    Failed {
        /// The printer's errors, explained.
        problems: Vec<Explanation>,
    },
}

/// Why a start did not lead to a print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotStartedReason {
    /// The printer stayed idle while an AMS is drying filament.
    AmsDrying,
    /// The printer stayed idle for no reported reason.
    StayedIdle,
    /// The printer is working, but on a job with another name.
    OtherJob,
    /// No status report arrived after the start.
    NoReports,
}

/// Watches for the beginning of one job.
#[derive(Debug, Clone)]
pub struct StartWatchdog {
    job_name: String,
    deadline: Duration,
    reports_at_start: u64,
    /// Errors the printer already showed before the start; they are not this job's.
    errors_at_start: (Option<u32>, Vec<HmsCode>),
    failed_at_start: bool,
    verdict: StartVerdict,
}

impl StartWatchdog {
    /// How long a printer gets to show that it began, unless set otherwise.
    pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(90);

    /// A watchdog for the job `job_name`, started when the printer's state was `before`.
    pub fn new(job_name: &str, before: &PrinterStatus) -> Self {
        Self {
            job_name: job_name.to_owned(),
            deadline: Self::DEFAULT_DEADLINE,
            reports_at_start: before.reports,
            errors_at_start: (before.print_error, before.hms.clone()),
            failed_at_start: before.phase() == Phase::Failed,
            verdict: StartVerdict::Waiting,
        }
    }

    /// Sets the deadline.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// The deadline.
    pub fn deadline(&self) -> Duration {
        self.deadline
    }

    /// Feeds the latest status, `waited` after the start command. Once a verdict other
    /// than [`StartVerdict::Waiting`] is reached it does not change.
    pub fn observe(
        &mut self,
        status: &PrinterStatus,
        waited: Duration,
        profile: &PrinterProfile,
    ) -> StartVerdict {
        if self.verdict != StartVerdict::Waiting {
            return self.verdict.clone();
        }
        let fresh = status.reports > self.reports_at_start;
        let phase = status.phase();
        let ours = status
            .job_name
            .as_deref()
            .is_none_or(|name| name == self.job_name);
        let working = matches!(
            phase,
            Phase::Preparing | Phase::Heating | Phase::Printing | Phase::Paused
        );
        // An error that was not there before the start: the printer refused the job (no
        // Developer Mode, a filament map it cannot use) or failed it at once.
        let new_error = has_errors(status)
            && (status.print_error != self.errors_at_start.0
                || status.hms != self.errors_at_start.1
                || (phase == Phase::Failed && !self.failed_at_start));
        if fresh && working && ours {
            self.verdict = StartVerdict::Started { phase };
        } else if fresh && !working && new_error {
            self.verdict = StartVerdict::Failed {
                problems: problems(status, profile),
            };
        } else if waited >= self.deadline {
            let (reason, message) = if !fresh {
                (
                    NotStartedReason::NoReports,
                    "The printer has not answered since the job was sent. Check that it is on and connected, then look at its screen.",
                )
            } else if working {
                (
                    NotStartedReason::OtherJob,
                    "The printer is busy with another job, so yours did not start. Send it again when the printer is free.",
                )
            } else if status.ams_drying() {
                (
                    NotStartedReason::AmsDrying,
                    "The printer accepted the job but did not start it: the AMS is drying filament. Stop the drying on the printer's screen, or wait until it finishes, then send the job again.",
                )
            } else {
                (
                    NotStartedReason::StayedIdle,
                    "The printer accepted the job but did not start it. Look at the printer's screen: it may be waiting for you, or the job may need to be sent again.",
                )
            };
            self.verdict = StartVerdict::NotStarted {
                reason,
                message: message.to_owned(),
            };
        }
        self.verdict.clone()
    }
}

fn has_errors(status: &PrinterStatus) -> bool {
    status.print_error.is_some() || !status.hms.is_empty()
}

/// Something that happened to a tracked job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum JobEvent {
    /// The printer moved to another phase.
    Phase {
        /// The new phase.
        phase: Phase,
        /// One sentence for the interface.
        headline: String,
    },
    /// Progress changed.
    Progress {
        /// Percent done.
        percent: u8,
        /// Minutes the printer expects to need.
        remaining_minutes: Option<u32>,
    },
    /// The printer reports a problem it did not report before.
    Problem(Explanation),
    /// The job ended.
    Ended(JobOutcome),
}

/// How a job ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum JobOutcome {
    /// The print finished.
    Finished,
    /// The print was cancelled, at the printer or from Hotend.
    Cancelled,
    /// The print failed.
    Failed {
        /// The printer's errors, explained, raw codes included.
        problems: Vec<Explanation>,
    },
}

/// Follows one started job through the status reports.
#[derive(Debug, Clone)]
pub struct JobTracker {
    phase: Option<Phase>,
    percent: Option<u8>,
    seen: Vec<String>,
    outcome: Option<JobOutcome>,
}

impl Default for JobTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl JobTracker {
    /// A tracker for a job the watchdog has seen start.
    pub fn new() -> Self {
        Self {
            phase: None,
            percent: None,
            seen: Vec::new(),
            outcome: None,
        }
    }

    /// How the job ended, once it has.
    pub fn outcome(&self) -> Option<&JobOutcome> {
        self.outcome.as_ref()
    }

    /// Feeds the latest status; returns what changed since the last call.
    pub fn observe(&mut self, status: &PrinterStatus, profile: &PrinterProfile) -> Vec<JobEvent> {
        let mut events = Vec::new();
        if self.outcome.is_some() {
            return events;
        }
        let phase = status.phase();
        if self.phase != Some(phase) {
            self.phase = Some(phase);
            events.push(JobEvent::Phase {
                phase,
                headline: crate::explain::headline(status),
            });
        }
        if let Some(percent) = status.progress_percent
            && self.percent != Some(percent)
            && matches!(phase, Phase::Printing | Phase::Paused)
        {
            self.percent = Some(percent);
            events.push(JobEvent::Progress {
                percent,
                remaining_minutes: status.remaining_minutes,
            });
        }
        let current = problems(status, profile);
        for problem in &current {
            if !self.seen.contains(&problem.raw_code) && self.seen.len() < 256 {
                self.seen.push(problem.raw_code.clone());
                if problem.meaning.as_deref() != Some("cancelled") {
                    events.push(JobEvent::Problem(problem.clone()));
                }
            }
        }
        let outcome = match phase {
            Phase::Finished => Some(JobOutcome::Finished),
            Phase::Failed => {
                let cancelled = current
                    .iter()
                    .any(|problem| problem.meaning.as_deref() == Some("cancelled"));
                Some(if cancelled {
                    JobOutcome::Cancelled
                } else {
                    JobOutcome::Failed { problems: current }
                })
            }
            _ => None,
        };
        if let Some(outcome) = outcome {
            self.outcome = Some(outcome.clone());
            events.push(JobEvent::Ended(outcome));
        }
        events
    }
}
