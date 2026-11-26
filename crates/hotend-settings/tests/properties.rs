//! Envelopes and proposals: the properties that need a change, and the adversarial cases
//! that this crate can answer on its own (docs/PRINT_SETTINGS.md).
//!
//! The plates come from the deterministic generator of `common`, so every run sees the same
//! cases. Each property states what it checks in its own words: the checks here do not call
//! the library's `verify` alone, they recompute the invariant from the plan.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::{
    Lcg, block, book, box_with_bore, calm, changes, corpus, everything, foot, gauge,
    gauge_signature, generated, inputs, lid, plan, post, sleeve, strip_ring, value, whole,
    with_regions,
};
use hotend_settings::bounds::{self, Group, Kind};
use hotend_settings::envelope::{self, Side};
use hotend_settings::explain::ANSWERS;
use hotend_settings::facts::{FitAtBed, FitSide, InnerLoop, OverhangRegion, PieceFacts};
use hotend_settings::inputs::{Asked, Asker, History, PlanInputs};
use hotend_settings::model::{
    Answer, Envelope, Family, Field, Intent, PlateType, PrintPlan, RuleId, Source, Value,
};
use hotend_settings::propose::{
    ChangeError, Invalid, ItemOutcome, Note, Outcome, ProposedChange, ProposedValue,
};
use hotend_settings::schema::{MAX_CHANGES, MAX_REASON_CHARS, MAX_SUMMARY_CHARS};
use hotend_settings::{Changes, Proposal, ProposalOutcome, RuleBook};
use serde_json::json;

// ---- Helpers ---------------------------------------------------------------------------

fn number(value: f64) -> ProposedValue {
    ProposedValue::Number(value)
}

fn text(value: &str) -> ProposedValue {
    ProposedValue::Text(value.to_owned())
}

fn change(field: &str, value: ProposedValue) -> ProposedChange {
    ProposedChange {
        field: field.to_owned(),
        value,
        reason: "because of the facts of this plate".to_owned(),
    }
}

fn proposal(by: Asker, changes: Vec<ProposedChange>) -> Proposal {
    Proposal {
        by,
        plate: None,
        changes,
        summary: "a summary".to_owned(),
    }
}

fn propose(inputs: &PlanInputs, by: Asker, changes: Vec<ProposedChange>) -> ProposalOutcome {
    book()
        .propose(inputs, &proposal(by, changes))
        .expect("a proposal never fails for a reason of its own")
}

/// One change by the agent, and its answer.
fn ask(inputs: &PlanInputs, field: Field, value: ProposedValue) -> (ProposalOutcome, ItemOutcome) {
    let outcome = propose(inputs, Asker::Agent, vec![change(field.as_str(), value)]);
    let item = outcome.items[0].clone();
    (outcome, item)
}

fn bytes(plan: &PrintPlan) -> Vec<u8> {
    serde_json::to_vec(plan).unwrap()
}

/// A value as a proposal states it.
fn proposed(value: Value) -> ProposedValue {
    match value {
        Value::Whole(whole) => number(whole as f64),
        Value::Decimal(decimal) => number(decimal),
        Value::Token(token) => text(token),
        Value::Flag(flag) => ProposedValue::Flag(flag),
        other => panic!("{other:?} is never proposed"),
    }
}

/// The value as it takes effect: the recorded baseline where the plan leaves a sticking
/// field to the installed standard preset.
fn effective(plan: &PrintPlan, field: Field) -> Value {
    let baseline = &book().data().baseline;
    match (field, value(plan, field)) {
        (Field::ElephantFoot, Value::Unset) => Value::decimal(baseline.elephant_foot_mm),
        (Field::FirstLayerHeight, Value::Unset) => Value::decimal(baseline.first_layer_height_mm),
        (Field::FirstLayerLineWidth, Value::Unset) => {
            Value::decimal(baseline.first_layer_line_width_mm)
        }
        (Field::FirstLayerAccel, Value::Unset) => Value::Whole(baseline.first_layer_accel_mm_s2),
        (_, other) => other,
    }
}

/// The safe side of each sticking field, written out here once more so that the test does
/// not take the library's word for it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Safe {
    Higher,
    Lower,
    Same,
    Brim,
}

fn safe_side(field: Field) -> Safe {
    match field {
        Field::Level
        | Field::BrimWidth
        | Field::FirstLayerHeight
        | Field::FirstLayerBed
        | Field::LayersBed
        | Field::PartFanOffLayers => Safe::Higher,
        Field::BrimGap
        | Field::FirstLayerWallSpeed
        | Field::FirstLayerInfillSpeed
        | Field::FirstLayerAccel
        | Field::AuxFanFirstLayers
        | Field::AuxFanMax => Safe::Lower,
        Field::SkirtLoops
        | Field::ElephantFoot
        | Field::FirstLayerLineWidth
        | Field::FirstLayerNozzle
        | Field::LayersNozzle => Safe::Same,
        Field::BrimKind => Safe::Brim,
        other => panic!("{other} is not a sticking field"),
    }
}

/// Invariant 3: no sticking field of `plan` is on the weak side of the rule plan.
fn assert_sticks_as_well(rule_plan: &PrintPlan, plan: &PrintPlan, context: &str) {
    for field in bounds::sticking_fields() {
        let (now, rules) = (effective(plan, field), effective(rule_plan, field));
        let holds = match (safe_side(field), now.number(), rules.number()) {
            (Safe::Higher, Some(now), Some(rules)) => now >= rules - 1e-9,
            (Safe::Lower, Some(now), Some(rules)) => now <= rules + 1e-9,
            // The rules set no cap, so any cap is a gain.
            (Safe::Lower, _, None) => rules.is_unset(),
            (Safe::Same, _, _) => now == rules,
            (Safe::Brim, _, _) => match (rules.token(), now.token()) {
                (Some("none"), Some(_)) => true,
                (Some("outer"), Some("outer" | "outer_and_inner")) => true,
                (rules, now) => rules == now,
            },
            _ => false,
        };
        assert!(
            holds,
            "{context}: {field} is {now:?}, weaker than the rule plan's {rules:?}"
        );
    }
}

/// Invariant 2: every value in its envelope, every envelope in its hard range, the rules'
/// own value inside the envelope, and caps never above the rules' cap.
fn assert_inside(inputs: &PlanInputs, plan: &PrintPlan, context: &str) {
    let settings = plan.settings.as_ref().expect("the plan is not blocked");
    assert_eq!(plan.decisions.len(), Field::ALL.len(), "{context}");
    for decision in &plan.decisions {
        let field = decision.field;
        assert_eq!(decision.value, settings.get(field), "{context}: {field}");
        assert_eq!(
            bounds::check(field, &decision.value),
            Ok(()),
            "{context}: {field}"
        );
        match &decision.envelope {
            Envelope::Range { min, max, step } => {
                let (hard_min, hard_max) = bounds::numeric_range(field)
                    .unwrap_or_else(|| panic!("{context}: {field} has a range but no numbers"));
                assert!(
                    hard_min <= *min && min <= max && *max <= hard_max && *step > 0.0,
                    "{context}: {field} has the envelope {min} to {max} outside {hard_min} to {hard_max}"
                );
                for (what, number) in [
                    ("value", decision.value.number()),
                    ("rule value", decision.rule_value.number()),
                ] {
                    match number {
                        Some(number) => assert!(
                            *min - 1e-9 <= number && number <= *max + 1e-9,
                            "{context}: the {what} {number} of {field} is outside {min} to {max}"
                        ),
                        // Left to the profile: only where the rules left it there.
                        None => assert!(decision.rule_value.is_unset(), "{context}: {field}"),
                    }
                }
                // Whole fields move in whole steps, decimals in hundredths.
                let whole_kind = matches!(bounds::spec(field).kind, Kind::Whole { .. });
                assert_eq!(
                    *step,
                    if whole_kind { 1.0 } else { 0.01 },
                    "{context}: {field}"
                );
            }
            Envelope::Set { allowed } => {
                assert!(
                    allowed.contains(&decision.value),
                    "{context}: {field} is {:?}, not one of {allowed:?}",
                    decision.value
                );
                for value in allowed {
                    assert_eq!(bounds::check(field, value), Ok(()), "{context}: {field}");
                    assert!(!value.is_unset(), "{context}: {field}");
                }
            }
            Envelope::Locked { .. } => {
                assert_eq!(decision.value, decision.rule_value, "{context}: {field}");
                assert_eq!(decision.asked.is_some(), decision.answer.is_some());
            }
        }
        // A field nobody may move is locked, and a locked field never follows a change.
        if !bounds::spec(field).who.adjustable {
            assert!(
                matches!(decision.envelope, Envelope::Locked { .. }),
                "{context}: {field} may not be moved but has an envelope"
            );
            assert_eq!(decision.answer, None, "{context}: {field}");
        }
        // A speed, an acceleration or a flow is a cap. It is never above the rules'.
        if envelope::side(field) == Side::Down {
            match (decision.value.number(), decision.rule_value.number()) {
                (Some(now), Some(rules)) => {
                    assert!(now <= rules + 1e-9, "{context}: {field} was raised")
                }
                (None, Some(_)) => panic!("{context}: the cap {field} was taken away"),
                _ => {}
            }
        }
        // An answer is recorded exactly for the fields somebody asked about.
        let asked = match field {
            Field::Level => inputs.level.is_some(),
            Field::Intent => inputs.intent.is_some(),
            other => inputs.changes.get(other).is_some(),
        };
        assert_eq!(decision.answer.is_some(), asked, "{context}: {field}");
        // Who decided: a change makes its asker the source exactly where it moved the value
        // away from the rules' value. (A field that follows an asked level keeps naming
        // that asker, as it did before any change.)
        if let Some(change) = inputs.changes.get(field) {
            if decision.value == decision.rule_value {
                assert_eq!(decision.source, decision.rule_source, "{context}: {field}");
                // Held to the rules' value, the decision names the rule that owns the bound.
                if decision
                    .answer
                    .is_some_and(|answered| answered.answer == Answer::Accepted)
                {
                    assert_eq!(decision.rule, decision.rule_rule, "{context}: {field}");
                }
            } else if field == Field::BrimKind && inputs.changes.get(Field::BrimWidth).is_some() {
                // The kind follows the width: whoever asked for the brim decided that one
                // is drawn, and a kind asked for on top of it is its asker's.
                let width = inputs
                    .changes
                    .get(Field::BrimWidth)
                    .map(|width| width.by.source());
                assert!(
                    decision.source == change.by.source() || Some(decision.source) == width,
                    "{context}: {field}"
                );
            } else {
                assert_eq!(decision.source, change.by.source(), "{context}: {field}");
            }
        }
        assert!(
            !decision.sentence.is_empty() && !decision.sentence.contains(['{', '}', '\n']),
            "{context}: {field}"
        );
    }
    // The library's own check agrees, and covers the cross-field rules.
    assert_eq!(book().verify(inputs, plan), Ok(()), "{context}");
    // Recomputed here: bed pair, prime tower, brim kind and width, wall loops.
    let derived = plan.derived.as_ref().unwrap();
    let (first, other) = (
        whole(plan, Field::FirstLayerBed),
        whole(plan, Field::LayersBed),
    );
    assert!(
        other <= first
            && first <= inputs.printer.bed_limit_c()
            && derived.bed_window_c[0] <= other
            && first <= derived.bed_window_c[1],
        "{context}: bed {first}/{other}"
    );
    assert_eq!(
        value(plan, Field::PrimeTower),
        Value::Flag(false),
        "{context}"
    );
    assert_eq!(
        value(plan, Field::BrimKind) == Value::Token("none"),
        whole(plan, Field::BrimWidth) == 0,
        "{context}: the brim kind and the brim width disagree"
    );
    assert!(
        whole(plan, Field::WallLoops) >= derived.wall_loops_floor,
        "{context}"
    );
    assert!(
        whole(plan, Field::BrimWidth) <= derived.brim_width_max_mm,
        "{context}"
    );
    if let Some(height) = value(plan, Field::FirstLayerHeight).number() {
        assert!(
            height <= 0.75 * inputs.printer.nozzle_diameter_mm() + 1e-9,
            "{context}"
        );
        assert!(
            height <= inputs.printer.max_layer_height_mm() + 1e-9,
            "{context}"
        );
    }
}

/// The answer to a proposal says what the plan does: the plan is the plan of the returned
/// inputs, an accepted or clamped item is in effect, a refused one is not.
fn assert_answer_is_true(before: &PlanInputs, outcome: &ProposalOutcome, context: &str) {
    let again = plan(&outcome.inputs);
    assert_eq!(
        bytes(&again),
        bytes(&outcome.plan),
        "{context}: not the plan of its inputs"
    );
    if outcome.refused.is_some() {
        assert_eq!(
            outcome.inputs, *before,
            "{context}: a refused proposal changed something"
        );
        assert!(
            outcome
                .items
                .iter()
                .all(|item| item.outcome == Outcome::Refused)
        );
    }
    for item in &outcome.items {
        let known = ANSWERS.iter().any(|(id, _)| *id == item.detail);
        assert!(known, "{context}: unknown answer {}", item.detail);
        assert!(
            !item.message.is_empty() && !item.message.contains(['{', '}', '\n']),
            "{context}: {:?}",
            item.message
        );
        let Some(field) = item.field else {
            assert_eq!(item.outcome, Outcome::Refused, "{context}");
            assert_eq!((item.asked, item.applied), (None, None), "{context}");
            continue;
        };
        match item.outcome {
            Outcome::Accepted => {
                assert_eq!(item.applied, item.asked, "{context}: {field}");
                assert_eq!(outcome.plan.value(field), item.asked, "{context}: {field}");
                assert_eq!(item.code, "accepted");
            }
            Outcome::Clamped => {
                assert_ne!(item.applied, item.asked, "{context}: {field}");
                assert_eq!(
                    outcome.plan.value(field),
                    item.applied,
                    "{context}: {field}"
                );
                assert!(item.rule.is_some(), "{context}: {field} names no rule");
                assert_eq!(item.code, "clamped");
            }
            Outcome::Refused => {
                assert_eq!(item.applied, None, "{context}: {field}");
                // The field keeps what it had: the plan before, unless another change of
                // the same proposal moved the field's default.
                if outcome.items.len() == 1 {
                    assert_eq!(outcome.inputs, *before, "{context}: {field}");
                }
            }
        }
    }
}

/// A value of the field's type inside its hard range: what passes the schema.
fn random_value(lcg: &mut Lcg, field: Field) -> ProposedValue {
    match bounds::spec(field).kind {
        Kind::Whole { min, max } => {
            number((min + ((max - min + 1) as f64 * lcg.next()) as i64).min(max) as f64)
        }
        Kind::Decimal { min, max } => {
            number(((min + (max - min) * lcg.next()) * 100.0).round() / 100.0)
        }
        Kind::Tokens(tokens) => text(lcg.pick(tokens)),
        Kind::Flag => ProposedValue::Flag(lcg.next() < 0.5),
        Kind::Colour => text("#102030"),
        Kind::Slot => text("3"),
    }
}

/// Up to `most` changes of distinct fields, mostly fields that may be moved.
fn random_changes(lcg: &mut Lcg, most: usize) -> Vec<ProposedChange> {
    let adjustable: Vec<Field> = bounds::adjustable_fields().collect();
    let count = 1 + (lcg.next() * most as f64) as usize % most;
    let mut fields = BTreeSet::new();
    while fields.len() < count {
        fields.insert(if lcg.next() < 0.9 {
            lcg.pick(&adjustable)
        } else {
            lcg.pick(Field::ALL)
        });
    }
    // In a shuffled order: the order written must not matter.
    let mut fields: Vec<Field> = fields.into_iter().collect();
    for at in (1..fields.len()).rev() {
        fields.swap(at, (lcg.next() * (at + 1) as f64) as usize % (at + 1));
    }
    fields
        .into_iter()
        .map(|field| change(field.as_str(), random_value(lcg, field)))
        .collect()
}

fn asker(lcg: &mut Lcg) -> Asker {
    if lcg.next() < 0.5 {
        Asker::Agent
    } else {
        Asker::Person
    }
}

/// Generated plates that have a plan, with their rule plan (nothing asked for).
fn plates(count: usize) -> Vec<(PlanInputs, PrintPlan)> {
    generated(count)
        .into_iter()
        .map(|inputs| inputs.reset())
        .filter_map(|inputs| {
            let rule_plan = plan(&inputs);
            rule_plan.settings.is_some().then_some((inputs, rule_plan))
        })
        .collect()
}

// ---- Invariants over random plates and random proposals ---------------------------------

#[test]
fn every_value_stays_in_its_envelope_and_no_sticking_field_weakens_under_any_proposals() {
    let mut lcg = Lcg(0x00C0_FFEE_2026);
    let (mut accepted, mut clamped, mut refused) = (0, 0, 0);
    for (number, (start, rule_plan)) in plates(160).into_iter().enumerate() {
        assert_inside(&start, &rule_plan, &format!("plate {number}, rule plan"));
        let mut inputs = start;
        for round in 0..6 {
            let context = format!("plate {number}, round {round}");
            let by = asker(&mut lcg);
            let outcome = propose(&inputs, by, random_changes(&mut lcg, 4));
            assert_eq!(outcome.refused, None, "{context}");
            assert_answer_is_true(&inputs, &outcome, &context);
            assert_inside(&outcome.inputs, &outcome.plan, &context);
            assert_sticks_as_well(&rule_plan, &outcome.plan, &context);
            // What was sent back as dropped is no longer in the set.
            for item in &outcome.dropped {
                assert_eq!(item.outcome, Outcome::Refused, "{context}");
                let field = item.field.unwrap();
                assert!(
                    outcome.inputs.changes.get(field).is_none()
                        || outcome
                            .plan
                            .decision(field)
                            .unwrap()
                            .answer
                            .is_some_and(|a| a.answer.applied()),
                    "{context}: {field}"
                );
            }
            // Nothing in the set is waiting to come back: every change is in effect.
            for decision in &outcome.plan.decisions {
                if let Some(answered) = decision.answer {
                    assert!(answered.answer.applied(), "{context}: {}", decision.field);
                }
            }
            for item in &outcome.items {
                match item.outcome {
                    Outcome::Accepted => accepted += 1,
                    Outcome::Clamped => clamped += 1,
                    Outcome::Refused => refused += 1,
                }
            }
            inputs = outcome.inputs;
        }
        // Reset gives the rule plan again, byte for byte.
        assert_eq!(bytes(&plan(&inputs.reset())), bytes(&rule_plan));
    }
    // The run met all three answers often enough to mean something.
    assert!(
        accepted > 300 && clamped > 100 && refused > 100,
        "{accepted} accepted, {clamped} clamped, {refused} refused"
    );
}

#[test]
fn a_set_of_changes_built_by_hand_cannot_leave_an_envelope() {
    // The core reads accepted changes back from a file. Whatever such a set holds, each
    // change is judged again when the plan is computed.
    let mut lcg = Lcg(0x00BA_D5E7);
    let adjustable: Vec<Field> = bounds::adjustable_fields()
        .filter(|field| !matches!(field, Field::Level | Field::Intent))
        .collect();
    for (number, (mut inputs, rule_plan)) in plates(120).into_iter().enumerate() {
        let mut set = Changes::none();
        for _ in 0..(1 + (lcg.next() * 12.0) as usize) {
            let field = lcg.pick(&adjustable);
            let value = match bounds::spec(field).kind {
                Kind::Whole { min, max } => {
                    Value::Whole((min + ((max - min + 1) as f64 * lcg.next()) as i64).min(max))
                }
                Kind::Decimal { min, max } => Value::decimal(min + (max - min) * lcg.next()),
                Kind::Tokens(tokens) => Value::Token(lcg.pick(tokens)),
                Kind::Flag => Value::Flag(lcg.next() < 0.5),
                other => panic!("{other:?} is not adjustable"),
            };
            set = set.with(field, value, asker(&mut lcg)).unwrap();
        }
        inputs.changes = set;
        inputs.level = (lcg.next() < 0.3).then(|| Asked {
            value: (lcg.next() * 4.0) as u8 % 4,
            by: Asker::Agent,
        });
        let context = format!("plate {number}");
        let plan = plan(&inputs);
        assert_inside(&inputs, &plan, &context);
        assert_sticks_as_well(&rule_plan, &plan, &context);
        // The same inputs give the same bytes, also from a book loaded afresh.
        assert_eq!(bytes(&plan), bytes(&common::plan(&inputs.clone())));
    }
}

#[test]
fn the_plan_with_changes_is_a_pure_function_of_its_inputs() {
    let fresh = RuleBook::from_toml(
        include_str!("../data/print-rules.toml"),
        include_str!("../data/print-cards.toml"),
        include_str!("../data/intents.toml"),
        include_str!("../data/sentences.toml"),
    )
    .unwrap();
    let mut lcg = Lcg(0x9E37_79B9);
    let mut hashes = BTreeSet::new();
    let all = plates(60);
    let count = all.len();
    for (inputs, _) in all {
        let by = asker(&mut lcg);
        let items = random_changes(&mut lcg, 5);
        let first = propose(&inputs, by, items.clone());
        let again = propose(&inputs, by, items.clone());
        let other_book = fresh.propose(&inputs, &proposal(by, items)).unwrap();
        // The whole answer, the notes among it, is the same every time.
        let whole_answer = serde_json::to_vec(&first).unwrap();
        assert_eq!(whole_answer, serde_json::to_vec(&again).unwrap());
        assert_eq!(whole_answer, serde_json::to_vec(&other_book).unwrap());
        assert_eq!(
            first.plan.sha256().unwrap(),
            fresh.plan(&first.inputs).unwrap().sha256().unwrap()
        );
        hashes.insert(first.plan.sha256().unwrap());
    }
    assert_eq!(hashes.len(), count, "different inputs give different plans");
    // A change is part of what the plan is a function of.
    let wider = propose(
        &lid(),
        Asker::Person,
        vec![change("adhesion.brim_width_mm", number(4.0))],
    );
    assert_ne!(wider.plan.sha256().unwrap(), plan(&lid()).sha256().unwrap());
}

#[test]
fn a_proposal_applied_twice_changes_nothing_more() {
    let mut lcg = Lcg(0x0077_17E5);
    for (number, (inputs, _)) in plates(120).into_iter().enumerate() {
        let by = asker(&mut lcg);
        let items = random_changes(&mut lcg, 6);
        let once = propose(&inputs, by, items.clone());
        let twice = propose(&once.inputs, by, items);
        assert_eq!(twice.inputs, once.inputs, "plate {number}");
        assert_eq!(bytes(&twice.plan), bytes(&once.plan), "plate {number}");
        // Each item gets the same answer the second time.
        let answers = |outcome: &ProposalOutcome| -> Vec<(Option<Field>, Outcome, &'static str)> {
            outcome
                .items
                .iter()
                .map(|item| (item.field, item.outcome, item.detail))
                .collect()
        };
        assert_eq!(answers(&twice), answers(&once), "plate {number}");
        assert!(twice.dropped.is_empty(), "plate {number}");
    }
}

#[test]
fn the_order_of_the_items_of_a_proposal_does_not_change_the_result() {
    let mut lcg = Lcg(0x0DE5_0DE5);
    for (number, (inputs, _)) in plates(120).into_iter().enumerate() {
        let by = asker(&mut lcg);
        // Some plates start with changes already accepted.
        let inputs = if lcg.next() < 0.5 {
            propose(&inputs, by, random_changes(&mut lcg, 4)).inputs
        } else {
            inputs
        };
        let items = random_changes(&mut lcg, 8);
        let mut reversed = items.clone();
        reversed.reverse();
        let mut rotated = items.clone();
        rotated.rotate_left(items.len() / 2);
        let reference = propose(&inputs, by, items);
        let by_field = |outcome: &ProposalOutcome| -> Vec<ItemOutcome> {
            let mut answers = outcome.items.clone();
            answers.sort_by_key(|item| item.field);
            answers
        };
        for other in [reversed, rotated] {
            let outcome = propose(&inputs, by, other);
            assert_eq!(outcome.inputs, reference.inputs, "plate {number}");
            assert_eq!(
                bytes(&outcome.plan),
                bytes(&reference.plan),
                "plate {number}"
            );
            assert_eq!(by_field(&outcome), by_field(&reference), "plate {number}");
            assert_eq!(outcome.dropped, reference.dropped, "plate {number}");
        }
    }
}

#[test]
fn proposals_in_any_order_give_the_same_plan_or_a_refusal_that_says_so() {
    // Three proposals for three different fields, one after the other, in every order.
    // Either every order ends in the same plan, byte for byte, or the orders that differ
    // answered a change with a refusal or dropped an earlier change, and said so.
    let mut lcg = Lcg(0x0BAD_C0DE_D00D);
    let (mut same, mut said) = (0, 0);
    for (number, (inputs, rule_plan)) in plates(110).into_iter().enumerate() {
        let by = asker(&mut lcg);
        let items = loop {
            let items = random_changes(&mut lcg, 3);
            if items.len() == 3 {
                break items;
            }
        };
        let orders: [[usize; 3]; 6] = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        let runs: Vec<(ProposalOutcome, bool)> = orders
            .iter()
            .map(|order| {
                let mut current = inputs.clone();
                let mut refusal = false;
                let mut last = None;
                for at in order {
                    let outcome = propose(&current, by, vec![items[*at].clone()]);
                    refusal |=
                        outcome.items[0].outcome == Outcome::Refused || !outcome.dropped.is_empty();
                    current = outcome.inputs.clone();
                    last = Some(outcome);
                }
                (last.unwrap(), refusal)
            })
            .collect();
        for (outcome, _) in &runs {
            assert_sticks_as_well(&rule_plan, &outcome.plan, &format!("plate {number}"));
        }
        let clean: Vec<&ProposalOutcome> = runs
            .iter()
            .filter(|(_, refusal)| !refusal)
            .map(|(outcome, _)| outcome)
            .collect();
        // Every order without a refusal ends in the same accepted set and the same plan.
        for outcome in &clean {
            assert_eq!(outcome.inputs, clean[0].inputs, "plate {number}");
            assert_eq!(
                bytes(&outcome.plan),
                bytes(&clean[0].plan),
                "plate {number}"
            );
        }
        if clean.len() == runs.len() {
            same += 1;
        } else {
            said += 1;
        }
    }
    assert!(
        same > 40 && said > 5,
        "{same} plates without a refusal, {said} with one"
    );
}

#[test]
fn every_corner_of_every_envelope_gives_a_valid_plan() {
    let named = [gauge(), sleeve(), post(), box_with_bore(true), lid()];
    let all: Vec<(PlanInputs, PrintPlan)> = named
        .into_iter()
        .map(|inputs| {
            let rule_plan = plan(&inputs.reset());
            (inputs, rule_plan)
        })
        .chain(plates(45))
        .collect();
    let mut lcg = Lcg(0x00C0_4E45);
    let mut corners = 0;
    for (index, (inputs, rule_plan)) in all.into_iter().enumerate() {
        let start = plan(&inputs);
        // Each corner on its own is delivered exactly.
        for decision in &start.decisions {
            let field = decision.field;
            for corner in decision.envelope.corners(field) {
                let context = format!("plate {index}, {field} = {corner:?}");
                let (outcome, item) = ask(&inputs, field, proposed(corner));
                corners += 1;
                // The one tie between two fields: a brim kind draws nothing without a
                // width, so on a plate without a brim it is asked for together with one.
                let kind_alone = field == Field::BrimKind
                    && corner != Value::Token("none")
                    && whole(&start, Field::BrimWidth) == 0;
                if kind_alone {
                    assert_eq!(
                        (item.outcome, item.code),
                        (Outcome::Refused, "conflict"),
                        "{context}"
                    );
                    assert_eq!(bytes(&outcome.plan), bytes(&start), "{context}");
                    let both = propose(
                        &inputs,
                        Asker::Agent,
                        vec![
                            change(field.as_str(), proposed(corner)),
                            change("adhesion.brim_width_mm", number(1.0)),
                        ],
                    );
                    // An inner brim needs more room the wider the brim is; with 1 mm of
                    // width every kind the plate offered without a brim is delivered.
                    if both.items[0].outcome == Outcome::Accepted {
                        assert_eq!(both.plan.value(field), Some(corner), "{context}");
                    } else {
                        assert_eq!(
                            both.items[0].detail, "not_allowed.brim_inner_room",
                            "{context}"
                        );
                    }
                    assert_inside(&both.inputs, &both.plan, &context);
                } else {
                    assert_eq!(item.outcome, Outcome::Accepted, "{context}: {}", item.code);
                    assert_eq!(outcome.plan.value(field), Some(corner), "{context}");
                }
                assert_answer_is_true(&inputs, &outcome, &context);
                assert_inside(&outcome.inputs, &outcome.plan, &context);
                assert_sticks_as_well(&rule_plan, &outcome.plan, &context);
            }
        }
        // All low corners at once, all high corners at once, and random corners at once.
        for pick in 0..5 {
            let context = format!("plate {index}, corner set {pick}");
            let items: Vec<ProposedChange> = start
                .decisions
                .iter()
                .filter_map(|decision| {
                    let corners = decision.envelope.corners(decision.field);
                    let corner = match pick {
                        0 => corners.first().copied(),
                        1 => corners.last().copied(),
                        _ if corners.is_empty() => None,
                        _ => Some(lcg.pick(&corners)),
                    }?;
                    Some(change(decision.field.as_str(), proposed(corner)))
                })
                .collect();
            let mut current = inputs.clone();
            for part in items.chunks(MAX_CHANGES) {
                let outcome = propose(&current, Asker::Person, part.to_vec());
                assert_eq!(outcome.refused, None, "{context}");
                assert_answer_is_true(&current, &outcome, &context);
                assert_inside(&outcome.inputs, &outcome.plan, &context);
                assert_sticks_as_well(&rule_plan, &outcome.plan, &context);
                current = outcome.inputs;
            }
        }
    }
    assert!(corners > 1500, "only {corners} corners were tried");
}

#[test]
fn the_envelopes_follow_the_documented_tables() {
    // On the measured gauge: PLA on the textured plate at level 1.
    let plan = plan(&gauge());
    let range = |field: Field| match plan.decision(field).unwrap().envelope {
        Envelope::Range { min, max, step } => (min, max, step),
        ref other => panic!("{field} has no range: {other:?}"),
    };
    assert_eq!(range(Field::Level), (1.0, 2.0, 1.0)); // rule value to 2
    assert_eq!(range(Field::BrimWidth), (5.0, 10.0, 1.0)); // rule value to 10
    assert_eq!(range(Field::FirstLayerWallSpeed), (10.0, 20.0, 1.0)); // 10 to the rule value
    assert_eq!(range(Field::FirstLayerInfillSpeed), (15.0, 30.0, 1.0));
    assert_eq!(range(Field::FirstLayerAccel), (250.0, 500.0, 1.0));
    assert_eq!(range(Field::FirstLayerBed), (65.0, 65.0, 1.0)); // rule value to the window's top
    assert_eq!(range(Field::PartFanOffLayers), (3.0, 5.0, 1.0)); // rule value to 5
    assert_eq!(range(Field::AuxFanMax), (0.0, 30.0, 1.0)); // 0 to the rule value
    assert_eq!(range(Field::WallLoops), (3.0, 6.0, 1.0)); // the thin-wall floor to 6
    assert_eq!(range(Field::InfillPct), (5.0, 50.0, 1.0));
    assert_eq!(range(Field::SupportsThreshold), (20.0, 45.0, 1.0));
    assert_eq!(range(Field::OuterWallSpeedCap), (20.0, 60.0, 1.0)); // 20 to the rule value
    assert_eq!(range(Field::SpeedCap), (20.0, 300.0, 1.0)); // no rule value: the hard range
    assert_eq!(range(Field::AccelCap), (500.0, 10_000.0, 1.0));
    assert_eq!(range(Field::OuterWallAccelCap), (500.0, 5000.0, 1.0));
    assert_eq!(range(Field::MaxFlowCap), (1.0, 25.0, 0.01));
    // Every field that may not be moved is locked.
    for field in Field::ALL.iter().copied() {
        let locked = matches!(
            plan.decision(field).unwrap().envelope,
            Envelope::Locked { .. }
        );
        let adjustable = bounds::spec(field).who.adjustable && field != Field::Intent;
        assert_eq!(locked, !adjustable, "{field}");
    }

    // Level 0 on a calm PLA plate: the bed may go from 60 to the window's top, the level
    // from 0 to 2, and a brim from none to 10 mm.
    let calm_plan = common::plan(&inputs(Family::Pla, PlateType::TexturedPei, vec![calm()]));
    let range = |field: Field| match calm_plan.decision(field).unwrap().envelope {
        Envelope::Range { min, max, .. } => (min, max),
        ref other => panic!("{field} has no range: {other:?}"),
    };
    assert_eq!(range(Field::Level), (0.0, 2.0));
    assert_eq!(range(Field::BrimWidth), (0.0, 10.0));
    assert_eq!(range(Field::FirstLayerBed), (60.0, 65.0));
    assert_eq!(range(Field::FirstLayerWallSpeed), (10.0, 30.0));
    assert_eq!(range(Field::WallLoops), (2.0, 6.0));
    assert_eq!(range(Field::AuxFanMax), (0.0, 50.0));

    // A warping material may take a brim of up to 15 mm; the flexible grade's flow cap and
    // outer wall cap are the top of their envelopes; SPD-2 caps are tops as well.
    let abs = common::plan(&inputs(Family::Abs, PlateType::TexturedPei, vec![calm()]));
    assert_eq!(
        abs.decision(Field::BrimWidth).unwrap().envelope,
        Envelope::Range {
            min: 5.0,
            max: 15.0,
            step: 1.0
        }
    );
    let tpu = common::plan(&sleeve());
    assert_eq!(
        tpu.decision(Field::MaxFlowCap).unwrap().envelope,
        Envelope::Range {
            min: 1.0,
            max: 2.5,
            step: 0.01
        }
    );
    assert_eq!(
        tpu.decision(Field::OuterWallSpeedCap).unwrap().envelope,
        Envelope::Range {
            min: 20.0,
            max: 30.0,
            step: 1.0
        }
    );
    // No aux fan cap on the TPU card: any cap is allowed, and none is the rule value.
    assert_eq!(
        tpu.decision(Field::AuxFanMax).unwrap().envelope,
        Envelope::Range {
            min: 0.0,
            max: 100.0,
            step: 1.0
        }
    );
    let tall = common::plan(&post());
    assert_eq!(
        tall.decision(Field::AccelCap).unwrap().envelope,
        Envelope::Range {
            min: 500.0,
            max: 4000.0,
            step: 1.0
        }
    );
    assert_eq!(
        tall.decision(Field::OuterWallAccelCap).unwrap().envelope,
        Envelope::Range {
            min: 500.0,
            max: 2000.0,
            step: 1.0
        }
    );
    // Level 2 on the post: the level cannot go anywhere, and says so as a range of one.
    assert_eq!(
        tall.decision(Field::Level).unwrap().envelope,
        Envelope::Range {
            min: 2.0,
            max: 2.0,
            step: 1.0
        }
    );
}

#[test]
fn the_safe_side_of_every_sticking_field_and_every_cap_is_stated() {
    for field in Field::ALL.iter().copied() {
        let spec = bounds::spec(field);
        let side = envelope::side(field);
        match spec.group {
            Group::Sticking => {
                assert_ne!(
                    side,
                    Side::Either,
                    "{field} of the sticking group has no safe side"
                );
                let expected = match safe_side(field) {
                    Safe::Higher => Side::Up,
                    Safe::Lower => Side::Down,
                    Safe::Same | Safe::Brim => Side::Fixed,
                };
                assert_eq!(side, expected, "{field}");
            }
            // Every motion field is a cap.
            Group::Motion => assert_eq!(side, Side::Down, "{field}"),
            _ => assert_eq!(side, Side::Either, "{field}"),
        }
    }
    // The comparison itself, on the cases that matter.
    let weaker = |field: Field, value: Value, reference: Value| {
        !envelope::not_weaker(field, &value, &reference)
    };
    assert!(weaker(Field::BrimWidth, Value::Whole(4), Value::Whole(5)));
    assert!(!weaker(Field::BrimWidth, Value::Whole(5), Value::Whole(5)));
    assert!(weaker(
        Field::FirstLayerWallSpeed,
        Value::Whole(21),
        Value::Whole(20)
    ));
    assert!(weaker(Field::AuxFanMax, Value::Unset, Value::Whole(30)));
    assert!(!weaker(Field::AuxFanMax, Value::Whole(30), Value::Unset));
    assert!(weaker(
        Field::BrimKind,
        Value::Token("none"),
        Value::Token("outer")
    ));
    assert!(weaker(
        Field::BrimKind,
        Value::Token("inner"),
        Value::Token("outer")
    ));
    assert!(!weaker(
        Field::BrimKind,
        Value::Token("outer_and_inner"),
        Value::Token("outer")
    ));
    assert!(weaker(
        Field::ElephantFoot,
        Value::Decimal(0.1),
        Value::Decimal(0.15)
    ));
    assert!(weaker(
        Field::FirstLayerBed,
        Value::Whole(60),
        Value::Whole(65)
    ));
    assert!(weaker(
        Field::OuterWallSpeedCap,
        Value::Whole(61),
        Value::Whole(60)
    ));
}

// ---- Worked examples ----------------------------------------------------------------------

#[test]
fn the_worked_examples_are_answered_as_documented() {
    // "No brim, the ring is small" is refused with AD-1 named.
    let (outcome, item) = ask(&gauge(), Field::BrimKind, text("none"));
    assert_eq!(
        (item.outcome, item.code),
        (Outcome::Refused, "not_allowed_here")
    );
    assert_eq!(item.rule, Some(RuleId::Ad1));
    assert!(item.message.contains("AD-1"), "{}", item.message);
    assert_eq!(outcome.inputs, gauge());
    assert_eq!(
        item.to_string(),
        "adhesion.brim_kind: refused: not_allowed_here (AD-1)"
    );
    // A brim narrower than the rule's is held to the rule's width, and AD-1 owns the bound.
    let (_, item) = ask(&gauge(), Field::BrimWidth, number(0.0));
    assert_eq!(
        (item.outcome, item.applied),
        (Outcome::Clamped, Some(Value::Whole(5)))
    );
    assert_eq!(item.rule, Some(RuleId::Ad1));
    assert_eq!(
        item.to_string(),
        "adhesion.brim_width_mm: clamped 0 -> 5 by AD-1"
    );

    // "A 12 mm brim on this lid" is clamped to 10, and the kind follows the width.
    let (outcome, item) = ask(&lid(), Field::BrimWidth, number(12.0));
    assert_eq!(
        (item.outcome, item.applied),
        (Outcome::Clamped, Some(Value::Whole(10)))
    );
    assert_eq!(
        item.to_string(),
        "adhesion.brim_width_mm: clamped 12 -> 10 by ENV-1"
    );
    assert_eq!(value(&outcome.plan, Field::BrimKind), Value::Token("outer"));
    let width = outcome.plan.decision(Field::BrimWidth).unwrap();
    assert_eq!(
        (width.source, width.asked),
        (Source::Agent, Some(Value::Whole(12)))
    );
    assert_eq!(
        width.sentence,
        "The assistant asked for 12. Hotend went as far as 10, the nearest value it allows on this plate (rule ENV-1)."
    );
    // No rule requires this brim, so it keeps the card's gap and the list mentions it.
    assert_eq!(value(&outcome.plan, Field::BrimGap), Value::Decimal(0.1));
    assert!(
        outcome
            .plan
            .checklist
            .items
            .iter()
            .any(|line| line.id == "watch_brim")
    );
    assert!(
        !plan(&lid())
            .checklist
            .items
            .iter()
            .any(|line| line.id == "watch_brim")
    );
    assert_eq!(outcome.plan.changed_by(Asker::Agent), 1);
    assert_eq!(outcome.plan.changed_by(Asker::Person), 0);

    // "This lattice has many small islands, so I ask for level 2" is accepted, and the
    // fields after the level take their level 2 values.
    let (outcome, item) = ask(&lid(), Field::Level, number(2.0));
    assert_eq!(item.outcome, Outcome::Accepted);
    assert_eq!(whole(&outcome.plan, Field::BrimWidth), 5);
    assert_eq!(whole(&outcome.plan, Field::FirstLayerWallSpeed), 15);
    assert_eq!(whole(&outcome.plan, Field::PartFanOffLayers), 5);
    // Level 3 comes from history only; a level below the rules' is held to the rules'.
    let (_, item) = ask(&lid(), Field::Level, number(3.0));
    assert_eq!(
        (item.outcome, item.applied, item.rule),
        (
            Outcome::Clamped,
            Some(Value::Whole(2)),
            Some(RuleId::LvlAsk)
        )
    );
    let (outcome, item) = ask(&gauge(), Field::Level, number(0.0));
    assert_eq!(
        (item.outcome, item.applied, item.rule),
        (Outcome::Clamped, Some(Value::Whole(1)), Some(RuleId::Lvl1))
    );
    assert_eq!(outcome.plan.changed_by(Asker::Agent), 0);

    // "The spire is tall, cap the outer wall acceleration at 1500" is accepted.
    let (outcome, item) = ask(&post(), Field::OuterWallAccelCap, number(1500.0));
    assert_eq!(item.outcome, Outcome::Accepted);
    assert_eq!(whole(&outcome.plan, Field::OuterWallAccelCap), 1500);
    // The cap cannot be raised above what SPD-2 gave.
    let (_, item) = ask(&post(), Field::OuterWallAccelCap, number(4000.0));
    assert_eq!(
        (item.outcome, item.applied, item.rule),
        (
            Outcome::Clamped,
            Some(Value::Whole(2000)),
            Some(RuleId::Spd2)
        )
    );

    // A 110 C bed for PLA is clamped to 65.
    let pla = inputs(Family::Pla, PlateType::TexturedPei, vec![calm()]);
    let (outcome, item) = ask(&pla, Field::FirstLayerBed, number(110.0));
    assert_eq!(
        (item.outcome, item.applied, item.rule),
        (Outcome::Clamped, Some(Value::Whole(65)), Some(RuleId::Plt2))
    );
    assert_eq!(whole(&outcome.plan, Field::LayersBed), 55);
    // A cooler bed than the rule's is held to the rule's.
    let (_, item) = ask(&pla, Field::FirstLayerBed, number(40.0));
    assert_eq!(
        (item.outcome, item.applied),
        (Outcome::Clamped, Some(Value::Whole(60)))
    );

    // A first layer speed of 50 on the ring is clamped to 20.
    let (_, item) = ask(&gauge(), Field::FirstLayerWallSpeed, number(50.0));
    assert_eq!(
        (item.outcome, item.applied, item.rule),
        (Outcome::Clamped, Some(Value::Whole(20)), Some(RuleId::Card))
    );

    // An inner brim on a bore is refused, although the value is verified.
    let (_, item) = ask(
        &box_with_bore(true),
        Field::BrimKind,
        text("outer_and_inner"),
    );
    assert_eq!(
        (item.outcome, item.code, item.rule),
        (Outcome::Refused, "not_allowed_here", Some(RuleId::Fit1))
    );

    // A flow cap above the rule's is clamped.
    let (_, item) = ask(&sleeve(), Field::MaxFlowCap, number(8.0));
    assert_eq!(
        (item.outcome, item.applied),
        (Outcome::Clamped, Some(Value::Decimal(2.5)))
    );
    let (outcome, item) = ask(&sleeve(), Field::MaxFlowCap, number(2.25));
    assert_eq!(item.outcome, Outcome::Accepted);
    assert_eq!(
        value(&outcome.plan, Field::MaxFlowCap),
        Value::Decimal(2.25)
    );

    // An enum value the slicer side has not verified is not allowed here.
    let (_, item) = ask(&pla, Field::InfillPattern, text("gyroid"));
    assert_eq!(
        (item.outcome, item.detail),
        (Outcome::Refused, "not_allowed.unverified")
    );
    let mut verified = pla.clone();
    verified.capabilities = everything();
    let (_, item) = ask(&verified, Field::InfillPattern, text("gyroid"));
    assert_eq!(item.outcome, Outcome::Accepted);

    // The intent of a gauge is locked; an intent on a product moves shell values only.
    let (outcome, item) = ask(&gauge(), Field::Intent, text("fast"));
    assert_eq!(
        (item.outcome, item.code, item.rule),
        (Outcome::Refused, "locked", Some(RuleId::Sig1))
    );
    assert_eq!(outcome.inputs, gauge());
    let (outcome, item) = ask(&verified, Field::Intent, text("strong"));
    assert_eq!(item.outcome, Outcome::Accepted);
    assert_eq!(whole(&outcome.plan, Field::WallLoops), 5);
    assert_sticks_as_well(&plan(&verified), &outcome.plan, "strong");

    // A field without A is not settable; a C field under a calibration says why.
    for field in [
        Field::BrimGap,
        Field::LayersBed,
        Field::PlateType,
        Field::Process,
        Field::PrimeTower,
        Field::ElephantFoot,
    ] {
        let (outcome, item) = ask(&gauge(), field, number(0.0));
        assert_eq!(
            (item.outcome, item.code),
            (Outcome::Refused, "not_settable"),
            "{field}"
        );
        assert_eq!((item.asked, item.applied), (None, None));
        assert_eq!(outcome.inputs, gauge());
    }
    let mut product = pla.clone();
    product.calibration = Some(gauge_signature());
    for field in [
        Field::ElephantFoot,
        Field::FirstLayerHeight,
        Field::FirstLayerLineWidth,
    ] {
        let (_, item) = ask(&product, field, number(0.2));
        assert_eq!(
            (item.outcome, item.code),
            (Outcome::Refused, "locked_by_calibration"),
            "{field}"
        );
    }

    // A brim kind alone on a plate without a brim breaks the tie between kind and width.
    let (outcome, item) = ask(&lid(), Field::BrimKind, text("outer"));
    assert_eq!(
        (item.outcome, item.code, item.detail),
        (Outcome::Refused, "conflict", "conflict.brim_needs_width")
    );
    assert_eq!(item.rule, Some(RuleId::Xf1));
    assert_eq!(outcome.inputs, lid());
}

#[test]
fn a_change_is_judged_again_whenever_the_plan_is_computed() {
    let pla = inputs(Family::Pla, PlateType::TexturedPei, vec![calm()]);
    // 25 mm/s is inside the level 0 envelope (10 to 30).
    let (first, item) = ask(&pla, Field::FirstLayerWallSpeed, number(25.0));
    assert_eq!(item.outcome, Outcome::Accepted);
    // Level 2 moves the rule value to 15: the accepted 25 is held to it, in the plan.
    let (second, item) = ask(&first.inputs, Field::Level, number(2.0));
    assert_eq!(item.outcome, Outcome::Accepted);
    let speed = second.plan.decision(Field::FirstLayerWallSpeed).unwrap();
    assert_eq!(
        (speed.value, speed.asked),
        (Value::Whole(15), Some(Value::Whole(25)))
    );
    assert_eq!(speed.answer.unwrap().answer, Answer::Clamped);
    assert_eq!(speed.source, Source::Rule);
    // The other way round ends in the same plan.
    let (level_first, _) = ask(&pla, Field::Level, number(2.0));
    let (other, item) = ask(
        &level_first.inputs,
        Field::FirstLayerWallSpeed,
        number(25.0),
    );
    assert_eq!(item.outcome, Outcome::Clamped);
    assert_eq!(bytes(&other.plan), bytes(&second.plan));

    // An accepted kind that a wider brim leaves no room for is dropped, and said so.
    let mut piece = common::strip(2.0);
    piece.inner_loops = vec![InnerLoop {
        min_width_mm: 22.0,
        fit: false,
    }];
    let mut ring = inputs(Family::Pla, PlateType::TexturedPei, vec![piece]);
    ring.capabilities = everything();
    let (with_inner, item) = ask(&ring, Field::BrimKind, text("outer_and_inner"));
    assert_eq!(item.outcome, Outcome::Accepted);
    let (wider, item) = ask(&with_inner.inputs, Field::BrimWidth, number(8.0));
    assert_eq!(item.outcome, Outcome::Accepted);
    assert_eq!(wider.dropped.len(), 1);
    assert_eq!(wider.dropped[0].field, Some(Field::BrimKind));
    assert_eq!(wider.dropped[0].detail, "not_allowed.brim_inner_room");
    assert_eq!(value(&wider.plan, Field::BrimKind), Value::Token("outer"));
    assert!(wider.inputs.changes.get(Field::BrimKind).is_none());

    // A refused change leaves an earlier accepted change of the same field in place.
    let (refused, item) = ask(&with_inner.inputs, Field::BrimKind, text("none"));
    assert_eq!(item.outcome, Outcome::Refused);
    assert_eq!(refused.inputs, with_inner.inputs);
    assert_eq!(
        value(&refused.plan, Field::BrimKind),
        Value::Token("outer_and_inner")
    );

    // The newest change per field wins, and the person can overrule the assistant.
    let (agent, _) = ask(&pla, Field::InfillPct, number(40.0));
    let person = propose(
        &agent.inputs,
        Asker::Person,
        vec![change("shell.infill_pct", number(20.0))],
    );
    assert_eq!(whole(&person.plan, Field::InfillPct), 20);
    assert_eq!(
        person.plan.decision(Field::InfillPct).unwrap().source,
        Source::Person
    );
    assert_eq!(
        (
            person.plan.changed_by(Asker::Agent),
            person.plan.changed_by(Asker::Person)
        ),
        (0, 1)
    );
    assert_eq!(person.inputs.changes.len(), 1);
}

#[test]
fn a_blocked_plan_takes_no_change() {
    for (name, inputs) in corpus() {
        if plan(&inputs).blocked.is_none() {
            continue;
        }
        let outcome = propose(
            &inputs,
            Asker::Agent,
            vec![
                change("adhesion.level", number(2.0)),
                change("intent", text("strong")),
            ],
        );
        let refusal = outcome.refused.as_ref().unwrap_or_else(|| panic!("{name}"));
        assert_eq!(refusal.code, "plan_blocked", "{name}");
        assert!(
            outcome.items.iter().all(|item| item.code == "plan_blocked"),
            "{name}"
        );
        assert_eq!(outcome.inputs, inputs, "{name}");
        assert!(outcome.plan.settings.is_none(), "{name}");
    }
}

// ---- Malformed and hostile proposals ----------------------------------------------------

const CANARY: &str = "CANARY-7f3a91";

/// Everything a refused call must leave alone, and everything it must not say back.
fn assert_refused_whole(inputs: &PlanInputs, outcome: &ProposalOutcome, context: &str) {
    let refusal = outcome
        .refused
        .as_ref()
        .unwrap_or_else(|| panic!("{context}: not refused"));
    assert_eq!(refusal.code, "invalid_arguments", "{context}");
    assert!(
        refusal.detail.starts_with("invalid."),
        "{context}: {}",
        refusal.detail
    );
    assert_eq!(outcome.inputs, *inputs, "{context}: something changed");
    assert_eq!(bytes(&outcome.plan), bytes(&plan(inputs)), "{context}");
    assert!(outcome.dropped.is_empty(), "{context}");
    for item in &outcome.items {
        assert_eq!(
            (item.outcome, item.code),
            (Outcome::Refused, "invalid_arguments"),
            "{context}"
        );
        assert_eq!((item.asked, item.applied), (None, None), "{context}");
    }
    // Nothing of what was sent is said back: no value, no reason, no summary.
    let said = serde_json::to_value(outcome).unwrap();
    assert!(outcome.items.iter().all(|item| item.note.is_none()));
    assert_eq!(outcome.summary, None, "{context}");
    assert!(
        !said.to_string().contains(CANARY),
        "{context}: the answer repeats what was sent"
    );
}

#[test]
fn malformed_values_are_refused_without_a_panic_and_change_nothing() {
    let base = lid();
    let long = "9".repeat(10_000);
    let hostile: Vec<(&str, ProposedValue, &str)> = vec![
        // Not finite, not plain.
        ("adhesion.brim_width_mm", number(f64::NAN), "invalid.number"),
        (
            "adhesion.brim_width_mm",
            number(f64::INFINITY),
            "invalid.number",
        ),
        (
            "adhesion.brim_width_mm",
            number(f64::NEG_INFINITY),
            "invalid.number",
        ),
        ("adhesion.brim_width_mm", number(-0.0), "invalid.number"),
        ("adhesion.level", number(-0.0), "invalid.number"),
        (
            "motion.max_flow_cap_mm3_s",
            number(f64::NAN),
            "invalid.number",
        ),
        // Huge, tiny and just outside.
        (
            "adhesion.brim_width_mm",
            number(1e308),
            "invalid.out_of_range",
        ),
        (
            "adhesion.brim_width_mm",
            number(-1e308),
            "invalid.out_of_range",
        ),
        (
            "adhesion.brim_width_mm",
            number(9_007_199_254_740_993.0),
            "invalid.out_of_range",
        ),
        (
            "adhesion.brim_width_mm",
            number(u64::MAX as f64),
            "invalid.out_of_range",
        ),
        (
            "adhesion.brim_width_mm",
            number(i64::MIN as f64),
            "invalid.out_of_range",
        ),
        (
            "adhesion.brim_width_mm",
            number(16.0),
            "invalid.out_of_range",
        ),
        (
            "adhesion.brim_width_mm",
            number(-1.0),
            "invalid.out_of_range",
        ),
        ("adhesion.level", number(4.0), "invalid.out_of_range"),
        ("first_layer.bed_c", number(121.0), "invalid.out_of_range"),
        ("first_layer.bed_c", number(29.0), "invalid.out_of_range"),
        (
            "motion.max_flow_cap_mm3_s",
            number(25.01),
            "invalid.out_of_range",
        ),
        (
            "motion.max_flow_cap_mm3_s",
            number(0.99),
            "invalid.out_of_range",
        ),
        (
            "motion.max_flow_cap_mm3_s",
            number(f64::MAX),
            "invalid.out_of_range",
        ),
        (
            "motion.max_flow_cap_mm3_s",
            number(f64::MIN_POSITIVE),
            "invalid.out_of_range",
        ),
        // A fraction where a whole number belongs.
        ("adhesion.brim_width_mm", number(5.5), "invalid.not_whole"),
        ("adhesion.level", number(1.000_000_1), "invalid.not_whole"),
        (
            "adhesion.brim_width_mm",
            number(f64::EPSILON),
            "invalid.not_whole",
        ),
        // The wrong kind of value.
        ("adhesion.brim_width_mm", text("5"), "invalid.wrong_type"),
        (
            "adhesion.brim_width_mm",
            text("5; M104 S300"),
            "invalid.wrong_type",
        ),
        ("adhesion.brim_width_mm", text(CANARY), "invalid.wrong_type"),
        ("adhesion.brim_width_mm", text(&long), "invalid.wrong_type"),
        (
            "adhesion.brim_width_mm",
            ProposedValue::Flag(true),
            "invalid.wrong_type",
        ),
        (
            "adhesion.brim_width_mm",
            ProposedValue::Other,
            "invalid.wrong_type",
        ),
        ("supports.enabled", number(1.0), "invalid.wrong_type"),
        ("supports.enabled", text("true"), "invalid.wrong_type"),
        ("adhesion.brim_kind", number(1.0), "invalid.wrong_type"),
        (
            "adhesion.brim_kind",
            ProposedValue::Flag(false),
            "invalid.wrong_type",
        ),
        ("intent", ProposedValue::Other, "invalid.wrong_type"),
        // A text that is no value of the field.
        ("adhesion.brim_kind", text("auto"), "invalid.unknown_value"),
        ("adhesion.brim_kind", text("Outer"), "invalid.unknown_value"),
        (
            "adhesion.brim_kind",
            text("outer "),
            "invalid.unknown_value",
        ),
        (
            "adhesion.brim_kind",
            text("outer\n"),
            "invalid.unknown_value",
        ),
        ("adhesion.brim_kind", text(""), "invalid.unknown_value"),
        ("adhesion.brim_kind", text(CANARY), "invalid.unknown_value"),
        ("adhesion.brim_kind", text(&long), "invalid.unknown_value"),
        ("intent", text("fit critical"), "invalid.unknown_value"),
        ("shell.seam", text("random; G28"), "invalid.unknown_value"),
        // A field that is none of Hotend's: slicer keys, paths, look-alikes, nothing.
        ("brim_type", text("outer_only"), "invalid.unknown_field"),
        (
            "machine_start_gcode",
            text("M104 S300"),
            "invalid.unknown_field",
        ),
        ("../x", number(1.0), "invalid.unknown_field"),
        ("", number(1.0), "invalid.unknown_field"),
        (
            "adhesion.brim_width_mm ",
            number(5.0),
            "invalid.unknown_field",
        ),
        (
            "Adhesion.Brim_Width_Mm",
            number(5.0),
            "invalid.unknown_field",
        ),
        (
            "adhesion.brim_width_mm\0",
            number(5.0),
            "invalid.unknown_field",
        ),
        (
            "adhesion.brim\u{0456}width_mm",
            number(5.0),
            "invalid.unknown_field",
        ),
        (
            "\u{0430}dhesion.level",
            number(2.0),
            "invalid.unknown_field",
        ),
        (CANARY, number(5.0), "invalid.unknown_field"),
    ];
    for (field, value, expected) in hostile {
        let context = format!("{field:.40} = {value:.60?}");
        let outcome = propose(&base, Asker::Agent, vec![change(field, value.clone())]);
        assert_refused_whole(&base, &outcome, &context);
        assert_eq!(outcome.items[0].detail, expected, "{context}");
        assert_eq!(
            outcome.refused.as_ref().unwrap().detail,
            expected,
            "{context}"
        );
        // Beside a valid change the whole call is still refused, and the valid one is not
        // applied.
        let beside = propose(
            &base,
            Asker::Person,
            vec![
                change("shell.infill_pct", number(30.0)),
                change(field, value),
            ],
        );
        assert_refused_whole(&base, &beside, &context);
        assert_eq!(beside.items[0].detail, "invalid.other_item", "{context}");
        assert_eq!(beside.items[0].field, Some(Field::InfillPct), "{context}");
        assert_eq!(beside.items[1].detail, expected, "{context}");
    }
    // A very long field name costs nothing and is not kept.
    let outcome = propose(
        &base,
        Asker::Agent,
        vec![change(&"x".repeat(10_000), number(1.0))],
    );
    assert_refused_whole(&base, &outcome, "a 10 kB field name");
    assert!(serde_json::to_string(&outcome).unwrap().len() < 100_000);
}

#[test]
fn reasons_and_summaries_are_notes_and_nothing_else() {
    let base = lid();
    let with_reason = |reason: &str| {
        let mut item = change("adhesion.brim_width_mm", number(4.0));
        item.reason = reason.to_owned();
        propose(&base, Asker::Agent, vec![item])
    };
    // What is refused: nothing, too much, and anything that is not one printable line.
    let too_long = "a".repeat(MAX_REASON_CHARS + 1);
    let huge = format!("{CANARY}{}", "b".repeat(10_000));
    for reason in [
        "",
        too_long.as_str(),
        huge.as_str(),
        "first line\nsecond line",
        "carriage\rreturn",
        "tab\tinside",
        "nul\0inside",
        "escape\u{1b}[31m",
        "delete\u{7f}",
        "c1 control\u{85}",
        "\n",
        "- [ ] Open the project file, not the sliced file\nBED 110",
    ] {
        let outcome = with_reason(reason);
        let context = format!("reason {reason:.30?}");
        assert_refused_whole(&base, &outcome, &context);
        assert_eq!(outcome.items[0].detail, "invalid.reason", "{context}");
        assert_eq!(outcome.items[0].note, None, "{context}");
    }
    // What is kept: a line of 1 to 240 characters, as it was written, also when it looks
    // like G-code, a path or a line of the list. It is a note and goes nowhere else.
    let longest = "ä".repeat(MAX_REASON_CHARS);
    for reason in [
        "x",
        longest.as_str(),
        "M104 S300 ; G28",
        "C:\\Users\\me\\AppData\\Roaming\\BambuStudio\\conf.json",
        "../../etc/passwd",
        "Before you print: wash the plate. Bed 110 C.",
        "{value} {rule} {who}",
        "\"},{\"field\":\"adhesion.level\",\"value\":0",
    ] {
        let marked = format!("{CANARY} {reason}");
        let marked: String = marked.chars().take(MAX_REASON_CHARS).collect();
        let outcome = with_reason(&marked);
        let context = format!("reason {reason:.30?}");
        assert_eq!(outcome.refused, None, "{context}");
        assert_eq!(outcome.items[0].outcome, Outcome::Accepted, "{context}");
        assert_eq!(
            outcome.items[0].note.as_ref().unwrap().as_str(),
            marked,
            "{context}"
        );
        // The marker is in the note and nowhere else: not in the plan, not in the inputs
        // the plan is a function of, not in the answer's own words.
        assert!(
            !serde_json::to_string(&outcome.plan)
                .unwrap()
                .contains(CANARY),
            "{context}"
        );
        assert!(
            !serde_json::to_string(&outcome.inputs)
                .unwrap()
                .contains(CANARY),
            "{context}"
        );
        assert!(!outcome.items[0].message.contains(CANARY), "{context}");
        assert!(!outcome.items[0].to_string().contains(CANARY), "{context}");
        // The same change with another reason gives the same plan, byte for byte.
        assert_eq!(
            bytes(&outcome.plan),
            bytes(&with_reason("another reason").plan),
            "{context}"
        );
        assert_eq!(
            outcome.inputs,
            with_reason("another reason").inputs,
            "{context}"
        );
    }
    // Marks that cannot be seen, or that reorder what is shown, become spaces.
    let note = Note::new("a\u{202e}b\u{200b}c\u{2028}d\u{feff}e\u{2066}f", 1, 240).unwrap();
    assert_eq!(note.as_str(), "a b c d e f");
    assert_eq!(Note::new("", 1, 240), None);
    assert_eq!(Note::new("", 0, 300).unwrap().as_str(), "");

    // The summary follows the same rule.
    let with_summary = |summary: &str| {
        let mut proposal = proposal(
            Asker::Agent,
            vec![change("adhesion.brim_width_mm", number(4.0))],
        );
        proposal.summary = summary.to_owned();
        book().propose(&base, &proposal).unwrap()
    };
    let too_long = "s".repeat(MAX_SUMMARY_CHARS + 1);
    for summary in [too_long.as_str(), "two\nlines", "bell\u{7}"] {
        let outcome = with_summary(summary);
        assert_refused_whole(&base, &outcome, "summary");
        assert_eq!(outcome.refused.as_ref().unwrap().detail, "invalid.summary");
        assert_eq!(outcome.summary, None);
    }
    let outcome = with_summary(&format!("{CANARY} wider brim"));
    assert_eq!(outcome.refused, None);
    assert!(
        outcome
            .summary
            .as_ref()
            .unwrap()
            .as_str()
            .starts_with(CANARY)
    );
    assert!(
        !serde_json::to_string(&outcome.plan)
            .unwrap()
            .contains(CANARY)
    );
    assert!(
        !serde_json::to_string(&outcome.inputs)
            .unwrap()
            .contains(CANARY)
    );
    assert_eq!(with_summary("").refused, None);
}

#[test]
fn the_shape_of_a_proposal_is_checked_before_anything_else() {
    let base = lid();
    let ok = |field: &str| change(field, number(4.0));
    // No change, too many changes, the same field twice.
    let outcome = propose(&base, Asker::Agent, Vec::new());
    assert_refused_whole(&base, &outcome, "empty");
    assert_eq!(outcome.refused.as_ref().unwrap().detail, "invalid.empty");
    let many: Vec<ProposedChange> = bounds::adjustable_fields()
        .take(MAX_CHANGES + 1)
        .map(|field| change(field.as_str(), number(1.0)))
        .collect();
    assert_eq!(many.len(), MAX_CHANGES + 1);
    let outcome = propose(&base, Asker::Agent, many);
    assert_refused_whole(&base, &outcome, "17 changes");
    assert_eq!(outcome.refused.as_ref().unwrap().detail, "invalid.too_many");
    assert!(outcome.items.is_empty());
    let flood = vec![ok("adhesion.brim_width_mm"); 10_000];
    let outcome = propose(&base, Asker::Agent, flood);
    assert_refused_whole(&base, &outcome, "10 000 changes");
    assert!(outcome.items.is_empty());
    let outcome = propose(
        &base,
        Asker::Agent,
        vec![
            ok("adhesion.brim_width_mm"),
            ok("shell.wall_loops"),
            ok("adhesion.brim_width_mm"),
        ],
    );
    assert_refused_whole(&base, &outcome, "duplicate");
    let details: Vec<&str> = outcome.items.iter().map(|item| item.detail).collect();
    assert_eq!(
        details,
        [
            "invalid.duplicate_field",
            "invalid.other_item",
            "invalid.duplicate_field"
        ]
    );
    // Sixteen valid changes are taken.
    let sixteen: Vec<ProposedChange> = [
        ("adhesion.level", number(1.0)),
        ("adhesion.brim_width_mm", number(6.0)),
        ("first_layer.wall_speed_mm_s", number(12.0)),
        ("first_layer.infill_speed_mm_s", number(20.0)),
        ("first_layer.accel_mm_s2", number(300.0)),
        ("intent", text("strong")),
        ("cooling.part_fan_off_layers", number(4.0)),
        ("cooling.aux_fan_max_pct", number(10.0)),
        ("shell.wall_loops", number(4.0)),
        ("shell.infill_pct", number(20.0)),
        ("supports.threshold_deg", number(40.0)),
        ("motion.outer_wall_speed_cap_mm_s", number(50.0)),
        ("motion.speed_cap_mm_s", number(150.0)),
        ("motion.accel_cap_mm_s2", number(3000.0)),
        ("motion.outer_wall_accel_cap_mm_s2", number(1500.0)),
        ("motion.max_flow_cap_mm3_s", number(12.5)),
    ]
    .into_iter()
    .map(|(field, value)| change(field, value))
    .collect();
    let outcome = propose(&base, Asker::Person, sixteen);
    assert_eq!(outcome.refused, None);
    for item in &outcome.items {
        assert_eq!(item.outcome, Outcome::Accepted, "{item}");
    }
    assert_eq!(outcome.plan.changed_by(Asker::Person), 16);
    assert_inside(&outcome.inputs, &outcome.plan, "sixteen changes");
    assert_sticks_as_well(&plan(&base), &outcome.plan, "sixteen changes");

    // The JSON arguments: unknown properties at every level, wrong types, missing parts.
    let good = json!({"changes": [{"field": "adhesion.brim_width_mm", "value": 4, "reason": "wider"}], "summary": "s"});
    let outcome = book().propose_json(&base, Asker::Agent, &good).unwrap();
    assert_eq!(outcome.refused, None);
    assert_eq!(outcome.items[0].outcome, Outcome::Accepted);
    let with_plate = json!({"plate": 2, "changes": [{"field": "intent", "value": "strong", "reason": "load"}], "summary": ""});
    assert_eq!(
        Proposal::from_json(Asker::Person, &with_plate)
            .unwrap()
            .plate,
        Some(2)
    );
    let item = json!({"field": "adhesion.brim_width_mm", "value": 4, "reason": "wider"});
    let bad_shapes = [
        json!(null),
        json!([]),
        json!("changes"),
        json!({}),
        json!({"changes": [item]}),
        json!({"summary": "s"}),
        json!({"changes": {}, "summary": "s"}),
        json!({"changes": [item], "summary": 3}),
        json!({"changes": [item], "summary": "s", "destination": "C:/Users"}),
        json!({"changes": [item], "summary": "s", "plate": 0}),
        json!({"changes": [item], "summary": "s", "plate": 65}),
        json!({"changes": [item], "summary": "s", "plate": "1"}),
        json!({"changes": [item], "summary": "s", "plate": 1.5}),
        json!({"changes": [[]], "summary": "s"}),
        json!({"changes": ["adhesion.brim_width_mm"], "summary": "s"}),
        json!({"changes": [{"field": "adhesion.brim_width_mm", "value": 4}], "summary": "s"}),
        json!({"changes": [{"field": "adhesion.brim_width_mm", "reason": "r"}], "summary": "s"}),
        json!({"changes": [{"value": 4, "reason": "r"}], "summary": "s"}),
        json!({"changes": [{"field": 7, "value": 4, "reason": "r"}], "summary": "s"}),
        json!({"changes": [{"field": "adhesion.brim_width_mm", "value": 4, "reason": ["r"]}], "summary": "s"}),
        json!({"changes": [{"field": "adhesion.brim_width_mm", "value": 4, "reason": "r", "key": "brim_width"}], "summary": "s"}),
        json!({"changes": [{"field": "adhesion.brim_width_mm", "value": 4, "reason": "r", CANARY: 1}], "summary": "s"}),
    ];
    for arguments in &bad_shapes {
        let context = arguments.to_string();
        assert_eq!(
            Proposal::from_json(Asker::Agent, arguments),
            Err(Invalid::Shape),
            "{context}"
        );
        let outcome = book().propose_json(&base, Asker::Agent, arguments).unwrap();
        assert_refused_whole(&base, &outcome, &context);
        assert_eq!(
            outcome.refused.as_ref().unwrap().detail,
            "invalid.shape",
            "{context}"
        );
    }
    // Ten thousand changes are refused before one of them is read.
    let flood = json!({"changes": vec![json!({"field": CANARY, "value": [], "reason": 1}); 10_000], "summary": "s"});
    assert_eq!(
        Proposal::from_json(Asker::Agent, &flood),
        Err(Invalid::TooMany)
    );
    let outcome = book().propose_json(&base, Asker::Agent, &flood).unwrap();
    assert_refused_whole(&base, &outcome, "a flood as JSON");
    // Values JSON can hold and a field cannot: lists, objects, nothing, numbers too large.
    for (value, expected) in [
        (json!([5]), "invalid.wrong_type"),
        (json!({"mm": 5}), "invalid.wrong_type"),
        (json!(null), "invalid.wrong_type"),
        (json!("5"), "invalid.wrong_type"),
        (json!(true), "invalid.wrong_type"),
        (json!(18_446_744_073_709_551_615u64), "invalid.out_of_range"),
        (json!(-9_223_372_036_854_775_808i64), "invalid.out_of_range"),
        (json!(5.5), "invalid.not_whole"),
        (json!(-0.0), "invalid.number"),
    ] {
        let arguments = json!({"changes": [{"field": "adhesion.brim_width_mm", "value": value, "reason": "r"}], "summary": "s"});
        let outcome = book()
            .propose_json(&base, Asker::Agent, &arguments)
            .unwrap();
        assert_refused_whole(&base, &outcome, &value.to_string());
        assert_eq!(outcome.items[0].detail, expected, "{value}");
    }
    // A whole number written with a fraction of zero is that whole number.
    let arguments = json!({"changes": [{"field": "adhesion.brim_width_mm", "value": 4.0, "reason": "r"}], "summary": "s"});
    let outcome = book()
        .propose_json(&base, Asker::Agent, &arguments)
        .unwrap();
    assert_eq!(outcome.items[0].applied, Some(Value::Whole(4)));
}

#[test]
fn every_answer_of_the_catalogue_is_reached() {
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    let mut note = |outcome: &ProposalOutcome| {
        seen.extend(outcome.refused.iter().map(|refusal| refusal.detail));
        seen.extend(outcome.items.iter().map(|item| item.detail));
        seen.extend(outcome.dropped.iter().map(|item| item.detail));
    };
    let agent = |inputs: &PlanInputs, field: &str, value: ProposedValue| {
        propose(inputs, Asker::Agent, vec![change(field, value)])
    };
    let pla = |piece: PieceFacts| inputs(Family::Pla, PlateType::TexturedPei, vec![piece]);
    let verified = |mut inputs: PlanInputs| {
        inputs.capabilities = everything();
        inputs
    };
    let calm_pla = pla(calm());

    // Accepted and clamped.
    note(&agent(&calm_pla, "adhesion.brim_width_mm", number(4.0)));
    note(&agent(&calm_pla, "adhesion.brim_width_mm", number(14.0)));
    // The whole call.
    note(
        &book()
            .propose_json(&calm_pla, Asker::Agent, &json!(7))
            .unwrap(),
    );
    note(&propose(&calm_pla, Asker::Agent, Vec::new()));
    note(&propose(
        &calm_pla,
        Asker::Agent,
        vec![change("intent", text("fast")); MAX_CHANGES + 1],
    ));
    note(&agent(&calm_pla, "brim_width", number(4.0)));
    note(&agent(&calm_pla, "adhesion.brim_width_mm", text("4")));
    note(&agent(
        &calm_pla,
        "adhesion.brim_width_mm",
        number(f64::NAN),
    ));
    note(&agent(&calm_pla, "adhesion.brim_width_mm", number(4.5)));
    note(&agent(&calm_pla, "adhesion.brim_width_mm", number(40.0)));
    note(&agent(&calm_pla, "adhesion.brim_kind", text("auto")));
    note(&propose(
        &calm_pla,
        Asker::Agent,
        vec![
            change("intent", text("fast")),
            change("intent", text("strong")),
            change("shell.infill_pct", number(9.0)),
        ],
    ));
    let mut no_reason = change("intent", text("fast"));
    no_reason.reason.clear();
    note(&propose(&calm_pla, Asker::Agent, vec![no_reason]));
    let mut long_summary = proposal(Asker::Agent, vec![change("intent", text("fast"))]);
    long_summary.summary = "s".repeat(MAX_SUMMARY_CHARS + 1);
    note(&book().propose(&calm_pla, &long_summary).unwrap());
    // No plan, no authority, locks.
    let mut no_plate = calm_pla.clone();
    no_plate.setup =
        hotend_settings::inputs::Setup::new(None, calm_pla.setup.spools().to_vec()).unwrap();
    note(&agent(&no_plate, "intent", text("fast")));
    note(&agent(&calm_pla, "adhesion.brim_gap_mm", number(0.0)));
    let mut product = calm_pla.clone();
    product.calibration = Some(gauge_signature());
    note(&agent(&product, "adhesion.elephant_foot_mm", number(0.1)));
    note(&agent(&gauge(), "intent", text("fast")));
    // The brim.
    note(&agent(&gauge(), "adhesion.brim_kind", text("none")));
    let with_brim = agent(&calm_pla, "adhesion.brim_width_mm", number(4.0));
    note(&agent(
        &with_brim.inputs,
        "adhesion.brim_kind",
        text("none"),
    ));
    note(&agent(
        &verified(pla(strip_ring(30.0, false))),
        "adhesion.brim_kind",
        text("inner"),
    ));
    note(&agent(
        &verified(pla(strip_ring(30.0, true))),
        "adhesion.brim_kind",
        text("outer_and_inner"),
    ));
    note(&agent(
        &verified(pla(strip_ring(12.0, false))),
        "adhesion.brim_kind",
        text("outer_and_inner"),
    ));
    note(&agent(
        &pla(strip_ring(30.0, false)),
        "adhesion.brim_kind",
        text("outer_and_inner"),
    ));
    note(&agent(&calm_pla, "adhesion.brim_kind", text("outer")));
    // Supports.
    let ledge = |area: f64, over: bool| {
        pla(with_regions(
            calm(),
            vec![OverhangRegion::free(area, 12.0, over)],
        ))
    };
    note(&agent(
        &ledge(300.0, false),
        "supports.enabled",
        ProposedValue::Flag(false),
    ));
    note(&agent(
        &calm_pla,
        "supports.enabled",
        ProposedValue::Flag(true),
    ));
    note(&agent(
        &ledge(300.0, true),
        "supports.where",
        text("bed_only"),
    ));

    // What the person made safer is kept against the agent.
    let wider = propose(
        &calm_pla,
        Asker::Person,
        vec![change("adhesion.brim_width_mm", number(8.0))],
    );
    note(&agent(&wider.inputs, "adhesion.brim_width_mm", number(2.0)));

    let all: BTreeSet<&'static str> = ANSWERS.iter().map(|(id, _)| *id).collect();
    let missing: Vec<&&str> = all.difference(&seen).collect();
    // `conflict` without a detail answers a broken cross-field rule that has no sentence
    // of its own. No input is known to cause one: the envelopes are built so that the one
    // tie between two fields, the brim's kind and width, is the only one a change can break.
    assert_eq!(missing, [&"conflict"], "answers no proposal reaches");
    assert!(seen.is_subset(&all));
}

#[test]
fn accepted_changes_can_be_stored_and_read_back() {
    let mut lcg = Lcg(0x0057_04ED);
    for (inputs, _) in plates(40) {
        let outcome = propose(&inputs, asker(&mut lcg), random_changes(&mut lcg, 8));
        let stored = serde_json::to_value(&outcome.inputs.changes).unwrap();
        let restored = Changes::restore(&stored).unwrap();
        assert_eq!(restored, outcome.inputs.changes);
        let mut again = outcome.inputs.clone();
        again.changes = restored;
        assert_eq!(bytes(&plan(&again)), bytes(&outcome.plan));
    }
    // The stored form, and what is not it.
    let set = changes(
        &[
            (Field::BrimWidth, Value::Whole(7)),
            (Field::MaxFlowCap, Value::Decimal(2.5)),
        ],
        Asker::Person,
    );
    assert_eq!(
        serde_json::to_value(&set).unwrap(),
        json!([
            {"field": "adhesion.brim_width_mm", "value": 7, "by": "person"},
            {"field": "motion.max_flow_cap_mm3_s", "value": 2.5, "by": "person"},
        ])
    );
    for bad in [
        json!({}),
        json!([[]]),
        json!([{"field": "adhesion.brim_width_mm", "value": 7}]),
        json!([{"field": "adhesion.brim_width_mm", "value": 7, "by": "other"}]),
        json!([{"field": "adhesion.brim_width_mm", "value": 7, "by": "person", "reason": "x"}]),
        json!([{"field": "adhesion.brim_width_mm", "value": "7", "by": "person"}]),
        json!([{"field": "adhesion.brim_width_mm", "value": 99, "by": "person"}]),
        json!([{"field": "brim_width", "value": 7, "by": "person"}]),
        json!([
            {"field": "adhesion.brim_width_mm", "value": 7, "by": "person"},
            {"field": "adhesion.brim_width_mm", "value": 8, "by": "agent"},
        ]),
    ] {
        assert_eq!(Changes::restore(&bad), Err(ChangeError::Shape), "{bad}");
    }
    // A set takes only fields that may be moved, typed values in the hard range, and it
    // leaves the level and the intent to the inputs of their own.
    let none = Changes::none;
    assert_eq!(
        none().with(Field::BrimGap, Value::Decimal(0.3), Asker::Agent),
        Err(ChangeError::NotAdjustable(Field::BrimGap))
    );
    assert_eq!(
        none().with(Field::Level, Value::Whole(2), Asker::Agent),
        Err(ChangeError::OwnInput(Field::Level))
    );
    assert_eq!(
        none().with(Field::AuxFanMax, Value::Unset, Asker::Agent),
        Err(ChangeError::Unset(Field::AuxFanMax))
    );
    assert!(matches!(
        none().with(Field::BrimWidth, Value::Whole(16), Asker::Agent),
        Err(ChangeError::Range(_))
    ));
    assert!(matches!(
        none().with(Field::BrimWidth, Value::Token("5"), Asker::Agent),
        Err(ChangeError::Range(_))
    ));
    assert!(matches!(
        none().with(Field::MaxFlowCap, Value::Decimal(f64::NAN), Asker::Agent),
        Err(ChangeError::Range(_))
    ));
    // A change kept in the level of a history: the set and the level are separate inputs.
    let mut inputs = lid();
    inputs.history = History::new(1, None).unwrap();
    let outcome = propose(
        &inputs,
        Asker::Agent,
        vec![change("adhesion.level", number(2.0))],
    );
    assert_eq!(outcome.inputs.level.map(|asked| asked.value), Some(2));
    assert!(outcome.inputs.changes.is_empty());
    assert_eq!(outcome.inputs.reset().level, None);
    let _ = (block(1.0, 1.0, 1.0), foot(1.0, 1.0, 1.0, 1.0), Intent::Fast);
    let _ = (FitAtBed {
        pair: String::new(),
        side: FitSide::Inside,
    },);
}
