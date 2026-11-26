//! Golden plans: the worked examples and every material
//! on every plate at levels 0, 1 and 2, as snapshots.
//!
//! Update the snapshots with `HOTEND_UPDATE_SNAPSHOTS=1` and review the diff: a changed
//! number here is a changed print.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{book, box_with_bore, gauge, lid, plan, post, sleeve, snapshot, token, value, whole};
use hotend_settings::inputs::History;
use hotend_settings::model::{Envelope, Family, Field, PlateType, PrintPlan, Source, Value};

fn pretty(plan: &PrintPlan) -> String {
    serde_json::to_string_pretty(plan).unwrap() + "\n"
}

fn allowed(plan: &PrintPlan, field: Field) -> Vec<Value> {
    match &plan.decision(field).unwrap().envelope {
        Envelope::Set { allowed } => allowed.clone(),
        other => panic!("{field} has no allowed set: {other:?}"),
    }
}

fn lines(plan: &PrintPlan, minutes: Option<u32>) -> Vec<&'static str> {
    plan.checklist
        .lines(minutes, false)
        .iter()
        .map(|line| line.id)
        .collect()
}

#[test]
fn the_gauge_plan_equals_the_hand_made_recipe_except_the_bed_after_layer_one() {
    let plan = plan(&gauge());
    assert_eq!(plan.blocked, None);

    // The recipe that produced the intended G-code (sliced 17 November 2026), field by field.
    assert_eq!(token(&plan, Field::PlateType), "textured-pei");
    assert_eq!(whole(&plan, Field::FirstLayerBed), 65);
    assert_eq!(token(&plan, Field::BrimKind), "outer");
    assert_eq!(whole(&plan, Field::BrimWidth), 5);
    assert_eq!(value(&plan, Field::BrimGap), Value::Decimal(0.0));
    assert_eq!(whole(&plan, Field::SkirtLoops), 0);
    assert_eq!(whole(&plan, Field::FirstLayerWallSpeed), 20);
    assert_eq!(whole(&plan, Field::FirstLayerInfillSpeed), 30);
    assert_eq!(whole(&plan, Field::PartFanOffLayers), 3);
    assert_eq!(whole(&plan, Field::AuxFanFirstLayers), 0);
    assert_eq!(whole(&plan, Field::AuxFanMax), 30);
    assert_eq!(whole(&plan, Field::WallLoops), 3);
    assert_eq!(value(&plan, Field::SupportsEnabled), Value::Flag(false));
    assert_eq!(value(&plan, Field::PrimeTower), Value::Flag(false));
    assert_eq!(
        value(&plan, Field::Colour).to_owned(),
        plan.settings.as_ref().unwrap().get(Field::Colour)
    );
    assert_eq!(
        serde_json::to_value(value(&plan, Field::Colour)).unwrap(),
        "#898989"
    );
    // The one difference: the hand-made project kept the bed at 60 C after the first layer.
    // Mating faces stand on the bed, so FIT-5 returns to the level 0 value.
    assert_eq!(whole(&plan, Field::LayersBed), 55);
    assert_eq!(
        plan.decision(Field::LayersBed).unwrap().rule.as_str(),
        "FIT-5"
    );

    // How it came about.
    assert_eq!(whole(&plan, Field::Level), 1);
    assert_eq!(
        plan.decision(Field::BrimWidth).unwrap().rule.as_str(),
        "AD-1"
    );
    assert_eq!(
        plan.decision(Field::BrimGap).unwrap().rule.as_str(),
        "FIT-2"
    );
    assert_eq!(token(&plan, Field::Intent), "fit_critical");
    assert_eq!(
        plan.decision(Field::Intent).unwrap().envelope,
        Envelope::Locked {
            rule: plan.decision(Field::Intent).unwrap().rule
        }
    );
    assert_eq!(token(&plan, Field::Process), "standard-0.20mm");
    assert_eq!(whole(&plan, Field::OuterWallSpeedCap), 60);
    let derived = plan.derived.as_ref().unwrap();
    assert_eq!(
        derived
            .fired
            .iter()
            .map(|rule| rule.as_str())
            .collect::<Vec<_>>(),
        ["AD-1", "AD-2"]
    );
    assert!(derived.brim_required && derived.fit_plate);
    assert_eq!(derived.asked_brim_width_mm, 5);
    // `none` is not an allowed kind, and neither is a brim inside the bore.
    assert_eq!(allowed(&plan, Field::BrimKind), [Value::Token("outer")]);
    // The four baseline fields are left to the standard preset; nothing else is unset
    // except what version 1 never writes.
    for field in [
        Field::ElephantFoot,
        Field::FirstLayerHeight,
        Field::FirstLayerLineWidth,
        Field::FirstLayerAccel,
    ] {
        assert_eq!(value(&plan, field), Value::Unset, "{field}");
        assert_eq!(plan.decision(field).unwrap().source, Source::Profile);
        assert_eq!(plan.decision(field).unwrap().rule.as_str(), "STK-1");
    }

    // The whole list, eight lines, for a print of twenty minutes.
    assert_eq!(
        lines(&plan, Some(20)),
        [
            "plate.textured-pei",
            "spool.ams",
            "open_sliced",
            "no_sync",
            "bed_levelling",
            "watch_brim",
            "after.brim_fit",
            "outcome"
        ]
    );
    let shown = plan.checklist.lines(Some(20), false);
    assert!(
        shown[1]
            .text
            .starts_with("This print uses your grey PLA from slot 1.")
    );
    assert!(shown[5].text.contains("about 10 brim lines"));

    // The signature a later product is calibrated against.
    let signature = book().expected_signature(&plan).unwrap();
    assert_eq!(signature.parts().first_layer_bed_c, 65);
    assert_eq!(signature.parts().layers_bed_c, 55);
    assert_eq!(signature.parts().elephant_foot_mm, 0.15);
    assert_eq!(signature.sha256().unwrap().len(), 64);

    snapshot("plan-gauge-pla-textured-pei.json", &pretty(&plan));
}

#[test]
fn the_phone_sleeve_of_tpu_90a() {
    let plan = plan(&sleeve());
    assert_eq!(token(&plan, Field::BrimKind), "none");
    assert_eq!(whole(&plan, Field::BrimWidth), 0);
    assert_eq!(whole(&plan, Field::Level), 0);
    assert_eq!(
        (
            whole(&plan, Field::FirstLayerBed),
            whole(&plan, Field::LayersBed)
        ),
        (35, 35)
    );
    assert_eq!(
        (
            whole(&plan, Field::FirstLayerWallSpeed),
            whole(&plan, Field::FirstLayerInfillSpeed)
        ),
        (15, 20)
    );
    assert_eq!(value(&plan, Field::MaxFlowCap), Value::Decimal(2.5));
    assert_eq!(whole(&plan, Field::OuterWallSpeedCap), 30);
    assert_eq!(whole(&plan, Field::NozzleId), 1);
    assert_eq!(
        serde_json::to_value(value(&plan, Field::Spool)).unwrap(),
        "external_main"
    );
    assert_eq!(value(&plan, Field::AuxFanMax), Value::Unset);
    // Ten lines: the hand-loading lines (two lines hold the four sentences), and the list
    // still ends with what to do afterwards and the question.
    let shown = lines(&plan, Some(90));
    assert_eq!(
        shown,
        [
            "plate.textured-pei",
            "glue.none",
            "spool.external",
            "tpu.dry",
            "tpu.load",
            "open_sliced",
            "no_sync",
            "bed_levelling",
            "after.flexible",
            "outcome"
        ]
    );
    snapshot("plan-sleeve-tpu-90a.json", &pretty(&plan));
}

#[test]
fn the_tall_petg_post() {
    let plan = plan(&post());
    let derived = plan.derived.as_ref().unwrap();
    assert_eq!(
        derived
            .fired
            .iter()
            .map(|rule| rule.as_str())
            .collect::<Vec<_>>(),
        ["AD-2", "AD-3"]
    );
    // Brim 8 mm by AD-3, then 10 at level 2.
    assert_eq!(derived.asked_brim_width_mm, 8);
    assert_eq!(whole(&plan, Field::Level), 2);
    assert_eq!(whole(&plan, Field::BrimWidth), 10);
    assert_eq!(value(&plan, Field::BrimGap), Value::Decimal(0.0));
    assert_eq!(
        (
            whole(&plan, Field::FirstLayerBed),
            whole(&plan, Field::LayersBed)
        ),
        (75, 70)
    );
    assert_eq!(
        (
            whole(&plan, Field::FirstLayerWallSpeed),
            whole(&plan, Field::FirstLayerInfillSpeed)
        ),
        (15, 25)
    );
    assert_eq!(whole(&plan, Field::PartFanOffLayers), 5);
    assert_eq!(value(&plan, Field::FirstLayerHeight), Value::Decimal(0.25));
    // SPD-2.
    assert_eq!(whole(&plan, Field::AccelCap), 4000);
    assert_eq!(whole(&plan, Field::OuterWallAccelCap), 2000);
    assert_eq!(whole(&plan, Field::OuterWallSpeedCap), 100);
    assert!(derived.gentle_motion);
    assert!(
        plan.warnings
            .iter()
            .any(|warning| warning.code == "turn_or_split")
    );
    // The venting line when the print takes over an hour, and not otherwise.
    assert!(lines(&plan, Some(61)).contains(&"vent"));
    assert!(!lines(&plan, Some(60)).contains(&"vent"));
    assert!(lines(&plan, None).contains(&"vent"));
    snapshot("plan-post-petg.json", &pretty(&plan));
}

#[test]
fn the_box_with_a_bore_a_cross_hole_and_a_ledge() {
    let plan = plan(&box_with_bore(true));
    assert_eq!(token(&plan, Field::BrimKind), "none");
    assert_eq!(whole(&plan, Field::Level), 0);
    // On because of the ledge; it may be switched off.
    assert_eq!(value(&plan, Field::SupportsEnabled), Value::Flag(true));
    assert_eq!(
        allowed(&plan, Field::SupportsEnabled),
        [Value::Flag(true), Value::Flag(false)]
    );
    assert_eq!(plan.derived.as_ref().unwrap().support_mm2, 60.0);
    assert!(
        plan.decision(Field::SupportsEnabled)
            .unwrap()
            .sentence
            .starts_with("Supports are on because of one overhanging area of about 60 mm2.")
    );
    // The cross hole lies over the part but does not count, so supports stay on the bed.
    assert_eq!(token(&plan, Field::SupportsWhere), "bed_only");
    // The bore is a declared fit: no brim inside it, although the values are verified.
    assert_eq!(
        allowed(&plan, Field::BrimKind),
        [Value::Token("none"), Value::Token("outer")]
    );
    snapshot("plan-box-pla.json", &pretty(&plan));

    // The cross hole alone gives supports off.
    let plan = common::plan(&box_with_bore(false));
    assert_eq!(value(&plan, Field::SupportsEnabled), Value::Flag(false));
    assert_eq!(plan.derived.as_ref().unwrap().support_mm2, 0.0);
}

#[test]
fn the_lid_with_a_bottom_fillet_and_a_slot() {
    let plan = plan(&lid());
    assert_eq!(plan.derived.as_ref().unwrap().support_mm2, 0.0);
    assert_eq!(value(&plan, Field::SupportsEnabled), Value::Flag(false));
    assert_eq!(
        allowed(&plan, Field::SupportsEnabled),
        [Value::Flag(false), Value::Flag(true)]
    );
    assert_eq!(token(&plan, Field::SupportsWhere), "bed_only");
    snapshot("plan-lid-pla.json", &pretty(&plan));
}

/// One line per plan: the values a level, a plate and a material decide.
fn summary(plan: &PrintPlan) -> String {
    if let Some(blocked) = &plan.blocked {
        return format!("blocked {}", blocked.code);
    }
    let show = |field: Field| match value(plan, field) {
        Value::Unset => "-".to_owned(),
        other => serde_json::to_string(&other)
            .unwrap()
            .trim_matches('"')
            .to_owned(),
    };
    let warnings: Vec<&str> = plan.warnings.iter().map(|warning| warning.code).collect();
    format!(
        "level {} brim {} {} gap {} | first layer {}/{} mm/s h {} | bed {}/{} window {:?} | fan off {} aux {}/{} | list {} | {}",
        show(Field::Level),
        show(Field::BrimKind),
        show(Field::BrimWidth),
        show(Field::BrimGap),
        show(Field::FirstLayerWallSpeed),
        show(Field::FirstLayerInfillSpeed),
        show(Field::FirstLayerHeight),
        show(Field::FirstLayerBed),
        show(Field::LayersBed),
        plan.derived.as_ref().unwrap().bed_window_c,
        show(Field::PartFanOffLayers),
        show(Field::AuxFanFirstLayers),
        show(Field::AuxFanMax),
        lines(plan, None).join(","),
        warnings.join(","),
    )
}

#[test]
fn every_material_on_every_plate_at_levels_0_1_and_2() {
    let mut table = String::new();
    for family in Family::ALL.iter().copied() {
        for plate in PlateType::ALL.iter().copied() {
            for level in 0..=2u8 {
                let mut inputs = common::inputs(family, plate, vec![common::calm()]);
                inputs.history = History::new(level, None).unwrap();
                let plan = plan(&inputs);
                table.push_str(&format!("{family} {plate} L{level}: {}\n", summary(&plan)));
            }
        }
    }
    snapshot("plans-materials-plates-levels.txt", &table);
}
