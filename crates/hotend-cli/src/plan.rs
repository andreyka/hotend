//! `hotend plan`: Hotend's print settings for a plate, with the reason for each.
//!
//! Read-only: the plan is computed by the core from the plate's measured facts and the
//! settings. No command here takes a slicer key, and none starts a print.

use std::path::PathBuf;

use hotend_core::{PrintPlanView, SecretBackend};
use serde_json::Value;

use crate::error::CliError;
use crate::{PlanCommand, app, render};

fn print_json(value: &impl serde::Serialize) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(value).map_err(|e| CliError::failed(e.to_string()))?;
    println!("{text}");
    Ok(())
}

pub async fn run(command: PlanCommand, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let core = app::open_core(data_dir, SecretBackend::from_env()).await?;
    let (target, explain, json) = match command {
        PlanCommand::Show { target, json } => (target, false, json),
        PlanCommand::Explain { target } => (target, true, false),
    };
    let project = app::find_project(&core, target.project.project.as_deref()).await?;
    let view = core
        .print_plan_get(&project.id, target.version, target.plate)
        .await?;
    if json {
        return print_json(&view);
    }
    println!("project {}  {}", project.id, render::clean(&project.name));
    let text = if explain {
        explain_text(&view)
    } else {
        show_text(&view)
    };
    print!("{}", render::clean(&text));
    Ok(())
}

/// A value as people read it: `as the installed profile has it` for an unset field.
fn value_text(value: &Value, unit: &str) -> String {
    let text = match value {
        Value::Null => return "as the installed profile has it".to_owned(),
        Value::String(text) => text.clone(),
        Value::Bool(true) => "on".to_owned(),
        Value::Bool(false) => "off".to_owned(),
        other => other.to_string(),
    };
    match unit {
        "" | "enum" | "flag" | "step" | "count" => text,
        "percent" => format!("{text} %"),
        unit => format!("{text} {unit}"),
    }
}

fn header(view: &PrintPlanView, out: &mut String) {
    out.push_str(&format!(
        "version {}, plate {}: plan {}\n",
        view.version,
        view.plate,
        &view.sha256[..16.min(view.sha256.len())]
    ));
    out.push_str(&format!(
        "a slice of this plate uses {}\n",
        view.settings_source
    ));
    out.push_str(&format!(
        "nozzle: {} mm{}\n",
        view.nozzle_diameter_mm,
        if view.nozzle_size_set {
            ""
        } else {
            " (assumed)"
        }
    ));
    if let Some(baseline) = &view.baseline {
        out.push_str(&format!(
            "proven baseline: {} / {} / {}, printed {} ({}); it set {}\n",
            baseline.printer_id,
            baseline.plate_type,
            baseline.material_id,
            baseline.printed_on,
            baseline.source,
            if baseline.fields.is_empty() {
                "nothing the rules did not already give".to_owned()
            } else {
                baseline.fields.join(", ")
            }
        ));
    }
    if let Some(blocked) = &view.blocked {
        out.push_str(&format!(
            "BLOCKED ({}): {}\n",
            blocked.code, blocked.sentence
        ));
    }
    for question in &view.questions {
        out.push_str(&format!(
            "question ({}): {}\n",
            question.code, question.sentence
        ));
    }
}

fn notices(view: &PrintPlanView, out: &mut String) {
    for (label, list) in [
        ("warning", &view.warnings),
        ("note", &view.notes),
        ("assumed", &view.assumptions),
    ] {
        for notice in list {
            out.push_str(&format!("{label} ({}): {}\n", notice.code, notice.sentence));
        }
    }
}

/// The plan as a short table of values, then the list.
pub fn show_text(view: &PrintPlanView) -> String {
    let mut out = String::new();
    header(view, &mut out);
    if !view.decisions.is_empty() {
        out.push_str("settings:\n");
        for decision in &view.decisions {
            out.push_str(&format!(
                "  {:<36} {:<32} {}\n",
                decision.field,
                value_text(&decision.value, &decision.unit),
                decision.origin
            ));
        }
    }
    notices(view, &mut out);
    if !view.checklist.is_empty() {
        out.push_str("before you print:\n");
        for item in &view.checklist {
            out.push_str(&format!("  - {}\n", item.text));
        }
    }
    out.push_str("`hotend plan explain` gives the reason for every value\n");
    out
}

/// One line per decision: field, value, where it came from, rule id, basis, sentence.
pub fn explain_text(view: &PrintPlanView) -> String {
    let mut out = String::new();
    header(view, &mut out);
    for decision in &view.decisions {
        out.push_str(&format!(
            "{} = {} [{}; {}, {}]: {}\n",
            decision.field,
            value_text(&decision.value, &decision.unit),
            decision.origin,
            decision.rule,
            decision.basis,
            decision.sentence
        ));
    }
    notices(view, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_read_as_words_with_their_unit() {
        assert_eq!(value_text(&json!(5), "mm"), "5 mm");
        assert_eq!(value_text(&json!(50), "percent"), "50 %");
        assert_eq!(value_text(&json!("outer"), "enum"), "outer");
        assert_eq!(value_text(&json!(false), "flag"), "off");
        assert_eq!(
            value_text(&Value::Null, "mm"),
            "as the installed profile has it"
        );
    }
}
