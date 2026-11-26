//! The two halves together: the facts the engine states for the ring gauge, read as the engine
//! wrote them, give the plan of the hand-made recipe.
//!
//! `python/tests/engine/data/ring-gauge-facts.json` is what the engine answers for the pocket
//! ring gauge; the engine's own test (`test_printfacts.py`) fails when the engine stops
//! answering it. `tests/golden.rs` checks the same recipe on facts written by hand, so a
//! change on either side that moves the gauge's plan shows here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{calm, inputs, plan, token, value, whole};
use hotend_settings::facts::{FitSide, PlateFacts};
use hotend_settings::model::{Envelope, Family, Field, PlateType, SpoolSlot, Value};

const RING_GAUGE: &str = include_str!("../../../python/tests/engine/data/ring-gauge-facts.json");

fn ring_gauge_facts() -> PlateFacts {
    PlateFacts::from_json(RING_GAUGE).expect("the rules read the engine's facts as written")
}

/// What the rules must find in the engine's words: one thin ring whose inside wall is a
/// mating face that rises from the bed.
#[test]
fn the_engines_ring_gauge_facts_say_what_the_rules_read() {
    let facts = ring_gauge_facts();
    assert_eq!(facts.family(), Family::Pla);
    let [ring] = facts.pieces() else {
        panic!("the ring gauge is one piece");
    };
    assert!((ring.thin_strip_mm() - 2.0).abs() < 0.05);
    assert!(facts.fit_at_bed());
    assert_eq!(ring.fit_at_bed.len(), 1);
    assert_eq!(ring.fit_at_bed[0].side, FitSide::Inside);
    assert!(ring.overhang_regions.as_ref().is_some_and(Vec::is_empty));
    assert_eq!(ring.inner_loops.len(), 1);
    assert!(ring.inner_loops[0].fit);
}

/// The recipe that printed the gauge, from the engine's facts: Textured PEI, bed 65 C on the
/// first layer, an outer brim of 5 mm that touches the piece, a first layer of 20 and
/// 30 mm/s, part cooling off for 3 layers, 3 walls, no supports and no prime tower. The one
/// difference to the hand-made project is the bed after layer 1 (55 by FIT-5, not 60).
#[test]
fn the_engines_ring_gauge_facts_give_the_hand_made_recipe() {
    let mut inputs = inputs(Family::Pla, PlateType::TexturedPei, vec![calm()]);
    inputs.facts = ring_gauge_facts();
    inputs.gauge = true;
    let plan = plan(&inputs);
    assert_eq!(plan.blocked, None);

    assert_eq!(token(&plan, Field::PlateType), "textured-pei");
    assert_eq!(value(&plan, Field::Spool), Value::Slot(SpoolSlot::Ams(1)));
    assert_eq!(whole(&plan, Field::FirstLayerBed), 65);
    assert_eq!(token(&plan, Field::BrimKind), "outer");
    assert_eq!(whole(&plan, Field::BrimWidth), 5);
    assert_eq!(value(&plan, Field::BrimGap), Value::Decimal(0.0));
    assert_eq!(whole(&plan, Field::FirstLayerWallSpeed), 20);
    assert_eq!(whole(&plan, Field::FirstLayerInfillSpeed), 30);
    assert_eq!(whole(&plan, Field::PartFanOffLayers), 3);
    assert_eq!(whole(&plan, Field::AuxFanFirstLayers), 0);
    assert_eq!(whole(&plan, Field::AuxFanMax), 30);
    assert_eq!(whole(&plan, Field::WallLoops), 3);
    assert_eq!(value(&plan, Field::SupportsEnabled), Value::Flag(false));
    assert_eq!(value(&plan, Field::PrimeTower), Value::Flag(false));

    // The one difference, and the rule that makes it.
    assert_eq!(whole(&plan, Field::LayersBed), 55);
    assert_eq!(
        plan.decision(Field::LayersBed).unwrap().rule.as_str(),
        "FIT-5"
    );

    // How it came about: the thin ring asks for the brim, the fit face closes its gap.
    assert_eq!(whole(&plan, Field::Level), 1);
    assert_eq!(
        plan.decision(Field::BrimWidth).unwrap().rule.as_str(),
        "AD-1"
    );
    assert_eq!(
        plan.decision(Field::BrimGap).unwrap().rule.as_str(),
        "FIT-2"
    );
    let derived = plan.derived.as_ref().unwrap();
    assert!(derived.brim_required && derived.fit_plate);
    assert_eq!(derived.asked_brim_width_mm, 5);

    // Nothing may take the brim away, and none fits inside the bore the thing drops through.
    match &plan.decision(Field::BrimKind).unwrap().envelope {
        Envelope::Set { allowed } => assert_eq!(allowed, &[Value::Token("outer")]),
        other => panic!("the brim kind has no allowed set: {other:?}"),
    }
    match plan.decision(Field::BrimWidth).unwrap().envelope {
        Envelope::Range { min, .. } => assert_eq!(min, 5.0),
        ref other => panic!("the brim width has no range: {other:?}"),
    }

    // Field by field the plan is the one the hand-written gauge facts give (tests/golden.rs).
    let by_hand = common::plan(&common::gauge());
    for field in Field::ALL {
        assert_eq!(plan.value(*field), by_hand.value(*field), "{field}");
    }

    // The list tells the person to scrape the brim off the mating face.
    let lines: Vec<&str> = plan
        .checklist
        .lines(Some(20), false)
        .iter()
        .map(|line| line.id)
        .collect();
    assert!(lines.contains(&"after.brim_fit"), "{lines:?}");
    assert!(lines.contains(&"outcome"), "{lines:?}");
}
