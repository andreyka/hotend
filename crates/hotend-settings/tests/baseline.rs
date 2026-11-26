//! The proven baseline: the sticking settings of a print the person reported as good are the
//! floor of a plan on the same printer, plate and material. The rules may go further, never
//! less far; without a baseline the rules decide alone.
//!
//! The plate is a ring-shaped test print of three pieces of PLA on the Textured PEI plate (the
//! sizes are invented); its baseline is the settings of a print that was reported good. Random
//! baselines on the generated plates check the properties; the checks recompute the floor here
//! instead of calling the library's.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Lcg, book, generated, inputs, plan, value, whole};
use hotend_settings::Changes;
use hotend_settings::bounds;
use hotend_settings::facts::PieceFacts;
use hotend_settings::inputs::{
    Asker, Baseline, BaselineValues, InputError, PlanInputs, PrinterFacts, SlotFeed,
};
use hotend_settings::model::{
    BrimKind, Day, Family, Field, MaterialId, PlateType, PrintPlan, RuleId, Source, SpoolSlot,
    Value,
};

fn bytes(plan: &PrintPlan) -> Vec<u8> {
    plan.sha256().unwrap().into_bytes()
}

/// One piece of the test print, as the engine states a piece.
fn piece(
    contact: f64,
    perimeter: f64,
    span: f64,
    height: f64,
    slender: f64,
    strip: f64,
    wall: f64,
) -> PieceFacts {
    PieceFacts {
        contact_mm2: contact,
        contact_perimeter_mm: perimeter,
        strip_width_mm: strip,
        contact_span_mm: span,
        height_mm: height,
        slenderness: slender,
        overhang_mm2: 0.0,
        fit_overhang_mm2: 0.0,
        overhang_over_part_mm2: 0.0,
        overhang_regions: Some(Vec::new()),
        inner_loops: Vec::new(),
        fit_at_bed: Vec::new(),
        min_wall_mm: Some(wall),
    }
}

/// The test print: a tall ring, a flat disc and a small post.
fn test_print() -> PlanInputs {
    inputs(
        Family::Pla,
        PlateType::TexturedPei,
        vec![
            piece(7854.0, 314.0, 100.0, 60.0, 0.677, 50.0, 1.6),
            piece(7088.0, 298.0, 95.0, 12.0, 0.1425, 47.5, 4.0),
            piece(314.0, 62.8, 20.0, 16.0, 0.903, 10.0, 8.0),
        ],
    )
}

/// The proven print: Textured PEI, Generic PLA, outer brim 5 mm, first layer 20 and 30
/// mm/s, bed 65 and 60 C, part fan off for 3 layers, auxiliary fan 0 on the first layer and
/// at most 30 percent, 3 walls, at level 1.
fn print_values() -> BaselineValues {
    BaselineValues {
        level: 1,
        brim_kind: BrimKind::Outer,
        brim_width_mm: 5,
        first_layer_wall_speed_mm_s: 20,
        first_layer_infill_speed_mm_s: 30,
        first_layer_bed_c: 65,
        layers_bed_c: 60,
        part_fan_off_layers: 3,
        aux_fan_first_layers_pct: 0,
        aux_fan_max_pct: 30,
        wall_loops: 3,
    }
}

fn day() -> Day {
    Day::parse("2026-11-17").unwrap()
}

fn print_baseline() -> Baseline {
    Baseline::new(
        PlateType::TexturedPei,
        MaterialId::parse("pla-generic").unwrap(),
        day(),
        print_values(),
    )
    .unwrap()
}

fn with(mut inputs: PlanInputs, baseline: Baseline) -> PlanInputs {
    inputs.baseline = Some(baseline);
    inputs
}

#[test]
fn the_test_print_by_the_rules_alone_is_level_0_without_a_brim() {
    let plan = plan(&test_print());
    assert_eq!(whole(&plan, Field::Level), 0);
    assert_eq!(value(&plan, Field::BrimKind), Value::Token("none"));
    assert_eq!(whole(&plan, Field::BrimWidth), 0);
    assert_eq!(value(&plan, Field::BrimGap), Value::Decimal(0.1));
    assert_eq!(whole(&plan, Field::FirstLayerWallSpeed), 30);
    assert_eq!(whole(&plan, Field::FirstLayerInfillSpeed), 50);
    assert_eq!(whole(&plan, Field::FirstLayerBed), 60);
    assert_eq!(whole(&plan, Field::LayersBed), 55);
    assert_eq!(whole(&plan, Field::AuxFanMax), 50);
    assert_eq!(whole(&plan, Field::AuxFanFirstLayers), 0);
    assert_eq!(whole(&plan, Field::PartFanOffLayers), 3);
    // The ring's wall of 1.6 mm asks for three loops (MAT-1).
    assert_eq!(whole(&plan, Field::WallLoops), 3);
    assert_eq!(plan.decision(Field::WallLoops).unwrap().rule, RuleId::Mat1);
    let derived = plan.derived.as_ref().unwrap();
    assert_eq!(derived.proven_on, None);
    assert!(derived.proven_fields.is_empty());
    // A plan without a baseline says nothing about one, not even an empty field.
    let text = serde_json::to_string(&plan).unwrap();
    assert!(!text.contains("proven"), "{text}");
}

#[test]
fn the_test_print_with_its_baseline_gets_exactly_the_proven_settings() {
    let inputs = with(test_print(), print_baseline());
    let plan = plan(&inputs);
    book().verify(&inputs, &plan).unwrap();
    let expect = [
        (Field::Level, Value::Whole(1)),
        (Field::BrimKind, Value::Token("outer")),
        (Field::BrimWidth, Value::Whole(5)),
        (Field::BrimGap, Value::Decimal(0.0)),
        (Field::FirstLayerWallSpeed, Value::Whole(20)),
        (Field::FirstLayerInfillSpeed, Value::Whole(30)),
        (Field::FirstLayerBed, Value::Whole(65)),
        (Field::LayersBed, Value::Whole(60)),
        (Field::PartFanOffLayers, Value::Whole(3)),
        (Field::AuxFanFirstLayers, Value::Whole(0)),
        (Field::AuxFanMax, Value::Whole(30)),
        (Field::WallLoops, Value::Whole(3)),
        (Field::PrimeTower, Value::Flag(false)),
        (Field::SupportsEnabled, Value::Flag(false)),
    ];
    for (field, wanted) in expect {
        assert_eq!(value(&plan, field), wanted, "{field}");
    }
    // Where the value came from: the proven print where the rules alone are less careful,
    // the rule where the rules alone give the same.
    for field in [
        Field::Level,
        Field::BrimKind,
        Field::BrimWidth,
        Field::FirstLayerWallSpeed,
        Field::FirstLayerInfillSpeed,
        Field::FirstLayerBed,
        Field::LayersBed,
        Field::AuxFanMax,
    ] {
        let decision = plan.decision(field).unwrap();
        assert_eq!(decision.source, Source::Proven, "{field}");
        assert_eq!(decision.rule, RuleId::Prv1, "{field}");
        assert_eq!(decision.basis.as_str(), "measured", "{field}");
        assert!(
            decision.sentence.contains("(2026-11-17)"),
            "{field}: {}",
            decision.sentence
        );
        assert!(!decision.sentence.contains("you chose"), "{field}");
        // The proven value is the rules' value of this plan: a change starts from it.
        assert_eq!(decision.rule_value, decision.value, "{field}");
        assert_eq!(decision.rule_source, Source::Proven, "{field}");
    }
    let gap = plan.decision(Field::BrimGap).unwrap();
    assert_eq!((gap.source, gap.rule), (Source::Rule, RuleId::Fit2));
    assert_eq!(plan.decision(Field::WallLoops).unwrap().rule, RuleId::Mat1);
    assert_eq!(
        plan.decision(Field::PartFanOffLayers).unwrap().rule,
        RuleId::Card
    );
    let derived = plan.derived.as_ref().unwrap();
    assert_eq!(derived.proven_on, Some(day()));
    assert!(derived.brim_required);
    assert_eq!(
        derived.proven_fields,
        vec![
            Field::Level,
            Field::BrimKind,
            Field::BrimWidth,
            Field::FirstLayerWallSpeed,
            Field::FirstLayerInfillSpeed,
            Field::FirstLayerBed,
            Field::LayersBed,
            Field::AuxFanMax,
        ]
    );
    assert!(plan.notes.iter().any(|note| note.code == "proven_baseline"));
    assert!(!plan.warnings.iter().any(|w| w.code == "proven_limited"));
    // The brim is drawn, so the list says how it comes off.
    assert!(
        plan.checklist
            .items
            .iter()
            .any(|item| item.id == "after.brim")
    );
    // The same inputs, the same plan.
    assert_eq!(bytes(&plan), bytes(&book().plan(&inputs).unwrap()));
    assert_ne!(bytes(&plan), bytes(&self::plan(&test_print())));
}

#[test]
fn a_baseline_of_another_plate_or_material_changes_nothing() {
    let alone = plan(&test_print());
    let other_plate = Baseline::new(
        PlateType::Cool,
        MaterialId::parse("pla-generic").unwrap(),
        day(),
        print_values(),
    )
    .unwrap();
    let other_material = Baseline::new(
        PlateType::TexturedPei,
        MaterialId::parse("pla-basic").unwrap(),
        day(),
        print_values(),
    )
    .unwrap();
    for baseline in [other_plate, other_material] {
        assert_eq!(bytes(&plan(&with(test_print(), baseline))), bytes(&alone));
    }
}

#[test]
fn the_rules_still_go_further_than_the_baseline() {
    // A tall, slender piece: level 2 with a wider brim, slower first layer and a longer fan
    // delay than the proven print.
    let tall = common::block(12.0, 12.0, 80.0);
    let inputs = with(
        common::inputs_with_material(
            inputs(Family::Pla, PlateType::TexturedPei, vec![tall]),
            "pla-generic",
        ),
        print_baseline(),
    );
    let plan = plan(&inputs);
    assert_eq!(whole(&plan, Field::Level), 2);
    assert!(whole(&plan, Field::BrimWidth) > 5);
    assert!(whole(&plan, Field::FirstLayerWallSpeed) < 20);
    assert!(whole(&plan, Field::FirstLayerInfillSpeed) < 30);
    assert!(whole(&plan, Field::PartFanOffLayers) > 3);
    for field in [
        Field::Level,
        Field::BrimWidth,
        Field::FirstLayerWallSpeed,
        Field::FirstLayerInfillSpeed,
        Field::PartFanOffLayers,
    ] {
        assert_ne!(
            plan.decision(field).unwrap().source,
            Source::Proven,
            "{field}"
        );
    }
    // The bed: the card's level 2 pair equals the proven one, so the rule says it.
    assert_eq!(whole(&plan, Field::FirstLayerBed), 65);
    assert_ne!(
        plan.decision(Field::FirstLayerBed).unwrap().source,
        Source::Proven
    );
}

#[test]
fn a_proven_value_beyond_what_the_plate_allows_is_held_there_and_said() {
    // A filament whose highest bed temperature is 62 C: the proven 65 C cannot be used.
    let mut inputs = with(test_print(), print_baseline());
    let feeds = vec![SlotFeed {
        slot: SpoolSlot::Ams(1),
        nozzle_id: 1,
        main: true,
    }];
    inputs.printer = PrinterFacts::new(0.4, 0.28, 62, feeds).unwrap();
    let plan = plan(&inputs);
    assert_eq!(whole(&plan, Field::FirstLayerBed), 62);
    assert_eq!(whole(&plan, Field::LayersBed), 60);
    let first = plan.decision(Field::FirstLayerBed).unwrap();
    assert_eq!(first.source, Source::Proven);
    assert_eq!(first.sentence_id, "proven.limited");
    assert!(
        first.sentence.contains("65") && first.sentence.contains("62"),
        "{}",
        first.sentence
    );
    assert!(plan.warnings.iter().any(|w| w.code == "proven_limited"));
    book().verify(&inputs, &plan).unwrap();
}

#[test]
fn no_change_can_take_a_plan_below_its_baseline() {
    let inputs = with(test_print(), print_baseline());
    let weak = [
        (Field::BrimWidth, Value::Whole(0)),
        (Field::BrimKind, Value::Token("none")),
        (Field::FirstLayerWallSpeed, Value::Whole(50)),
        (Field::FirstLayerInfillSpeed, Value::Whole(105)),
        (Field::FirstLayerBed, Value::Whole(55)),
        (Field::PartFanOffLayers, Value::Whole(1)),
        (Field::AuxFanMax, Value::Whole(100)),
        (Field::WallLoops, Value::Whole(2)),
    ];
    for by in [Asker::Person, Asker::Agent] {
        let mut changes = Changes::none();
        for (field, value) in weak {
            changes = changes.with(field, value, by).unwrap();
        }
        let mut asked = inputs.clone();
        asked.changes = changes;
        asked.level = Some(hotend_settings::inputs::Asked { value: 0, by });
        let plan = plan(&asked);
        book().verify(&asked, &plan).unwrap();
        assert_keeps_baseline(&asked, &plan, &format!("{by:?}"));
        assert_eq!(whole(&plan, Field::BrimWidth), 5);
        assert_eq!(whole(&plan, Field::WallLoops), 3);
    }
}

#[test]
fn a_baseline_refuses_values_hotend_does_not_accept() {
    let make = |change: fn(&mut BaselineValues)| {
        let mut values = print_values();
        change(&mut values);
        Baseline::new(
            PlateType::TexturedPei,
            MaterialId::parse("pla-generic").unwrap(),
            day(),
            values,
        )
    };
    let field = |result: Result<Baseline, InputError>| match result {
        Err(InputError::Baseline(field, _)) => field,
        other => panic!("{other:?}"),
    };
    assert_eq!(field(make(|v| v.level = 3)), Field::Level);
    assert_eq!(
        field(make(|v| v.brim_kind = BrimKind::Inner)),
        Field::BrimKind
    );
    assert_eq!(
        field(make(|v| v.brim_kind = BrimKind::OuterAndInner)),
        Field::BrimKind
    );
    assert_eq!(field(make(|v| v.brim_width_mm = 0)), Field::BrimWidth);
    assert_eq!(
        field(make(|v| v.brim_kind = BrimKind::None)),
        Field::BrimWidth
    );
    assert_eq!(field(make(|v| v.brim_width_mm = 16)), Field::BrimWidth);
    assert_eq!(
        field(make(|v| v.first_layer_wall_speed_mm_s = 9)),
        Field::FirstLayerWallSpeed
    );
    assert_eq!(
        field(make(|v| v.first_layer_bed_c = 121)),
        Field::FirstLayerBed
    );
    assert_eq!(field(make(|v| v.layers_bed_c = 66)), Field::LayersBed);
    assert_eq!(
        field(make(|v| v.part_fan_off_layers = 0)),
        Field::PartFanOffLayers
    );
    assert_eq!(
        field(make(|v| v.aux_fan_first_layers_pct = 41)),
        Field::AuxFanFirstLayers
    );
    assert_eq!(field(make(|v| v.aux_fan_max_pct = 101)), Field::AuxFanMax);
    assert_eq!(field(make(|v| v.wall_loops = 7)), Field::WallLoops);
    assert!(make(|_| {}).is_ok());
}

#[test]
fn a_day_is_a_real_calendar_day_written_as_digits() {
    assert_eq!(day().to_string(), "2026-11-17");
    assert_eq!(Day::parse("2028-02-29").unwrap().to_string(), "2028-02-29");
    for bad in [
        "2026-02-29",
        "2100-02-29",
        "2026-13-01",
        "2026-11-31",
        "2026-00-10",
        "2026-11-00",
        "1999-12-31",
        "2200-01-01",
        "2026-1-17",
        "2026/11/17",
        "2026-11-17 ",
        "+026-11-17",
        "2026-11-1x",
        "",
        "２０２６-11-17",
    ] {
        assert_eq!(Day::parse(bad), None, "{bad}");
    }
    assert!(Day::new(2000, 2, 29).is_some());
}

// ---- Properties over generated plates and random baselines ------------------------------

fn random_baseline(lcg: &mut Lcg, inputs: &PlanInputs) -> Baseline {
    let mut range =
        |low: i64, high: i64| low + ((lcg.next() * (high - low + 1) as f64) as i64).min(high - low);
    let width = range(0, 15);
    let first = range(30, 120);
    let values = BaselineValues {
        level: range(0, 2) as u8,
        brim_kind: if width > 0 {
            BrimKind::Outer
        } else {
            BrimKind::None
        },
        brim_width_mm: width,
        first_layer_wall_speed_mm_s: range(10, 50),
        first_layer_infill_speed_mm_s: range(15, 105),
        first_layer_bed_c: first,
        layers_bed_c: range(30, first),
        part_fan_off_layers: range(1, 10),
        aux_fan_first_layers_pct: range(0, 40),
        aux_fan_max_pct: range(0, 100),
        wall_loops: range(2, 6),
    };
    Baseline::new(
        inputs.setup.plate_type().unwrap(),
        inputs.material.clone(),
        day(),
        values,
    )
    .unwrap()
}

/// The value as it takes effect: the recorded first layer baseline where the plan leaves it
/// to the installed standard preset.
fn effective(plan: &PrintPlan, field: Field) -> Value {
    let stock = &book().data().baseline;
    match (field, value(plan, field)) {
        (Field::ElephantFoot, Value::Unset) => Value::decimal(stock.elephant_foot_mm),
        (Field::FirstLayerHeight, Value::Unset) => Value::decimal(stock.first_layer_height_mm),
        (Field::FirstLayerLineWidth, Value::Unset) => {
            Value::decimal(stock.first_layer_line_width_mm)
        }
        (Field::FirstLayerAccel, Value::Unset) => Value::Whole(stock.first_layer_accel_mm_s2),
        (_, other) => other,
    }
}

/// Whether `now` holds at least as well as `reference` for `field`, written out here.
fn at_least(field: Field, now: &Value, reference: &Value) -> bool {
    let up = matches!(
        field,
        Field::Level
            | Field::BrimWidth
            | Field::FirstLayerBed
            | Field::LayersBed
            | Field::PartFanOffLayers
            | Field::WallLoops
            | Field::FirstLayerHeight
    );
    let down = matches!(
        field,
        Field::BrimGap
            | Field::FirstLayerWallSpeed
            | Field::FirstLayerInfillSpeed
            | Field::FirstLayerAccel
            | Field::AuxFanFirstLayers
            | Field::AuxFanMax
    );
    if field == Field::BrimKind {
        return reference == &Value::Token("none")
            || now == reference
            || now == &Value::Token("outer_and_inner");
    }
    match (now.number(), reference.number()) {
        (Some(now), Some(reference)) if up => now >= reference - 1e-9,
        (Some(now), Some(reference)) if down => now <= reference + 1e-9,
        (_, None) if down => reference.is_unset(),
        _ => now == reference,
    }
}

/// Every field of the baseline holds at least as well as the baseline, held only to the top
/// of the bed window, the widest brim and the longest fan delay of the plate.
fn assert_keeps_baseline(inputs: &PlanInputs, plan: &PrintPlan, context: &str) {
    let baseline = inputs.baseline.as_ref().unwrap();
    let derived = plan.derived.as_ref().unwrap();
    let bed_top = derived.bed_window_c[1];
    let fan_max = book().data().level.fan_off_layers;
    for field in Baseline::FIELDS.iter().copied() {
        let wanted = baseline.get(field).unwrap();
        let floor = match (field, wanted.whole()) {
            (Field::BrimWidth, Some(width)) => Value::Whole(width.min(derived.brim_width_max_mm)),
            (Field::FirstLayerBed, Some(bed)) => Value::Whole(bed.min(bed_top)),
            (Field::LayersBed, Some(bed)) => {
                let first = baseline.get(Field::FirstLayerBed).unwrap().whole().unwrap();
                Value::Whole(bed.min(bed_top).min(first))
            }
            (Field::PartFanOffLayers, Some(layers)) => Value::Whole(layers.min(fan_max)),
            _ => wanted,
        };
        let now = value(plan, field);
        assert!(
            at_least(field, &now, &floor),
            "{context}: {field} is {now:?}, the proven floor {floor:?}"
        );
    }
}

#[test]
fn a_baseline_never_makes_a_plan_less_careful_and_is_always_kept() {
    let mut lcg = Lcg(0xBA5E_2026_1117);
    let mut proven_seen = 0;
    let mut rules_beyond = 0;
    let mut checked = 0;
    for (number, inputs) in generated(400).into_iter().enumerate() {
        let alone = plan(&inputs);
        if alone.settings.is_none() {
            continue;
        }
        for round in 0..3 {
            let context = format!("plate {number}, baseline {round}");
            let proven_inputs = with(inputs.clone(), random_baseline(&mut lcg, &inputs));
            let proven = plan(&proven_inputs);
            assert!(proven.settings.is_some(), "{context}: {:?}", proven.blocked);
            book()
                .verify(&proven_inputs, &proven)
                .unwrap_or_else(|v| panic!("{context}: {v:?}"));
            // Never less careful than the rules alone: every sticking field and the walls.
            for field in bounds::sticking_fields().chain([Field::WallLoops]) {
                let (now, rules) = (effective(&proven, field), effective(&alone, field));
                assert!(
                    at_least(field, &now, &rules),
                    "{context}: {field} is {now:?}, the rules alone {rules:?}"
                );
            }
            assert_keeps_baseline(&proven_inputs, &proven, &context);
            // The decisions say where a value came from: a proven value is the baseline's (or
            // held to the plate), and the rules' own value never says "proven".
            let derived = proven.derived.as_ref().unwrap();
            for decision in &proven.decisions {
                // FIT-5 raises the bed over a gauge's for the level, and names who set the
                // level: the proven print, here.
                if decision.source == Source::Proven
                    && decision.sentence_id != "bed.raised_over_calibration"
                {
                    proven_seen += 1;
                    assert!(
                        derived.proven_fields.contains(&decision.field),
                        "{context}: {}",
                        decision.field
                    );
                    assert!(
                        decision.sentence.contains("2026-11-17"),
                        "{context}: {}",
                        decision.sentence
                    );
                }
            }
            for field in Baseline::FIELDS.iter().copied() {
                let wanted = proven_inputs.baseline.as_ref().unwrap().get(field).unwrap();
                if !at_least(field, &wanted, &value(&proven, field)) {
                    rules_beyond += 1;
                }
            }
            // A pure function: again, the same bytes.
            assert_eq!(bytes(&proven), bytes(&plan(&proven_inputs)), "{context}");
            checked += 1;
        }
    }
    assert!(checked > 600, "{checked}");
    assert!(
        proven_seen > 1000 && rules_beyond > 500,
        "{proven_seen} proven, {rules_beyond} beyond"
    );
}

#[test]
fn proposals_on_a_plan_with_a_baseline_never_go_below_it() {
    let mut lcg = Lcg(0x0B5E_55ED);
    for (number, inputs) in generated(120).into_iter().enumerate() {
        if plan(&inputs).settings.is_none() {
            continue;
        }
        let mut current = with(inputs.clone(), random_baseline(&mut lcg, &inputs));
        for round in 0..4 {
            let context = format!("plate {number}, round {round}");
            let mut changes = Vec::new();
            for field in Baseline::FIELDS.iter().copied() {
                if lcg.next() < 0.5 {
                    continue;
                }
                let value = match field {
                    Field::Level => continue,
                    Field::BrimKind => serde_json::json!(
                        ["none", "outer", "inner", "outer_and_inner"]
                            [(lcg.next() * 4.0) as usize % 4]
                    ),
                    _ => {
                        let (low, high) = bounds::numeric_range(field).unwrap();
                        serde_json::json!((low + (high - low) * lcg.next()).round())
                    }
                };
                changes.push(serde_json::json!({"field": field.as_str(), "value": value, "reason": "try a weaker value"}));
            }
            if changes.is_empty() {
                continue;
            }
            let by = if lcg.next() < 0.5 {
                Asker::Person
            } else {
                Asker::Agent
            };
            let arguments = serde_json::json!({"changes": changes, "summary": "weaker"});
            let outcome = book().propose_json(&current, by, &arguments).unwrap();
            assert_eq!(outcome.inputs.baseline, current.baseline, "{context}");
            book().verify(&outcome.inputs, &outcome.plan).unwrap();
            assert_keeps_baseline(&outcome.inputs, &outcome.plan, &context);
            current = outcome.inputs;
        }
    }
}
