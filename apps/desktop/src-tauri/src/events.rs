//! The core's event stream as the window receives it.
//!
//! Guarantee: every event the core reports is passed on in order, unchanged. A reader that
//! falls behind is told how many events it lost (`lagged`), so it can read the current
//! state again; nothing is invented and nothing but the core's events is sent.

use hotend_core::{CoreEvent, LanPrinterStatus};
use serde::Serialize;
use tokio::sync::broadcast;

/// One message of the event stream.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ShellEvent {
    /// An agent run's or a print job's event, as the core reports it.
    Core(CoreEvent),
    /// What a printer followed over the local network reported (`CoreEvent::Printer`), in
    /// the core's own form: `{"channel": "printer", "status": ...}`. Read-only: nothing on
    /// this stream reaches the printer.
    Printer(PrinterReport),
    Lagged(Lagged),
}

/// A printer's report, as `CoreEvent::Printer` serialises it.
#[derive(Debug, Clone, Serialize)]
pub struct PrinterReport {
    channel: &'static str,
    pub status: Box<LanPrinterStatus>,
}

/// The reader was too slow and `missed` events were dropped.
#[derive(Debug, Clone, Serialize)]
pub struct Lagged {
    channel: &'static str,
    pub missed: u64,
}

impl ShellEvent {
    fn lagged(missed: u64) -> Self {
        Self::Lagged(Lagged {
            channel: "lagged",
            missed,
        })
    }

    /// The core's event as the window receives it: a printer's report as its own variant,
    /// everything else as it is.
    pub fn from_core(event: CoreEvent) -> Self {
        match event {
            CoreEvent::Printer { status } => Self::Printer(PrinterReport {
                channel: "printer",
                status,
            }),
            other => Self::Core(other),
        }
    }
}

/// Forwards the core's events (`Core::events()`) to `send` until `send` returns false (the
/// reader is gone) or the core closes.
pub async fn forward(
    mut events: broadcast::Receiver<CoreEvent>,
    mut send: impl FnMut(ShellEvent) -> bool,
) {
    loop {
        let event = match events.recv().await {
            Ok(event) => ShellEvent::from_core(event),
            Err(broadcast::error::RecvError::Lagged(missed)) => ShellEvent::lagged(missed),
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if !send(event) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_lag_notice_is_its_own_channel() {
        let json = serde_json::to_value(ShellEvent::lagged(7)).unwrap();
        assert_eq!(json, serde_json::json!({"channel": "lagged", "missed": 7}));
    }

    /// A printer's report reaches the window exactly as the core serialises it.
    #[test]
    fn a_printer_report_is_passed_on_unchanged() {
        let status: LanPrinterStatus = serde_json::from_value(serde_json::json!({
            "state": {
                "phase": "printing", "headline": "Printing", "progress_percent": 40,
                "remaining_minutes": 12, "layer": 8, "total_layers": 20, "job_name": "box",
                "bed_c": 60.0, "problems": [], "ams": []
            },
            "nozzles": [{"id": 0, "current_c": 219.5, "target_c": 220.0}],
            "bed": {"current_c": 60.0, "target_c": 60.0},
            "chamber": {"current_c": null, "target_c": null},
            "reports": 3, "job_id": null, "matched_name": null, "connected": true, "ended": null
        }))
        .unwrap();
        let core = CoreEvent::Printer {
            status: Box::new(status),
        };
        let expected = serde_json::to_value(&core).unwrap();
        let shell = ShellEvent::from_core(core);
        assert!(matches!(shell, ShellEvent::Printer(_)));
        assert_eq!(serde_json::to_value(&shell).unwrap(), expected);
        assert_eq!(expected["channel"], "printer");
    }
}
