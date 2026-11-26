//! Plain language for what the printer reports. Every text keeps the raw code beside it,
//! because the code is what Bambu's support, wiki and the printer's own screen use.
//!
//! Hotend does not ship Bambu's message catalogue. An HMS code gets a general sentence
//! from its module and severity, a specific one when `profiles/bambu.toml` knows the code,
//! and a link to Bambu's page for it.

use serde::Serialize;

use crate::profile::PrinterProfile;
use crate::protocol::hms::{HmsCode, Module, Severity, print_error_text};
use crate::status::{Phase, PrinterStatus, stage_name};

/// One thing to tell the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Explanation {
    /// What happened and what to do, in plain words.
    pub message: String,
    /// The printer's own code, for example `HMS_0700_2000_0002_0001` or `0700_8012`.
    pub raw_code: String,
    /// A stable word for conditions Hotend acts on (`cancelled`, `not_authorised`, ...).
    pub meaning: Option<String>,
    /// Whether the printer says it cannot continue.
    pub blocking: bool,
    /// Bambu's page for the code.
    pub help_url: Option<String>,
}

/// Explains one HMS entry.
pub fn explain_hms(code: HmsCode, profile: &PrinterProfile) -> Explanation {
    let known = profile.known_hms(code.attr, code.code);
    let part = match code.module() {
        Module::Mainboard => "its main board",
        Module::Camera => "its camera or print inspection",
        Module::Ams => "the filament system (AMS)",
        Module::Toolhead => "the tool head",
        Module::Motion => "its motion system or heaters",
        Module::Unknown => "one of its parts",
    };
    let general = match code.severity() {
        Severity::Fatal => format!(
            "The printer reports a fatal problem with {part} and cannot continue. Look at the printer's screen before doing anything else."
        ),
        Severity::Serious => format!(
            "The printer reports a serious problem with {part}. The print may be paused; look at the printer's screen."
        ),
        Severity::Common => {
            format!("The printer reports a problem with {part}. Look at the printer's screen.")
        }
        Severity::Info => format!("The printer has a note about {part}."),
        Severity::Unknown => format!("The printer reports something about {part}."),
    };
    Explanation {
        message: known.map_or(general, |known| known.message.clone()),
        raw_code: code.to_string(),
        meaning: known.map(|known| known.meaning.clone()),
        blocking: matches!(code.severity(), Severity::Fatal | Severity::Serious),
        help_url: Some(format!(
            "https://wiki.bambulab.com/en/x1/troubleshooting/hmscode/{}",
            code.digits()
        )),
    }
}

/// Explains a print error number.
pub fn explain_print_error(code: u32, profile: &PrinterProfile) -> Explanation {
    let known = profile.known_print_error(code);
    Explanation {
        message: known.map_or_else(
            || {
                "The printer stopped the job with an error. Look at the printer's screen for the reason."
                    .to_owned()
            },
            |known| known.message.clone(),
        ),
        raw_code: print_error_text(code),
        meaning: known.map(|known| known.meaning.clone()),
        blocking: known.is_none_or(|known| known.meaning != "cancelled"),
        help_url: None,
    }
}

/// Everything wrong the printer reports now, most serious first.
pub fn problems(status: &PrinterStatus, profile: &PrinterProfile) -> Vec<Explanation> {
    let mut out: Vec<Explanation> = status
        .print_error
        .map(|code| explain_print_error(code, profile))
        .into_iter()
        .chain(status.hms.iter().map(|code| explain_hms(*code, profile)))
        .collect();
    out.sort_by_key(|explanation| !explanation.blocking);
    out
}

/// One sentence on what the printer is doing.
pub fn headline(status: &PrinterStatus) -> String {
    let stage = status.stage.map(stage_name).unwrap_or("unknown");
    let progress = || {
        let percent = status.progress_percent.unwrap_or(0);
        match status.remaining_minutes {
            Some(minutes) if minutes > 0 => format!("{percent}% done, about {minutes} min left"),
            _ => format!("{percent}% done"),
        }
    };
    match status.phase() {
        Phase::Idle if status.ams_drying() => {
            "The printer is idle; the AMS is drying filament.".to_owned()
        }
        Phase::Idle => "The printer is idle.".to_owned(),
        Phase::Preparing => match stage {
            "auto_bed_leveling" => "Getting ready: levelling the bed.".to_owned(),
            "homing_toolhead" => "Getting ready: homing.".to_owned(),
            "filament_loading" | "changing_filament" | "preparing_ams" => {
                "Getting ready: loading filament.".to_owned()
            }
            "cleaning_nozzle_tip" => "Getting ready: cleaning the nozzle.".to_owned(),
            _ => "Getting ready to print.".to_owned(),
        },
        Phase::Heating => "Heating up.".to_owned(),
        Phase::Printing => format!("Printing: {}.", progress()),
        Phase::Paused => match stage {
            "paused_filament_runout" => "Paused: the filament ran out.".to_owned(),
            "paused_user" => "Paused by you.".to_owned(),
            "paused_front_cover_falling" => "Paused: the front cover came off.".to_owned(),
            "paused_nozzle_clog" => "Paused: the nozzle may be clogged.".to_owned(),
            _ => "Paused. Look at the printer's screen for the reason.".to_owned(),
        },
        Phase::Finished => "The print is finished.".to_owned(),
        Phase::Failed => "The print did not finish.".to_owned(),
        Phase::Offline => "The printer is offline.".to_owned(),
        Phase::Unknown => "Waiting for the printer to report its state.".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::profile::Profiles;
    use crate::status::GcodeState;

    fn x2d() -> PrinterProfile {
        Profiles::builtin().unwrap().printer("bambu-x2d").unwrap()
    }

    #[test]
    fn codes_get_plain_words_and_keep_the_raw_code() {
        let profile = x2d();
        let known = explain_hms(
            HmsCode {
                attr: 0x0300_0100,
                code: 0x0001_0007,
            },
            &profile,
        );
        assert!(known.message.contains("heated bed"));
        assert_eq!(known.raw_code, "HMS_0300_0100_0001_0007");
        assert!(known.blocking);
        assert!(known.help_url.unwrap().ends_with("/0300_0100_0001_0007"));

        let general = explain_hms(
            HmsCode {
                attr: 0x0700_2000,
                code: 0x0003_0001,
            },
            &profile,
        );
        assert!(general.message.contains("filament system (AMS)"));
        assert_eq!(general.raw_code, "HMS_0700_2000_0003_0001");
        assert!(!general.blocking);
        assert_eq!(general.meaning, None);

        let cancelled = explain_print_error(0x0300_400C, &profile);
        assert_eq!(cancelled.meaning.as_deref(), Some("cancelled"));
        assert!(!cancelled.blocking);
        let developer_mode = explain_print_error(0x0502_4007, &profile);
        assert!(developer_mode.message.contains("Developer Mode"));
        assert_eq!(developer_mode.raw_code, "0502_4007");
        let unknown = explain_print_error(0x0C00_1234, &profile);
        assert!(unknown.blocking && unknown.message.contains("screen"));
        assert_eq!(unknown.raw_code, "0C00_1234");
    }

    #[test]
    fn problems_are_listed_most_serious_first_and_states_have_headlines() {
        let profile = x2d();
        let mut status = PrinterStatus {
            gcode_state: GcodeState::Running,
            stage: Some(0),
            progress_percent: Some(40),
            remaining_minutes: Some(25),
            ..PrinterStatus::default()
        };
        assert_eq!(headline(&status), "Printing: 40% done, about 25 min left.");
        status.hms = vec![
            HmsCode {
                attr: 0x0C00_0100,
                code: 0x0004_0001,
            },
            HmsCode {
                attr: 0x0700_0100,
                code: 0x0001_0001,
            },
        ];
        let listed = problems(&status, &profile);
        assert_eq!(listed.len(), 2);
        assert!(listed[0].blocking && !listed[1].blocking);
        status.gcode_state = GcodeState::Pause;
        status.stage = Some(6);
        assert_eq!(headline(&status), "Paused: the filament ran out.");
        status.gcode_state = GcodeState::Unknown;
        assert!(headline(&status).starts_with("Waiting"));
    }
}
