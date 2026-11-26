//! The adhesion level.
//!
//! Level 0 is the normal first layer, 1 "at risk", 2 "stubborn", 3 "ask". The geometry gives
//! a level, a silk spool raises it by one, the print history and a proven baseline are floors, and the agent or
//! the person may ask for more. Nothing here can lower a level: every step takes a maximum.

use crate::inputs::{Asked, History};
use crate::model::{Basis, RuleId, Source, Why};
use crate::ruledata::LevelRules;

/// The level of a plate and how it came about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LevelOutcome {
    /// The level of the plan.
    pub(crate) level: i64,
    /// Who set it, and by which rule.
    pub(crate) why: Why,
    /// The sentence of the decision.
    pub(crate) sentence: &'static str,
    /// The level the geometry, the spool and the history give.
    pub(crate) rule_level: i64,
    /// Who set `rule_level`.
    pub(crate) rule_why: Why,
    /// The level that was asked for, when it was not taken as asked.
    pub(crate) not_taken: Option<i64>,
}

fn why(source: Source, rule: RuleId) -> Why {
    Why {
        source,
        rule,
        basis: Basis::Judgment,
    }
}

/// Decides the level. `geometry` is 0, 1 or 2, from the brim rules; `proven` is the level
/// of the proven baseline when one applies, a floor like the history.
pub(crate) fn decide(
    geometry: i64,
    silk: bool,
    history: &History,
    proven: Option<u8>,
    asked: Option<&Asked<u8>>,
    rules: &LevelRules,
) -> LevelOutcome {
    let (mut level, mut reason, mut sentence) = match geometry {
        i64::MIN..=0 => (0, why(Source::Rule, RuleId::Lvl0), "level.0"),
        1 => (1, why(Source::Rule, RuleId::Lvl1), "level.1"),
        _ => (2, why(Source::Rule, RuleId::Lvl2), "level.2"),
    };
    if silk && level < rules.silk_max {
        level += 1;
        reason = why(Source::Rule, RuleId::LvlSilk);
        sentence = "level.silk";
    }
    let floor = i64::from(history.min_level());
    if floor > level {
        level = floor;
        reason = why(
            Source::History,
            if level >= 3 {
                RuleId::Lvl3
            } else {
                RuleId::LvlHist
            },
        );
        sentence = if level >= 3 {
            "level.3"
        } else {
            "level.history"
        };
    }
    if let Some(proven) = proven.map(i64::from)
        && proven > level
    {
        level = proven;
        reason = Why {
            source: Source::Proven,
            rule: RuleId::Prv1,
            basis: Basis::Measured,
        };
        sentence = "level.proven";
    }
    let rule_level = level;
    let rule_why = reason;

    let mut not_taken = None;
    if let Some(asked) = asked {
        let wanted = i64::from(asked.value);
        // A request can raise the level, to `ask_max` at most. Level 3 comes from history
        // only, and a request below the rules' level changes nothing.
        let granted = wanted.min(rules.ask_max);
        if granted > level {
            level = granted;
            reason = why(asked.by.source(), RuleId::LvlAsk);
            sentence = "level.asked";
        }
        if level != wanted {
            not_taken = Some(wanted);
        }
    }
    LevelOutcome {
        level,
        why: reason,
        sentence,
        rule_level,
        rule_why,
        not_taken,
    }
}
