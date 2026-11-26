//! The effort level of the person's settings, and the models it is sent to.
//!
//! Anthropic's Messages API takes the level as `output_config.effort` (general
//! availability, no beta header). Hotend's egress writes it into a generating request of
//! an Anthropic run, and only when the run's model is one the provider documents as
//! accepting that level. For any other model name, typed by the person or new, the field
//! is left out: an unknown model may refuse the request, and the provider's own default
//! then applies. Other providers get nothing: Hotend maps no field of theirs. An
//! `output_config.effort` the agent wrote into a request is removed from every request,
//! whatever the provider, route or model, so the level is never the agent's; an
//! `output_config` left empty by that is removed whole.
//!
//! Source: Anthropic's API documentation (effort and adaptive thinking, models overview),
//! as read on 18 November 2026. A model added to the list needs a fresh reading.

use serde::{Deserialize, Serialize};

/// How much the model may think and spend on one answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    /// The default of Hotend.
    #[default]
    High,
    Xhigh,
    Max,
}

impl Effort {
    /// Every level, lowest first.
    pub const ALL: [Effort; 5] = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];

    /// The level as the API spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

const ALL_FIVE: &[Effort] = &Effort::ALL;
const WITHOUT_XHIGH: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High, Effort::Max];
const UP_TO_HIGH: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High];

/// The model names Anthropic documents as accepting `output_config.effort`, each with the
/// levels it accepts. Exact names only: a dated or otherwise altered name is not in the
/// list and gets no effort field.
const DOCUMENTED: &[(&str, &[Effort])] = &[
    ("claude-fable-5-2", ALL_FIVE),
    ("claude-fable-5-1", ALL_FIVE),
    ("claude-fable-5", ALL_FIVE),
    ("claude-mythos-5-2", ALL_FIVE),
    ("claude-mythos-5-1", ALL_FIVE),
    ("claude-opus-5-5", ALL_FIVE),
    ("claude-opus-5", ALL_FIVE),
    ("claude-opus-4-8", ALL_FIVE),
    ("claude-opus-4-7", ALL_FIVE),
    ("claude-sonnet-5-5", ALL_FIVE),
    ("claude-sonnet-5", ALL_FIVE),
    ("claude-haiku-5-5", ALL_FIVE),
    ("claude-opus-4-6", WITHOUT_XHIGH),
    ("claude-sonnet-4-6", WITHOUT_XHIGH),
    ("claude-opus-4-5", UP_TO_HIGH),
];

/// The value of `output_config.effort` for an Anthropic request of `model`, or `None`
/// when the model is not documented to accept `effort` at that level.
pub fn anthropic_effort(model: &str, effort: Effort) -> Option<&'static str> {
    DOCUMENTED
        .iter()
        .find(|(name, _)| *name == model)
        .filter(|(_, levels)| levels.contains(&effort))
        .map(|_| effort.as_str())
}

/// Whether the model is in the documented list at all (for the settings screen).
pub fn documented_model(model: &str) -> bool {
    DOCUMENTED.iter().any(|(name, _)| *name == model)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn the_default_is_high_and_old_settings_read_as_high() {
        assert_eq!(Effort::default(), Effort::High);
        let parsed: Effort = serde_json::from_str("\"xhigh\"").unwrap();
        assert_eq!(parsed, Effort::Xhigh);
        assert!(serde_json::from_str::<Effort>("\"extreme\"").is_err());
    }

    #[test]
    fn effort_is_sent_only_where_the_model_documents_the_level() {
        assert_eq!(
            anthropic_effort("claude-fable-5-2", Effort::High),
            Some("high")
        );
        assert_eq!(
            anthropic_effort("claude-opus-5-5", Effort::Max),
            Some("max")
        );
        assert_eq!(anthropic_effort("claude-opus-4-6", Effort::Xhigh), None);
        assert_eq!(anthropic_effort("claude-opus-4-5", Effort::Max), None);
        assert_eq!(
            anthropic_effort("claude-opus-4-5", Effort::High),
            Some("high")
        );
        // Documented to refuse the field, or unknown: left out.
        assert_eq!(anthropic_effort("claude-haiku-4-5", Effort::High), None);
        assert_eq!(anthropic_effort("claude-sonnet-4-5", Effort::Low), None);
        assert_eq!(
            anthropic_effort("claude-fable-5-2-20261101", Effort::High),
            None
        );
        assert_eq!(anthropic_effort("my-own-model", Effort::High), None);
    }

    #[test]
    fn the_settings_screen_uses_the_same_table() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/desktop/src/host/data/providers.json"
        );
        let text = std::fs::read_to_string(path).expect("the window's provider list");
        let data: serde_json::Value = serde_json::from_str(&text).expect("JSON");
        let screen = data["anthropic_effort"]
            .as_object()
            .expect("anthropic_effort");
        assert_eq!(screen.len(), DOCUMENTED.len());
        for (model, levels) in DOCUMENTED {
            let shown: Vec<&str> = screen[*model]
                .as_array()
                .expect("levels")
                .iter()
                .map(|level| level.as_str().expect("a level"))
                .collect();
            let ours: Vec<&str> = levels.iter().map(|level| level.as_str()).collect();
            assert_eq!(shown, ours, "{model}");
        }
        assert_eq!(
            data["providers"]["anthropic"]["models"][0],
            serde_json::json!("claude-fable-5-2")
        );
    }
}
