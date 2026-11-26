//! Envelopes: the values of a field Hotend is willing to deliver for one plate, and the
//! check every finished plan must pass (docs/PRINT_SETTINGS.md).
//!
//! Guarantees:
//! - every envelope lies inside the constant hard range of its field and holds the rules'
//!   own value, so "no change" is always deliverable;
//! - for every field of the sticking group the envelope starts at the rules' value and
//!   extends only to the safer side ([`side`]): a higher level, a wider brim, a hotter bed, a
//!   longer fan delay, a slower first layer, a lower fan cap. Nothing a person or a model
//!   asks for can weaken how the first layer holds;
//! - every speed, acceleration and flow field is a cap: its envelope ends at the rules' cap,
//!   so a change can lower a limit and never raise one;
//! - a change is judged in one place (`Bounds::judge`): a number outside the envelope is
//!   held to the nearest bound, a value outside an allowed set changes nothing;
//! - [`RuleBook::verify`] checks a finished plan against all of the above and against the
//!   cross-field rules. [`RuleBook::plan`] runs it on every plan it returns.

use crate::bounds::{self, Group, Kind};
use crate::explain::{Catalogue, P, PlanError};
use crate::inputs::{Asker, Baseline, PlanInputs};
use crate::model::{
    Answer, Answered, Basis, BrimKind, Decision, Envelope, Field, PrintPlan, RuleId, Value,
};
use crate::propose::Change;
use crate::rules::RuleBook;

/// Rounding slack when two decimals with two places are compared.
const EPS: f64 = 1e-9;

/// The side of the rules' value on which a number is at least as safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// A higher number is at least as safe: level, brim width, bed temperature, fan delay.
    Up,
    /// A lower number is at least as safe: first layer speeds, fan caps, the brim gap, and
    /// every cap on speed, acceleration and flow.
    Down,
    /// Only the rules' value is safe: nobody moves the field.
    Fixed,
    /// Neither side matters for how the print holds: shell and support values.
    Either,
}

/// The safe side of a field.
pub const fn side(field: Field) -> Side {
    match field {
        Field::Level
        | Field::BrimWidth
        | Field::FirstLayerHeight
        | Field::FirstLayerBed
        | Field::LayersBed
        | Field::PartFanOffLayers => Side::Up,
        Field::BrimGap
        | Field::FirstLayerWallSpeed
        | Field::FirstLayerInfillSpeed
        | Field::FirstLayerAccel
        | Field::AuxFanFirstLayers
        | Field::AuxFanMax
        | Field::OuterWallSpeedCap
        | Field::SpeedCap
        | Field::AccelCap
        | Field::OuterWallAccelCap
        | Field::MaxFlowCap
        | Field::JerkCap => Side::Down,
        Field::BrimKind
        | Field::SkirtLoops
        | Field::ElephantFoot
        | Field::FirstLayerLineWidth
        | Field::FirstLayerNozzle
        | Field::LayersNozzle => Side::Fixed,
        Field::PlateType
        | Field::Spool
        | Field::NozzleId
        | Field::Colour
        | Field::Intent
        | Field::Process
        | Field::WallLoops
        | Field::InfillPct
        | Field::InfillPattern
        | Field::Seam
        | Field::SupportsEnabled
        | Field::SupportsKind
        | Field::SupportsThreshold
        | Field::SupportsWhere
        | Field::PrimeTower => Side::Either,
    }
}

/// Whether `value` holds the print at least as well as `reference` does, for a field of
/// the sticking group or a cap. "No cap" is the weakest cap, and a brim kind is at least as
/// safe as another when it draws everything the other draws.
///
/// Both values must be given as they take effect: a caller that compares two plans puts
/// the recorded baseline in the place of a value that is left to the installed profile.
pub fn not_weaker(field: Field, value: &Value, reference: &Value) -> bool {
    if field == Field::BrimKind {
        let kind = |value: &Value| value.token().and_then(BrimKind::from_token);
        return matches!(
            (kind(value), kind(reference)),
            (Some(_), Some(BrimKind::None))
                | (
                    Some(BrimKind::Outer | BrimKind::OuterAndInner),
                    Some(BrimKind::Outer)
                )
                | (
                    Some(BrimKind::Inner | BrimKind::OuterAndInner),
                    Some(BrimKind::Inner)
                )
                | (Some(BrimKind::OuterAndInner), Some(BrimKind::OuterAndInner))
        );
    }
    match (side(field), value.number(), reference.number()) {
        (Side::Either, _, _) => true,
        (Side::Fixed, _, _) => value == reference,
        (Side::Up, Some(value), Some(reference)) => value >= reference - EPS,
        (Side::Down, Some(value), Some(reference)) => value <= reference + EPS,
        // Nothing limited the field before, so any cap is a gain.
        (Side::Down, _, None) => reference.is_unset(),
        (Side::Up, None, None) => value == reference,
        // One side is left to the profile and the other is not: not shown to be safe.
        _ => false,
    }
}

/// The limits of a plate that the envelopes of some fields end at. The rule engine works
/// them out from the card, the rule data and the facts before it decides any field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    /// The highest level a request can reach.
    pub(crate) level_max: i64,
    /// The widest brim a change may ask for.
    pub(crate) brim_width_max: i64,
    /// The top of the bed temperature window, cut at the printer's and the filament's limit.
    pub(crate) bed_top: i64,
    /// The longest fan delay a change may ask for.
    pub(crate) fan_off_max: i64,
    /// The first layer acceleration of the sticking baseline: the top for a plan that
    /// leaves the acceleration to the installed profile.
    pub(crate) first_layer_accel_top: i64,
    /// The fewest wall loops the thin-wall rule and the proven baseline allow.
    pub(crate) wall_floor: i64,
    /// The rule that owns that floor: `MAT-1`, `PRV-1` when the proven baseline raised it,
    /// or `ENV-1` at the end of the hard range.
    pub(crate) wall_floor_owner: RuleId,
    /// The highest support threshold the material allows.
    pub(crate) threshold_max: i64,
}

/// A value that an allowed set does not hold, with the rule that keeps it out and the
/// catalogue sentence that says why.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Denied {
    pub(crate) value: Value,
    pub(crate) rule: RuleId,
    pub(crate) detail: &'static str,
}

/// An envelope with the owners of its bounds. The owners are not part of the stored plan;
/// they name the rule in the answer to a change.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Bounds {
    pub(crate) envelope: Envelope,
    low: RuleId,
    high: RuleId,
    denied: Vec<Denied>,
}

/// What became of a value somebody asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Judged {
    /// Inside the envelope: used as asked.
    Taken(Value),
    /// A number outside the envelope: held to the nearest bound, which `rule` owns.
    Held { value: Value, rule: RuleId },
    /// Not deliverable on this plate: nothing changes.
    Refused {
        answer: Answer,
        rule: RuleId,
        detail: &'static str,
    },
}

impl Bounds {
    /// A field nobody may move.
    pub(crate) fn locked(rule: RuleId) -> Self {
        Self {
            envelope: Envelope::Locked { rule },
            low: rule,
            high: rule,
            denied: Vec::new(),
        }
    }

    /// The allowed values of an enum or a flag, with the reason for each value left out.
    pub(crate) fn set(allowed: Vec<Value>, denied: Vec<Denied>) -> Self {
        Self {
            envelope: Envelope::Set { allowed },
            low: RuleId::Env1,
            high: RuleId::Env1,
            denied,
        }
    }

    /// The numbers from `low` to `high`, each with the rule that owns the bound. The range
    /// is cut at the hard range of the field and always holds `value`, the rules' own.
    fn range(field: Field, value: &Value, low: (f64, RuleId), high: (f64, RuleId)) -> Self {
        let Some((hard_min, hard_max)) = bounds::numeric_range(field) else {
            return Self::locked(low.1);
        };
        let at = value.number();
        let min = at
            .map_or(low.0, |at| low.0.min(at))
            .clamp(hard_min, hard_max);
        let max = at
            .map_or(high.0, |at| high.0.max(at))
            .clamp(hard_min, hard_max);
        let step = match bounds::spec(field).kind {
            Kind::Decimal { .. } => 0.01,
            _ => 1.0,
        };
        Self {
            envelope: Envelope::Range { min, max, step },
            low: low.1,
            high: high.1,
            denied: Vec::new(),
        }
    }

    /// Judges a value somebody asked for. The value has the type of the field and lies in
    /// its hard range: [`crate::propose::Changes`] takes nothing else.
    pub(crate) fn judge(&self, field: Field, asked: &Value) -> Judged {
        match &self.envelope {
            Envelope::Locked { rule } => Judged::Refused {
                answer: Answer::Locked,
                rule: *rule,
                detail: "locked",
            },
            Envelope::Set { allowed } => {
                if allowed.contains(asked) {
                    return Judged::Taken(*asked);
                }
                match self.denied.iter().find(|denied| denied.value == *asked) {
                    Some(denied) => Judged::Refused {
                        answer: Answer::NotAllowed,
                        rule: denied.rule,
                        detail: denied.detail,
                    },
                    // A value of another kind than the field's: no set can hold it.
                    None => Judged::Refused {
                        answer: Answer::Conflict,
                        rule: RuleId::Xf1,
                        detail: "conflict",
                    },
                }
            }
            Envelope::Range { min, max, .. } => {
                let Some(number) = asked.number() else {
                    return Judged::Refused {
                        answer: Answer::Conflict,
                        rule: RuleId::Xf1,
                        detail: "conflict",
                    };
                };
                let typed = |number: f64| match bounds::spec(field).kind {
                    Kind::Decimal { .. } => Value::decimal(number),
                    _ => Value::Whole(number.round() as i64),
                };
                if number < *min - EPS {
                    Judged::Held {
                        value: typed(*min),
                        rule: self.low,
                    }
                } else if number > *max + EPS {
                    Judged::Held {
                        value: typed(*max),
                        rule: self.high,
                    }
                } else {
                    Judged::Taken(typed(number))
                }
            }
        }
    }
}

impl Limits {
    /// The envelope of a numeric field whose rule value is `value`, set by `rule`. Enum and
    /// flag fields get their allowed sets from the rules themselves; every field that the
    /// agent or the person may not move is locked.
    pub(crate) fn bounds(&self, field: Field, value: &Value, rule: RuleId) -> Bounds {
        let spec = bounds::spec(field);
        let (Some((hard_min, hard_max)), true) =
            (bounds::numeric_range(field), spec.who.adjustable)
        else {
            return Bounds::locked(rule);
        };
        let hard_low = (hard_min, RuleId::Env1);
        let hard_high = (hard_max, RuleId::Env1);
        // The rules' own value as a bound; a field left to the profile has none.
        let own = value.number().map(|at| (at, rule));
        let (low, high) = match field {
            Field::Level => (own, Some((self.level_max as f64, RuleId::LvlAsk))),
            Field::BrimWidth => (own, Some((self.brim_width_max as f64, RuleId::Env1))),
            Field::FirstLayerBed => (own, Some((self.bed_top as f64, RuleId::Plt2))),
            Field::PartFanOffLayers => (own, Some((self.fan_off_max as f64, RuleId::Lvl2))),
            Field::WallLoops => {
                let floor = self.wall_floor as f64;
                let owner = if floor > hard_min {
                    self.wall_floor_owner
                } else {
                    RuleId::Env1
                };
                (Some((floor, owner)), Some(hard_high))
            }
            Field::InfillPct => (Some(hard_low), Some(hard_high)),
            Field::SupportsThreshold => (
                Some(hard_low),
                Some((self.threshold_max as f64, RuleId::Sup2)),
            ),
            // Left to the installed profile, the first layer acceleration is the baseline's:
            // a change starts there, not at the end of the hard range.
            Field::FirstLayerAccel => (
                Some(hard_low),
                Some(own.unwrap_or((self.first_layer_accel_top as f64, RuleId::Stk1))),
            ),
            // First layer speeds and every cap: down from the rules' value, or from the end
            // of the hard range when the rules set no cap.
            _ if matches!(side(field), Side::Down) => {
                (Some(hard_low), Some(own.unwrap_or(hard_high)))
            }
            _ => (None, None),
        };
        match (low, high) {
            (Some(low), Some(high)) => Bounds::range(field, value, low, high),
            // A field that must start at the rules' value and has none, or a field this
            // table does not know: it cannot be moved, which can never weaken a plan.
            _ => Bounds::locked(rule),
        }
    }
}

impl Envelope {
    /// Whether the envelope holds `value`. A value that is left to the installed profile is
    /// inside only when the rules left it there (`rule_value`).
    pub fn contains(&self, value: &Value, rule_value: &Value) -> bool {
        match self {
            Envelope::Locked { .. } => value == rule_value,
            Envelope::Set { allowed } => allowed.contains(value),
            Envelope::Range { min, max, .. } => match value.number() {
                Some(number) => number >= *min - EPS && number <= *max + EPS,
                None => value.is_unset() && rule_value.is_unset(),
            },
        }
    }

    /// Whether every value of the envelope may exist for `field`: inside its hard range,
    /// and of its type.
    pub fn in_hard_range(&self, field: Field) -> bool {
        match self {
            Envelope::Locked { .. } => true,
            Envelope::Set { allowed } => allowed
                .iter()
                .all(|value| !value.is_unset() && bounds::check(field, value).is_ok()),
            Envelope::Range { min, max, step } => {
                bounds::numeric_range(field).is_some_and(|(hard_min, hard_max)| {
                    min.is_finite()
                        && max.is_finite()
                        && *step > 0.0
                        && *min <= *max + EPS
                        && *min >= hard_min - EPS
                        && *max <= hard_max + EPS
                })
            }
        }
    }

    /// The corners of the envelope as values of `field`: both ends of a range, every value
    /// of a set, nothing for a locked field.
    pub fn corners(&self, field: Field) -> Vec<Value> {
        match self {
            Envelope::Locked { .. } => Vec::new(),
            Envelope::Set { allowed } => allowed.clone(),
            Envelope::Range { min, max, .. } => {
                let typed = |number: f64| match bounds::spec(field).kind {
                    Kind::Decimal { .. } => Value::decimal(number),
                    _ => Value::Whole(number.round() as i64),
                };
                let mut corners = vec![typed(*min)];
                if (*max - *min).abs() > EPS {
                    corners.push(typed(*max));
                }
                corners
            }
        }
    }
}

/// Records in a decision that a change was not applied, and why.
pub(crate) fn refuse(
    decision: &mut Decision,
    change: &Change,
    answer: Answer,
    rule: RuleId,
    detail: &'static str,
) {
    decision.asked = (change.value != decision.value).then_some(change.value);
    decision.answer = Some(Answered {
        by: change.by,
        answer,
        rule,
        detail,
    });
}

/// Applies a change to the decision the rules made for its field. The decision still holds
/// the rules' value; afterwards it holds the value of the plan and says who decided it.
pub(crate) fn apply(
    decision: &mut Decision,
    bounds: &Bounds,
    change: &Change,
    catalogue: &Catalogue,
) -> Result<(), PlanError> {
    let (value, held_by) = match bounds.judge(decision.field, &change.value) {
        Judged::Taken(value) => (value, None),
        Judged::Held { value, rule } => (value, Some(rule)),
        Judged::Refused {
            answer,
            rule,
            detail,
        } => {
            refuse(decision, change, answer, rule, detail);
            return Ok(());
        }
    };
    // The value passes the hard range once more when the settings are built.
    bounds::check(decision.field, &value)?;
    let who = P::Word(catalogue.word("asker", asker_token(change.by))?);
    if value != decision.rule_value {
        let (id, sentence) = match held_by {
            None => catalogue.decision("change.accepted", &[("who", who)])?,
            Some(rule) => catalogue.decision(
                "change.clamped",
                &[
                    ("who", who),
                    ("asked", P::Number(change.value.number().unwrap_or(0.0))),
                    ("value", P::Number(value.number().unwrap_or(0.0))),
                    ("rule", P::Word(rule.as_str())),
                ],
            )?,
        };
        decision.value = value;
        decision.source = change.by.source();
        decision.basis = Basis::Judgment;
        decision.sentence_id = id;
        decision.sentence = sentence;
    }
    // A value that was held names the rule that owns the bound, also when the bound is the
    // rules' own value and nothing moved.
    if let Some(rule) = held_by {
        decision.rule = rule;
    }
    decision.asked = (change.value != value).then_some(change.value);
    decision.answer = Some(Answered {
        by: change.by,
        answer: if held_by.is_some() {
            Answer::Clamped
        } else {
            Answer::Accepted
        },
        rule: held_by.unwrap_or(decision.rule_rule),
        detail: if held_by.is_some() {
            "clamped"
        } else {
            "accepted"
        },
    });
    Ok(())
}

/// The token of an asker in the catalogue's word table.
pub(crate) fn asker_token(asker: Asker) -> &'static str {
    match asker {
        Asker::Agent => "agent",
        Asker::Person => "person",
    }
}

/// A finished plan that breaks one of Hotend's own rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("`{field}` breaks rule {rule}: {what}")]
pub struct Violation {
    /// The rule that is broken.
    pub rule: RuleId,
    /// The field whose value breaks it.
    pub field: Field,
    /// What is wrong, in fixed words.
    pub what: &'static str,
    /// The catalogue sentence for the answer to a change that caused it.
    pub(crate) detail: &'static str,
}

fn violation(rule: RuleId, field: Field, what: &'static str) -> Violation {
    Violation {
        rule,
        field,
        what,
        detail: "conflict",
    }
}

impl RuleBook {
    /// Checks a plan against what must hold for every plan, with or without changes:
    ///
    /// - every value has the type and the hard range of its field, lies inside its
    ///   envelope, and every envelope lies inside the hard range;
    /// - no sticking field and no cap is on the weak side of the rules' value;
    /// - the cross-field rules that this crate can see: a first layer the
    ///   nozzle can print, bed temperatures inside the window and under the printer's limit
    ///   with the later layers never hotter than the first, one filament and no prime
    ///   tower, a brim kind that draws a brim exactly when the brim has a width, calibrated
    ///   fields equal to the gauge's, and wall loops at or above the thin-wall floor.
    ///
    /// Whether the brim fits on the bed needs the layout and is checked by the
    /// caller; whether every enum value can be written is already part of the
    /// allowed sets, which hold only values the slicer side has verified.
    pub fn verify(&self, inputs: &PlanInputs, plan: &PrintPlan) -> Result<(), Violation> {
        let Some(settings) = &plan.settings else {
            // A blocked plan delivers nothing, so there is nothing to break.
            return if plan.decisions.is_empty() && plan.blocked.is_some() {
                Ok(())
            } else {
                Err(violation(
                    RuleId::Xf1,
                    Field::PlateType,
                    "a plan without settings must be blocked and decide nothing",
                ))
            };
        };
        let Some(derived) = &plan.derived else {
            return Err(violation(
                RuleId::Xf1,
                Field::PlateType,
                "the plan does not say what its rules worked out",
            ));
        };
        if plan.decisions.len() != Field::ALL.len() {
            return Err(violation(
                RuleId::Xf1,
                Field::PlateType,
                "the plan does not decide every field",
            ));
        }
        for (decision, field) in plan.decisions.iter().zip(Field::ALL.iter().copied()) {
            let fail = |rule: RuleId, what: &'static str| Err(violation(rule, field, what));
            if decision.field != field || decision.value != settings.get(field) {
                return fail(RuleId::Xf1, "the decision and the settings disagree");
            }
            if bounds::check(field, &decision.value).is_err()
                || bounds::check(field, &decision.rule_value).is_err()
            {
                return fail(RuleId::Env1, "a value lies outside the hard range");
            }
            if !decision.envelope.in_hard_range(field) {
                return fail(RuleId::Env1, "the envelope lies outside the hard range");
            }
            if !decision
                .envelope
                .contains(&decision.value, &decision.rule_value)
            {
                return fail(RuleId::Env1, "the value lies outside its envelope");
            }
            // The rule value of the level is the level before any request, and that of
            // every other field is the rules' value at the plan's level. A higher level
            // never weakens a rule value, so this also holds against the plan nobody
            // changed.
            let guarded =
                bounds::spec(field).group == Group::Sticking || matches!(side(field), Side::Down);
            if guarded && !not_weaker(field, &decision.value, &decision.rule_value) {
                return fail(
                    RuleId::Spd1,
                    "the value is on the weak side of the rules' value",
                );
            }
        }

        let number = |field: Field| settings.get(field).number();
        // A first layer this nozzle can print.
        let nozzle = inputs.printer.nozzle_diameter_mm();
        let cross = &self.data().cross;
        if let Some(height) = number(Field::FirstLayerHeight) {
            let limit = (cross.first_layer_height_max_factor * nozzle)
                .min(inputs.printer.max_layer_height_mm());
            if height > limit + EPS {
                return Err(violation(
                    RuleId::Xf1,
                    Field::FirstLayerHeight,
                    "the first layer is higher than the nozzle can print",
                ));
            }
        }
        if let Some(width) = number(Field::FirstLayerLineWidth)
            && (width < cross.line_width_min_factor * nozzle - EPS
                || width > cross.line_width_max_factor * nozzle + EPS)
        {
            return Err(violation(
                RuleId::Xf1,
                Field::FirstLayerLineWidth,
                "the first layer line is not one the nozzle can print",
            ));
        }
        // Bed temperatures and PLT-2, against the window of the card itself, not the one the plan
        // states: inside the window, and at or below the printer's and the filament's limit.
        let window = self
            .cards()
            .card(inputs.facts.family())
            .zip(inputs.setup.plate_type())
            .and_then(|(card, plate)| card.on(plate))
            .and_then(|on| on.bed_values())
            .map(|bed| bed.window);
        let limit = inputs.printer.bed_limit_c();
        match (
            window,
            settings.get(Field::FirstLayerBed).whole(),
            settings.get(Field::LayersBed).whole(),
        ) {
            (Some([low, high]), Some(first), Some(other))
                if other <= first
                    && first <= high.min(limit)
                    && other >= low
                    && derived.bed_window_c == [low, high.min(limit)] => {}
            _ => {
                return Err(violation(
                    RuleId::Plt2,
                    Field::FirstLayerBed,
                    "a bed temperature lies outside the window, above the printer's limit, or rises after the first layer",
                ));
            }
        }
        // One filament and no prime tower.
        if settings.get(Field::PrimeTower) != Value::Flag(false) || settings.filaments().len() != 1
        {
            return Err(violation(
                RuleId::Pt1,
                Field::PrimeTower,
                "a plate has one filament and no prime tower",
            ));
        }
        // The brim is one thing with two fields: a kind that draws nothing has no width,
        // and a width needs a kind that draws it.
        let drawn = number(Field::BrimWidth).is_some_and(|width| width > 0.0);
        let kind = settings.get(Field::BrimKind);
        if (kind != Value::Token(BrimKind::None.as_str())) != drawn {
            return Err(Violation {
                rule: RuleId::Xf1,
                field: Field::BrimKind,
                what: "the brim kind and the brim width disagree",
                detail: if drawn {
                    "conflict"
                } else {
                    "conflict.brim_needs_width"
                },
            });
        }
        // Calibrated fields equal the gauge's.
        if let (true, Some(signature)) = (derived.calibrated, &inputs.calibration) {
            let parts = signature.parts();
            for (field, recorded) in [
                (Field::ElephantFoot, parts.elephant_foot_mm),
                (Field::FirstLayerHeight, parts.first_layer_height_mm),
                (Field::FirstLayerLineWidth, parts.first_layer_line_width_mm),
            ] {
                if settings.get(field) != Value::decimal(recorded) {
                    return Err(violation(
                        RuleId::Sig1,
                        field,
                        "a calibrated field differs from what the fit gauge was printed with",
                    ));
                }
            }
        }
        // MAT-1.
        if number(Field::WallLoops).is_none_or(|loops| loops < derived.wall_loops_floor as f64) {
            return Err(violation(
                RuleId::Mat1,
                Field::WallLoops,
                "fewer wall loops than thin walls need",
            ));
        }
        // PRV-1: nothing of the plan is less careful than the proven baseline, held only to
        // what the plate allows (the window, the widest brim, the longest fan delay).
        if let Some(baseline) = inputs.applicable_baseline() {
            for field in Baseline::FIELDS.iter().copied() {
                let Some(floor) =
                    proven_floor(baseline, field, derived, self.data().level.fan_off_layers)
                else {
                    continue;
                };
                if !proven_kept(field, &settings.get(field), &floor) {
                    return Err(violation(
                        RuleId::Prv1,
                        field,
                        "a value is less careful than the proven baseline",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The value of the proven baseline for `field` as a floor of this plate: held to the top of
/// the bed window (cut at the printer's limit), the widest brim and the longest fan delay a
/// plan may have. The bed after the first layer is held to the first layer's floor as well.
pub(crate) fn proven_floor(
    baseline: &Baseline,
    field: Field,
    derived: &crate::model::Derived,
    fan_off_max: i64,
) -> Option<Value> {
    let value = baseline.get(field)?;
    let whole = |value: &Value| value.whole();
    let top = match field {
        Field::BrimWidth => Some(derived.brim_width_max_mm),
        Field::FirstLayerBed => Some(derived.bed_window_c[1]),
        Field::LayersBed => {
            let first = baseline
                .get(Field::FirstLayerBed)
                .as_ref()
                .and_then(whole)?;
            Some(derived.bed_window_c[1].min(first))
        }
        Field::PartFanOffLayers => Some(fan_off_max),
        _ => None,
    };
    Some(match (whole(&value), top) {
        (Some(number), Some(top)) => Value::Whole(number.min(top)),
        _ => value,
    })
}

/// Whether `value` is at least as careful as the proven `floor` of `field`. The wall loops
/// are compared upwards: more walls hold a thin part better.
pub(crate) fn proven_kept(field: Field, value: &Value, floor: &Value) -> bool {
    if field == Field::WallLoops {
        return match (value.number(), floor.number()) {
            (Some(value), Some(floor)) => value >= floor - EPS,
            _ => false,
        };
    }
    not_weaker(field, value, floor)
}
