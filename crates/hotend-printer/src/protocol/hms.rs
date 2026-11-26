//! Error codes the printer reports: HMS entries and the print error number.
//!
//! The code format, the severity levels and the module numbers follow ha-bambulab
//! (`pybambu/models.py` `HMSNotification`, `const.py` `HMS_SEVERITY_LEVELS` and
//! `HMS_MODULES`). The raw code is always kept: it is what Bambu's support and wiki use.

use std::fmt;

use serde::Serialize;

/// One health message of the printer: two 32-bit halves, shown as
/// `HMS_0300_0100_0001_0007`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct HmsCode {
    /// Upper half: module (top byte) and part.
    pub attr: u32,
    /// Lower half: severity (upper 16 bits) and number.
    pub code: u32,
}

/// How serious the printer says a health message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The printer cannot continue.
    Fatal,
    /// The print is at risk.
    Serious,
    /// Something to look at.
    Common,
    /// For information.
    Info,
    /// A level this version does not know.
    Unknown,
}

/// The part of the printer a health message is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Module {
    /// Main board.
    Mainboard,
    /// Camera and inspection.
    Camera,
    /// Filament system (AMS).
    Ams,
    /// Tool head.
    Toolhead,
    /// Motion controller.
    Motion,
    /// A module this version does not know.
    Unknown,
}

impl HmsCode {
    /// The code from a report's `attr` and `code`. `None` when either is missing, zero or
    /// out of range (an empty entry carries no message).
    pub fn from_report(attr: Option<i64>, code: Option<i64>) -> Option<Self> {
        let attr = u32::try_from(attr?).ok().filter(|v| *v > 0)?;
        let code = u32::try_from(code?).ok().filter(|v| *v > 0)?;
        Some(Self { attr, code })
    }

    /// The printer's severity level.
    pub fn severity(&self) -> Severity {
        match self.code >> 16 {
            1 => Severity::Fatal,
            2 => Severity::Serious,
            3 => Severity::Common,
            4 => Severity::Info,
            _ => Severity::Unknown,
        }
    }

    /// The part of the printer the message is about.
    pub fn module(&self) -> Module {
        match self.attr >> 24 {
            0x05 => Module::Mainboard,
            0x0C => Module::Camera,
            0x07 => Module::Ams,
            0x08 => Module::Toolhead,
            0x03 => Module::Motion,
            _ => Module::Unknown,
        }
    }

    /// The code with underscores but without the `HMS_` prefix, as Bambu's wiki addresses
    /// it: `0300_0100_0001_0007`.
    pub fn digits(&self) -> String {
        format!(
            "{:04X}_{:04X}_{:04X}_{:04X}",
            self.attr >> 16,
            self.attr & 0xFFFF,
            self.code >> 16,
            self.code & 0xFFFF
        )
    }
}

impl fmt::Display for HmsCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HMS_{}", self.digits())
    }
}

/// The print error number as the printer's screen shows it: `0700_8012`.
pub fn print_error_text(code: u32) -> String {
    format!("{:04X}_{:04X}", code >> 16, code & 0xFFFF)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn codes_are_formatted_and_classified() {
        // The example from ha-bambulab: attr 0x03000100, code 0x00010007.
        let code = HmsCode::from_report(Some(50_331_904), Some(65_543)).unwrap();
        assert_eq!(code.to_string(), "HMS_0300_0100_0001_0007");
        assert_eq!(code.severity(), Severity::Fatal);
        assert_eq!(code.module(), Module::Motion);
        let ams = HmsCode {
            attr: 0x0700_2000,
            code: 0x0002_0001,
        };
        assert_eq!(
            (ams.module(), ams.severity()),
            (Module::Ams, Severity::Serious)
        );
        assert_eq!(print_error_text(117_473_286), "0700_8006");
    }

    #[test]
    fn empty_or_impossible_entries_carry_no_code() {
        assert!(HmsCode::from_report(None, Some(1)).is_none());
        assert!(HmsCode::from_report(Some(0), Some(1)).is_none());
        assert!(HmsCode::from_report(Some(-5), Some(1)).is_none());
        assert!(HmsCode::from_report(Some(1 << 40), Some(1)).is_none());
        let odd = HmsCode {
            attr: 0xFF00_0000,
            code: 0x0009_0000,
        };
        assert_eq!(
            (odd.module(), odd.severity()),
            (Module::Unknown, Severity::Unknown)
        );
    }
}
