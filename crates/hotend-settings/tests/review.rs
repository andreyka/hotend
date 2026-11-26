//! Regression tests for the model, the rules, the envelopes and the proposals. Each test
//! states the plate that showed the defect.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{
    block, book, calm, everything, foot, inputs, person, plan, signature, token, value, whole,
    with_regions,
};
use hotend_settings::facts::{
    FitAtBed, FitSide, InnerLoop, OverhangRegion, PieceFacts, PlateFacts, Problem,
};
use hotend_settings::inputs::{Asker, History, PlanInputs, PrinterFacts, Setup, SlotFeed};
use hotend_settings::model::{
    ChecklistItem, Envelope, Family, Field, Intent, MaterialId, PlateType, PrintPlan, SpoolSlot,
    Value,
};
use hotend_settings::propose::{Note, Outcome};
use hotend_settings::{ProposalOutcome, RuleBook};
use serde_json::json;

fn pla(pieces: Vec<PieceFacts>) -> PlanInputs {
    inputs(Family::Pla, PlateType::TexturedPei, pieces)
}

fn allowed(plan: &PrintPlan, field: Field) -> Vec<Value> {
    match &plan.decision(field).unwrap().envelope {
        Envelope::Set { allowed } => allowed.clone(),
        other => panic!("{field} has no allowed set: {other:?}"),
    }
}

fn hang(regions: Vec<OverhangRegion>) -> PieceFacts {
    with_regions(calm(), regions)
}

fn fit_on_bed(mut piece: PieceFacts) -> PieceFacts {
    piece.fit_at_bed = vec![FitAtBed {
        pair: "axle".to_owned(),
        side: FitSide::Outside,
    }];
    piece
}

const ON: Value = Value::Flag(true);
const OFF: Value = Value::Flag(false);

// ---- Supports ----------------------------------------------------------------------------

/// A round horizontal hole a little wider than the limit of SUP-1 switches supports on, and
/// that is all it does: support inside a through hole or on a fit bore cannot be dug out, so
/// the person must be able to switch it off and to keep it off the part.
#[test]
fn a_hole_ceiling_switches_supports_on_and_never_locks_them() {
    for (area, diameter) in [(100.9, 9.0), (269.17, 9.0), (359.0, 12.0), (449.0, 20.0)] {
        let plan = plan(&pla(vec![hang(vec![OverhangRegion::hole_ceiling(
            area, diameter, true,
        )])]));
        assert_eq!(value(&plan, Field::SupportsEnabled), ON, "{diameter} mm");
        assert_eq!(
            allowed(&plan, Field::SupportsEnabled),
            [ON, OFF],
            "{diameter} mm, {area} mm2"
        );
        assert_eq!(token(&plan, Field::SupportsWhere), "everywhere");
        assert_eq!(
            allowed(&plan, Field::SupportsWhere),
            [Value::Token("everywhere"), Value::Token("bed_only")],
            "{diameter} mm, {area} mm2"
        );
        // Both changes are taken.
        let outcome = book()
            .propose_json(
                &pla(vec![hang(vec![OverhangRegion::hole_ceiling(
                    area, diameter, true,
                )])]),
                Asker::Person,
                &json!({"summary": "", "changes": [
                    {"field": "supports.enabled", "value": false, "reason": "a through hole"},
                    {"field": "supports.where", "value": "bed_only", "reason": "a through hole"},
                ]}),
            )
            .unwrap();
        assert_eq!(value(&outcome.plan, Field::SupportsEnabled), OFF);
        assert_eq!(token(&outcome.plan, Field::SupportsWhere), "bed_only");
    }
    // The control: a hole of 8 mm does not count at all.
    let small = plan(&pla(vec![hang(vec![OverhangRegion::hole_ceiling(
        269.17, 8.0, true,
    )])]));
    assert_eq!(value(&small, Field::SupportsEnabled), OFF);
    assert_eq!(allowed(&small, Field::SupportsEnabled), [OFF, ON]);

    // A ledge still forces supports, with or without a hole beside it, and a ledge above
    // the part still keeps them from standing on the bed only.
    let ledge = plan(&pla(vec![hang(vec![OverhangRegion::free(
        150.0, 12.0, true,
    )])]));
    assert_eq!(allowed(&ledge, Field::SupportsEnabled), [ON]);
    assert_eq!(
        allowed(&ledge, Field::SupportsWhere),
        [Value::Token("everywhere")]
    );
    let both = plan(&pla(vec![hang(vec![
        OverhangRegion::hole_ceiling(300.0, 12.0, true),
        OverhangRegion::free(150.0, 12.0, false),
    ])]));
    assert_eq!(allowed(&both, Field::SupportsEnabled), [ON]);
    assert_eq!(token(&both, Field::SupportsWhere), "everywhere");
    assert_eq!(
        allowed(&both, Field::SupportsWhere),
        [Value::Token("everywhere"), Value::Token("bed_only")]
    );
    // A hole and a ledge that only together pass the forced limit do not force.
    let together = plan(&pla(vec![hang(vec![
        OverhangRegion::hole_ceiling(300.0, 12.0, true),
        OverhangRegion::free(60.0, 6.0, false),
    ])]));
    assert_eq!(allowed(&together, Field::SupportsEnabled), [ON, OFF]);
}

/// A piece the engine could not classify keeps `off` allowed for its own overhang. It does
/// not unlock the supports another piece is known to need.
#[test]
fn supports_are_forced_by_the_classified_pieces_alone() {
    let ledge = || hang(vec![OverhangRegion::free(900.0, 30.0, false)]);
    let unknown = |area: f64| {
        let mut piece = calm();
        piece.overhang_mm2 = area;
        piece.overhang_over_part_mm2 = area;
        piece.overhang_regions = None;
        piece
    };
    assert_eq!(
        allowed(&plan(&pla(vec![ledge()])), Field::SupportsEnabled),
        [ON]
    );
    for area in [0.0, 150.0] {
        let mixed = pla(vec![ledge(), unknown(area)]);
        assert_eq!(
            allowed(&plan(&mixed), Field::SupportsEnabled),
            [ON],
            "{area}"
        );
        let outcome = book()
            .propose_json(
                &mixed,
                Asker::Agent,
                &json!({"summary": "", "changes": [
                    {"field": "supports.enabled", "value": false, "reason": "faster"},
                ]}),
            )
            .unwrap();
        assert_eq!(outcome.items[0].code, "not_allowed_here");
        assert_eq!(value(&outcome.plan, Field::SupportsEnabled), ON);
    }
    // Unclassified overhang alone, or beside a ledge that forces nothing, keeps `off`.
    assert_eq!(
        allowed(&plan(&pla(vec![unknown(5000.0)])), Field::SupportsEnabled),
        [ON, OFF]
    );
    let small = hang(vec![OverhangRegion::free(60.0, 6.0, false)]);
    assert_eq!(
        allowed(
            &plan(&pla(vec![small, unknown(5000.0)])),
            Field::SupportsEnabled
        ),
        [ON, OFF]
    );
}

/// SUP-3 sums what hangs over the part. The engine flags a region only when 4 mm2 or half of
/// it does, so several regions that each hang a little over the part carry no flag; the
/// piece's own total still says so.
#[test]
fn small_areas_over_the_part_are_summed_over_the_regions() {
    let mut piece = hang(vec![
        OverhangRegion::free(30.0, 6.0, false),
        OverhangRegion::free(30.0, 6.0, false),
        OverhangRegion::free(30.0, 6.0, false),
    ]);
    piece.overhang_over_part_mm2 = 9.0;
    let plan_of = |piece: &PieceFacts| plan(&pla(vec![piece.clone()]));
    assert_eq!(token(&plan_of(&piece), Field::SupportsWhere), "everywhere");
    assert_eq!(
        allowed(&plan_of(&piece), Field::SupportsWhere),
        [Value::Token("everywhere")]
    );
    piece.overhang_over_part_mm2 = 3.9;
    assert_eq!(token(&plan_of(&piece), Field::SupportsWhere), "bed_only");
    // What can lie in a region that is not counted proves nothing about the others.
    let mut piece = hang(vec![
        OverhangRegion::free(30.0, 6.0, false),
        OverhangRegion::hole_ceiling(125.0, 8.0, true),
    ]);
    assert_eq!(piece.overhang_over_part_mm2, 125.0);
    assert_eq!(token(&plan_of(&piece), Field::SupportsWhere), "bed_only");
    piece.overhang_over_part_mm2 = 130.0;
    assert_eq!(token(&plan_of(&piece), Field::SupportsWhere), "everywhere");
}

/// One overhanging area is not "1 overhanging areas".
#[test]
fn the_supports_sentence_counts_one_area_as_one() {
    let one = plan(&pla(vec![hang(vec![OverhangRegion::free(
        60.0, 30.0, false,
    )])]));
    let sentence = &one.decision(Field::SupportsEnabled).unwrap().sentence;
    assert!(
        sentence.contains("one overhanging area of about 60 mm2"),
        "{sentence}"
    );
    let two = plan(&pla(vec![hang(vec![
        OverhangRegion::free(60.0, 30.0, false),
        OverhangRegion::free(30.0, 30.0, false),
    ])]));
    let sentence = &two.decision(Field::SupportsEnabled).unwrap().sentence;
    assert!(
        sentence.contains("2 overhanging areas, the largest about 60 mm2"),
        "{sentence}"
    );
}

// ---- Bed temperatures --------------------------------------------------------------------

fn differs(plan: &PrintPlan) -> bool {
    plan.warnings
        .iter()
        .any(|warning| warning.code == "fit_signature_differs")
}

fn beds(plan: &PrintPlan) -> (i64, i64) {
    (
        whole(plan, Field::FirstLayerBed),
        whole(plan, Field::LayersBed),
    )
}

/// FIT-5: a product takes the bed pair of its gauge, but never a colder one than the card
/// gives the plate's own level, whatever raised that level. A gauge printed at a lower level
/// or on another plate must not cool the bed under a thin or slender piece.
#[test]
fn a_gauge_colder_than_the_level_never_cools_the_bed() {
    let calibrated = |piece: PieceFacts, bed: (i64, i64)| {
        let mut inputs = pla(vec![fit_on_bed(piece)]);
        inputs.intent = person(Intent::FitCritical);
        inputs.calibration = Some(signature(|parts| {
            parts.first_layer_bed_c = bed.0;
            parts.layers_bed_c = bed.1;
        }));
        plan(&inputs)
    };
    let alone = |piece: PieceFacts| {
        let mut inputs = pla(vec![fit_on_bed(piece)]);
        inputs.intent = person(Intent::FitCritical);
        plan(&inputs)
    };
    // (the piece, its level, the pair of an uncalibrated plan)
    let thin = foot(600.0, 600.0, 90.0, 5.0);
    let short_peg = foot(78.5, 31.4, 10.0, 8.0);
    let tall_peg = block(20.0, 20.0, 100.0);
    for (piece, level, pair) in [
        (thin, 1, (65, 55)),
        (short_peg, 1, (65, 55)),
        (tall_peg, 2, (65, 60)),
        (calm(), 0, (60, 55)),
    ] {
        let plain = alone(piece.clone());
        assert_eq!(whole(&plain, Field::Level), level);
        assert_eq!(beds(&plain), pair);
        for gauge in [(55, 55), (58, 55), (60, 55)] {
            let plan = calibrated(piece.clone(), gauge);
            assert_eq!(beds(&plan), pair, "level {level}, gauge {gauge:?}");
            // The fit is then not printed as calibrated, and the plan says so.
            assert_eq!(
                differs(&plan),
                gauge != pair,
                "level {level}, gauge {gauge:?}"
            );
            if gauge.0 < pair.0 {
                let decision = plan.decision(Field::FirstLayerBed).unwrap();
                assert_eq!(decision.rule.as_str(), "FIT-5");
                assert_eq!(decision.sentence_id, "bed.raised_over_calibration");
            }
        }
    }
    // A gauge that was hotter than the level's pair is still taken as it is.
    let plan = calibrated(calm(), (65, 55));
    assert_eq!(beds(&plan), (65, 55));
    assert!(!differs(&plan));
    assert_eq!(
        plan.decision(Field::FirstLayerBed).unwrap().sentence_id,
        "bed.calibration"
    );
}

fn printer_with_bed_limit(limit: i64) -> PrinterFacts {
    let feed = SlotFeed {
        slot: SpoolSlot::Ams(1),
        nozzle_id: 1,
        main: true,
    };
    PrinterFacts::new(0.4, 0.28, limit, vec![feed]).unwrap()
}

/// 5.8.2: a bed temperature lies inside the card's window and at or below the limit of the
/// printer and the filament. Where the limit lies below the window there is no such
/// temperature, and nothing is planned.
#[test]
fn a_bed_limit_below_the_cards_window_blocks_the_plan() {
    for (family, limit) in [
        (Family::Pla, 30),
        (Family::Pla, 40),
        (Family::Pla, 54),
        (Family::Petg, 64),
        (Family::Abs, 30),
        (Family::Abs, 65),
        (Family::Abs, 89),
        (Family::Asa, 89),
    ] {
        let mut inputs = inputs(family, PlateType::TexturedPei, vec![calm()]);
        inputs.printer = printer_with_bed_limit(limit);
        let plan = plan(&inputs);
        assert_eq!(
            plan.blocked.as_ref().map(|notice| notice.code),
            Some("bed_limit_below_window"),
            "{family} at {limit} C"
        );
        assert!(plan.settings.is_none() && plan.decisions.is_empty());
        let sentence = &plan.blocked.as_ref().unwrap().sentence;
        assert!(sentence.contains(&format!("{limit} C")), "{sentence}");
    }
    let mut gauge = common::gauge();
    gauge.printer = printer_with_bed_limit(30);
    assert_eq!(plan(&gauge).blocked.unwrap().code, "bed_limit_below_window");
    // A limit inside the window holds the bed down, with a warning, and the window the plan
    // states keeps its low end.
    for (family, limit, pair, window, warned) in [
        (Family::Pla, 55, (55, 55), [55, 55], true),
        (Family::Pla, 62, (60, 55), [55, 62], false),
        (Family::Abs, 95, (95, 95), [90, 95], true),
    ] {
        let mut inputs = inputs(family, PlateType::TexturedPei, vec![calm()]);
        inputs.printer = printer_with_bed_limit(limit);
        let plan = plan(&inputs);
        assert_eq!(beds(&plan), pair, "{family} at {limit} C");
        assert_eq!(plan.derived.as_ref().unwrap().bed_window_c, window);
        assert_eq!(
            plan.warnings
                .iter()
                .any(|warning| warning.code == "bed_limited"),
            warned,
            "{family} at {limit} C"
        );
        assert_eq!(book().verify(&inputs, &plan), Ok(()));
    }
}

// ---- Hard ranges and the data files --------------------------------------------------------

const RULES: &str = include_str!("../data/print-rules.toml");
const CARDS: &str = include_str!("../data/print-cards.toml");
const INTENTS: &str = include_str!("../data/intents.toml");
const SENTENCES: &str = include_str!("../data/sentences.toml");

/// A value a sliced file states is judged against the plan: the nozzle temperatures against
/// the family's range and the bed temperatures against the plate's window, not against the
/// widest range any family has.
#[test]
fn an_echoed_temperature_is_judged_against_the_family_and_the_plate() {
    let ok = |plan: &PrintPlan, field: Field, value: i64| {
        plan.check_echo(field, &Value::Whole(value)).is_ok()
    };
    let pla = plan(&pla(vec![calm()]));
    for field in [Field::FirstLayerNozzle, Field::LayersNozzle] {
        for value in [190, 220, 240] {
            assert!(ok(&pla, field, value), "{field} {value}");
        }
        for value in [170, 189, 241, 290, 300] {
            assert!(!ok(&pla, field, value), "{field} {value}");
        }
        assert!(pla.check_echo(field, &Value::Unset).is_err());
    }
    for field in [Field::FirstLayerBed, Field::LayersBed] {
        for value in [55, 60, 65] {
            assert!(ok(&pla, field, value), "{field} {value}");
        }
        for value in [30, 54, 66, 100] {
            assert!(!ok(&pla, field, value), "{field} {value}");
        }
    }
    let abs = plan(&inputs(Family::Abs, PlateType::TexturedPei, vec![calm()]));
    assert!(ok(&abs, Field::FirstLayerNozzle, 290));
    assert!(!ok(&abs, Field::FirstLayerNozzle, 220));
    assert!(ok(&abs, Field::FirstLayerBed, 100));
    assert!(!ok(&abs, Field::FirstLayerBed, 60));
    // A limit of the printer below the window's top narrows the range as well.
    let mut limited = inputs(Family::Pla, PlateType::TexturedPei, vec![calm()]);
    limited.printer = printer_with_bed_limit(62);
    let limited = plan(&limited);
    assert!(ok(&limited, Field::FirstLayerBed, 62));
    assert!(!ok(&limited, Field::FirstLayerBed, 63));
    // Every other field is judged against its constant range.
    assert!(ok(&pla, Field::BrimWidth, 15));
    assert!(!ok(&pla, Field::BrimWidth, 16));
    assert!(
        pla.check_echo(Field::BrimGap, &Value::decimal(0.4))
            .is_err()
            && pla.check_echo(Field::BrimGap, &Value::decimal(0.1)).is_ok()
    );
    // The family's range is stated in the plan.
    assert_eq!(pla.derived.as_ref().unwrap().nozzle_range_c, [190, 240]);
}

fn edited(file: &str, from: &str, to: &str) -> String {
    assert!(
        file.contains(from),
        "the data file no longer holds {from:?}"
    );
    file.replacen(from, to, 1)
}

/// What guards adhesion and the fit rests on constants, not on the data files alone: a data
/// file that asks for less than the constant floor fails to load.
#[test]
fn a_data_file_cannot_go_below_the_constant_floors() {
    let load = |rules: &str, cards: &str| {
        RuleBook::from_toml(rules, cards, INTENTS, SENTENCES)
            .map(|_| ())
            .map_err(|error| error.file)
    };
    assert_eq!(load(RULES, CARDS), Ok(()));
    for (from, to) in [
        // FIT-2: a required brim touches the part.
        ("required_mm = 0.0", "required_mm = 0.3"),
        ("required_mm = 0.0", "required_mm = 0.05"),
        ("min_elephant_foot_mm = 0.1", "min_elephant_foot_mm = 0.0"),
        // A brim rule that fires draws a brim.
        (
            "below_mm = 3.0\nwidth_mm = 5",
            "below_mm = 3.0\nwidth_mm = 0",
        ),
        (
            "below_mm2 = 300.0\nwidth_mm = 3",
            "below_mm2 = 300.0\nwidth_mm = 2",
        ),
        ("very_width_mm = 8", "very_width_mm = 2"),
        ("thin_or_tall_width_mm = 3", "thin_or_tall_width_mm = 0"),
        (
            "[brim.warping]\nwidth_mm = 5",
            "[brim.warping]\nwidth_mm = 1",
        ),
        // The rules keep firing where they fire today.
        (
            "below_mm = 3.0\nwidth_mm = 5",
            "below_mm = 0.5\nwidth_mm = 5",
        ),
        ("below_mm2 = 300.0", "below_mm2 = 20.0"),
        ("above = 2.0\nwidth_mm = 5", "above = 9.0\nwidth_mm = 5"),
        ("low_above_mm = 180.0", "low_above_mm = 900.0"),
        ("medium_above_mm = 120.0", "medium_above_mm = 900.0"),
        // Level 2 never brings less than level 1.
        ("brim_min_mm = 5", "brim_min_mm = 0"),
        ("brim_extra_mm = 3", "brim_extra_mm = 0"),
        ("fan_off_layers = 5", "fan_off_layers = 1"),
        (
            "first_layer_height_mm = 0.25",
            "first_layer_height_mm = 0.16",
        ),
        // MAT-1.
        ("loops = 3", "loops = 2"),
    ] {
        assert_eq!(
            load(&edited(RULES, from, to), CARDS),
            Err("print-rules.toml"),
            "{from} -> {to}"
        );
    }
    for (from, to) in [
        // A bed window below the family's constant range.
        ("window = [55, 65]", "window = [30, 65]"),
        ("window = [65, 80]", "window = [40, 80]"),
        ("window = [90, 100]", "window = [60, 100]"),
        ("window = [90, 100]", "window = [90, 120]"),
        // A card that delays the fan less, or opens the gap of a brim wider, than the floor.
        ("fan_off_layers = 3", "fan_off_layers = 1"),
    ] {
        assert_eq!(
            load(RULES, &edited(CARDS, from, to)),
            Err("print-cards.toml"),
            "{from} -> {to}"
        );
    }
}

// ---- The before-you-print list -----------------------------------------------------------

fn shown(plan: &PrintPlan, minutes: Option<u32>, project: bool) -> Vec<&'static str> {
    plan.checklist
        .lines(minutes, project)
        .iter()
        .map(|line| line.id)
        .collect()
}

/// The lines that are never dropped: the first three classes, and the lines
/// the fit, the history and level 3 rest on.
fn never_dropped(item: &ChecklistItem) -> bool {
    item.class <= 3
        || [
            "run_calibration",
            "few_pieces",
            "after.brim_fit",
            "after.flexible",
            "after.flexible_brim",
            "outcome",
        ]
        .contains(&item.id)
}

const TIMES: [(Option<u32>, bool); 6] = [
    (None, false),
    (None, true),
    (Some(14), false),
    (Some(14), true),
    (Some(90), false),
    (Some(90), true),
];

fn with_history(mut inputs: PlanInputs, level: u8) -> PlanInputs {
    inputs.history = History::new(level, None).unwrap();
    inputs
}

/// The line that says to scrape the brim off a mating face protects the fit, and the
/// question whether every piece stayed down feeds the history. Neither is ever dropped,
/// whatever the print time and whether the project is delivered or not.
#[test]
fn the_scrape_line_and_the_outcome_question_are_never_dropped() {
    let tall_fit_peg = pla(vec![fit_on_bed(block(10.0, 10.0, 80.0))]);
    let cool_gauge = {
        let mut gauge = common::gauge();
        gauge.setup = Setup::new(Some(PlateType::Cool), gauge.setup.spools().to_vec()).unwrap();
        gauge
    };
    for (name, inputs) in [
        ("gauge", common::gauge()),
        ("gauge after failures", with_history(common::gauge(), 3)),
        ("gauge on the Cool Plate", cool_gauge),
        ("tall fit peg", tall_fit_peg),
    ] {
        let plan = plan(&inputs);
        for (minutes, project) in TIMES {
            let ids = shown(&plan, minutes, project);
            assert!(
                ids.contains(&"after.brim_fit"),
                "{name} at {minutes:?}, project {project}: {ids:?}"
            );
            assert_eq!(ids.last(), Some(&"outcome"), "{name}: {ids:?}");
            assert_eq!(ids.contains(&"project"), project);
        }
    }
    // The gauge: its eight lines, in their order.
    assert_eq!(
        shown(&plan(&common::gauge()), Some(14), false),
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
    // An unproven material and tall pegs keep the question too.
    let abs_lid = inputs(
        Family::Abs,
        PlateType::TexturedPei,
        vec![block(200.0, 150.0, 4.0)],
    );
    let petg_peg = inputs(
        Family::Petg,
        PlateType::TexturedPei,
        vec![block(10.0, 10.0, 80.0)],
    );
    for inputs in [abs_lid, petg_peg, pla(vec![block(10.0, 10.0, 80.0)])] {
        let plan = plan(&inputs);
        for (minutes, project) in TIMES {
            let ids = shown(&plan, minutes, project);
            assert_eq!(ids.last(), Some(&"outcome"), "{ids:?}");
            // The line about bed levelling stays as well: it is what the first layer
            // rests on.
            assert!(ids.contains(&"bed_levelling"), "{ids:?}");
            assert!(ids.len() <= 8, "{ids:?}");
        }
    }
}

/// Over every test plate: the lines that are never dropped are shown, lines go from the
/// highest class first, and the list is longer than its limit only by lines that are never
/// dropped.
#[test]
fn the_list_drops_only_what_may_go() {
    let mut longest_rigid = 0;
    let mut longest_flexible = 0;
    for inputs in common::corpus()
        .into_iter()
        .map(|(_, inputs)| inputs)
        .chain(common::generated(400))
    {
        let plan = plan(&inputs);
        if plan.blocked.is_some() {
            continue;
        }
        let level = whole(&plan, Field::Level);
        let flexible = inputs.facts.family() == Family::Tpu;
        let base = if flexible { 10 } else { 8 };
        assert_eq!(
            plan.checklist.limit,
            if level >= 3 { base + 3 } else { base }
        );
        let afters = plan
            .checklist
            .items
            .iter()
            .filter(|item| item.id.starts_with("after."))
            .count();
        assert!(afters <= 1);
        for (minutes, project) in TIMES {
            let lines = plan.checklist.lines(minutes, project);
            let ids: Vec<&str> = lines.iter().map(|line| line.id).collect();
            let applies = |item: &&ChecklistItem| match item.id {
                "vent" => minutes.is_none_or(|minutes| minutes > 60),
                "project" => project,
                _ => true,
            };
            for item in plan.checklist.items.iter().filter(applies) {
                if never_dropped(item) {
                    assert!(ids.contains(&item.id), "{} was dropped: {ids:?}", item.id);
                }
            }
            assert_eq!(ids.last(), Some(&"outcome"));
            let kept = lines.iter().filter(|line| never_dropped(line)).count();
            assert!(lines.len() <= plan.checklist.limit.max(kept), "{ids:?}");
            // What went has a class at least as high as every line that stayed and may go.
            let lowest_dropped = plan
                .checklist
                .items
                .iter()
                .filter(applies)
                .filter(|item| !ids.contains(&item.id))
                .map(|item| item.class)
                .min();
            let highest_staying = lines
                .iter()
                .filter(|line| !never_dropped(line))
                .map(|line| line.class)
                .max();
            if let (Some(dropped), Some(staying)) = (lowest_dropped, highest_staying) {
                assert!(dropped >= staying, "{ids:?}");
            }
            if level < 3 {
                if flexible {
                    longest_flexible = longest_flexible.max(lines.len());
                } else {
                    longest_rigid = longest_rigid.max(lines.len());
                }
            }
        }
    }
    // Below level 3 the lines that are never dropped fit into the list:
    // eight lines, ten for a flexible material.
    assert_eq!((longest_rigid, longest_flexible), (8, 10));
}

/// MAT-2 asks for a list line on a crowded flexible plate, and a flexible list has room for
/// it, for the line about taking the part off and for the question.
#[test]
fn a_crowded_flexible_plate_shows_its_lines() {
    let five = inputs(
        Family::Tpu,
        PlateType::TexturedPei,
        vec![foot(1200.0, 480.0, 160.0, 12.0); 5],
    );
    let plan = plan(&five);
    for (minutes, project) in TIMES {
        let ids = shown(&plan, minutes, project);
        for id in [
            "few_pieces",
            "after.flexible",
            "outcome",
            "tpu.dry",
            "tpu.load",
        ] {
            assert!(ids.contains(&id), "{id} is missing: {ids:?}");
        }
        assert!(ids.len() <= 10, "{ids:?}");
    }
    // The sleeve, without the project: ten lines at most, every one
    // of them shown.
    let sleeve = common::plan(&common::sleeve());
    let ids = shown(&sleeve, None, false);
    assert_eq!(ids.len(), sleeve.checklist.items.len() - 1, "{ids:?}");
    assert!(ids.len() <= 10);
}

/// The hand-loading line is the vendor's for 85A and 90A. A harder grade does not get it; a
/// softer one and one whose hardness the id does not name do. The flow cap follows the same
/// grade.
#[test]
fn a_hardness_takes_the_nearest_softer_or_equal_grade() {
    for (id, soft) in [
        ("tpu-86a", true),
        ("tpu-88a", true),
        ("tpu-90a", true),
        ("tpu-generic", true),
        ("tpu-91a", false),
        ("tpu-95a", false),
        ("tpu-95a-hf", false),
    ] {
        let plan = plan(&common::inputs_with_material(common::sleeve(), id));
        assert!(plan.blocked.is_none(), "{id}: {:?}", plan.blocked);
        assert_eq!(
            value(&plan, Field::MaxFlowCap),
            if soft {
                Value::decimal(2.5)
            } else {
                Value::Unset
            },
            "{id}"
        );
        let ids = shown(&plan, None, false);
        assert_eq!(ids.contains(&"tpu.load"), soft, "{id}: {ids:?}");
        assert!(ids.contains(&"tpu.dry"), "{id}");
    }
    for id in ["tpu-85a", "tpu-80a", "tpu-085a", "tpu-185a"] {
        let plan = plan(&common::inputs_with_material(common::sleeve(), id));
        assert_eq!(
            plan.blocked.as_ref().map(|notice| notice.code),
            Some("material_not_printable"),
            "{id}"
        );
    }
}

/// The hardness an id names is read the same way as `python/hotend_fit/materials.py` reads
/// it; both sides can load this table.
#[test]
fn the_hardness_of_an_id_is_read_like_the_fit_library_reads_it() {
    let table: serde_json::Value =
        serde_json::from_str(include_str!("data/shore-table.json")).unwrap();
    let rows = table["rows"].as_array().unwrap();
    assert!(rows.len() >= 12);
    for row in rows {
        let id = row["id"].as_str().unwrap();
        let wanted = row["shore_a"].as_u64().map(|value| value as u8);
        assert_eq!(MaterialId::parse(id).unwrap().shore_a(), wanted, "{id}");
    }
}

/// Level 3 names every aid at the printer: on a plate where glue is allowed and not already
/// required, the list offers it.
#[test]
fn level_3_offers_glue_where_it_is_allowed() {
    let at = |family: Family, plate: PlateType, level: u8| {
        let plan = plan(&with_history(inputs(family, plate, vec![calm()]), level));
        shown(&plan, Some(14), true)
    };
    let pei = at(Family::Pla, PlateType::TexturedPei, 3);
    for id in [
        "plate.check",
        "glue.allowed",
        "run_calibration",
        "bed_levelling",
        "outcome",
    ] {
        assert!(pei.contains(&id), "{id} is missing: {pei:?}");
    }
    assert!(!at(Family::Pla, PlateType::TexturedPei, 2).contains(&"glue.allowed"));
    let cool = at(Family::Pla, PlateType::Cool, 3);
    assert!(cool.contains(&"glue.required") && !cool.contains(&"glue.allowed"));
    // No glue for a flexible filament where the card says so.
    let tpu = at(Family::Tpu, PlateType::TexturedPei, 3);
    assert!(
        tpu.contains(&"glue.none") && !tpu.contains(&"glue.allowed"),
        "{tpu:?}"
    );
}

/// The vendor's page on plates asks for glue on the Engineering Plate.
#[test]
fn pla_on_the_engineering_plate_gets_the_glue_line() {
    let plan = plan(&inputs(Family::Pla, PlateType::Engineering, vec![calm()]));
    let ids = shown(&plan, Some(14), false);
    assert!(ids.contains(&"glue.required"), "{ids:?}");
    assert!(ids.contains(&"plate.not_recommended"));
}

/// The level sentence names what the level changed for this material on this plate.
#[test]
fn the_level_sentence_says_what_changed() {
    let sentence = |inputs: &PlanInputs| {
        let plan = plan(inputs);
        assert_eq!(whole(&plan, Field::Level), 1);
        let decision = plan.decision(Field::Level).unwrap();
        (decision.sentence_id, decision.sentence.clone())
    };
    let small = || block(20.0, 10.0, 5.0);
    // PLA on Textured PEI: slower and hotter.
    let (id, text) = sentence(&pla(vec![small()]));
    assert_eq!(id, "level.1");
    assert!(text.contains("slower") && text.contains("hotter"), "{text}");
    // PETG on the High Temp Plate: the bed pair is the same at every level.
    let (id, text) = sentence(&inputs(Family::Petg, PlateType::HighTemp, vec![small()]));
    assert_eq!(id, "level.1.slower");
    assert!(
        text.contains("slower") && !text.contains("hotter"),
        "{text}"
    );
    // TPU prints its first layer at one speed at every level.
    let thin = || foot(600.0, 600.0, 90.0, 5.0);
    let (id, text) = sentence(&inputs(Family::Tpu, PlateType::TexturedPei, vec![thin()]));
    assert_eq!(id, "level.1.hotter");
    assert!(
        !text.contains("slower") && text.contains("hotter"),
        "{text}"
    );
    let (id, text) = sentence(&inputs(Family::Tpu, PlateType::HighTemp, vec![thin()]));
    assert_eq!(id, "level.1.same");
    assert!(
        !text.contains("slower") && !text.contains("hotter"),
        "{text}"
    );
}

/// The vent line is for the time after the first layers, and names the cover the vendor
/// names.
#[test]
fn the_vent_line_waits_for_the_first_layers() {
    let plan = plan(&pla(vec![calm()]));
    let lines = plan.checklist.lines(Some(90), false);
    let vent = lines.iter().find(|line| line.id == "vent").unwrap();
    assert!(
        vent.text.starts_with("Once the first layers are down"),
        "{}",
        vent.text
    );
    assert!(vent.text.contains("top glass cover") && !vent.text.contains("door"));
}

/// A flexible brim does not snap off. Where a flexible piece gets one, the list says how to
/// take it off, in place of the plain line about lifting the part.
#[test]
fn a_flexible_brim_comes_with_its_trimming_line() {
    let mut asked = common::sleeve();
    asked.level = person(2);
    let plan = plan(&asked);
    assert!(whole(&plan, Field::BrimWidth) >= 5);
    let ids = shown(&plan, None, true);
    assert!(ids.contains(&"after.flexible_brim"), "{ids:?}");
    assert!(!ids.contains(&"after.flexible") && !ids.contains(&"after.brim"));
    let plain = shown(&common::plan(&common::sleeve()), None, true);
    assert!(plain.contains(&"after.flexible") && !plain.contains(&"after.flexible_brim"));
}

// ---- Plate facts -------------------------------------------------------------------------

fn refused(pieces: Vec<PieceFacts>) -> Option<(&'static str, Problem)> {
    PlateFacts::new(Family::Pla, pieces)
        .err()
        .map(|error| (error.fact, error.problem))
}

/// Two wrong numbers that excuse each other are refused: every fact is held to what the
/// others allow at all, so a faulty engine cannot talk a thin ring out of its brim.
#[test]
fn facts_that_contradict_each_other_are_refused() {
    let ring = || {
        let mut ring = foot(456.0, 452.0, 74.0, 6.0);
        ring.strip_width_mm = 2.0;
        ring
    };
    assert_eq!(refused(vec![ring()]), None);
    let honest = plan(&pla(vec![ring()]));
    assert_eq!(whole(&honest, Field::BrimWidth), 5);
    assert_eq!(whole(&honest, Field::Level), 1);

    // No outline is shorter than the circle of the same area.
    for perimeter in [0.0, 0.001, 1.0, 70.0] {
        let mut lying = ring();
        lying.contact_perimeter_mm = perimeter;
        lying.strip_width_mm = 1000.0;
        assert_eq!(
            refused(vec![lying.clone()]),
            Some(("contact_perimeter_mm", Problem::Inconsistent)),
            "{perimeter}"
        );
        // Not even with a strip width that fits the false perimeter.
        lying.strip_width_mm = 2.0;
        assert!(refused(vec![lying]).is_some());
    }
    // No strip is wider than the radius of that circle.
    let mut lying = ring();
    lying.strip_width_mm = 500.0;
    assert_eq!(
        refused(vec![lying]),
        Some(("strip_width_mm", Problem::Inconsistent))
    );
    // A contact is not larger than the square on its longest side.
    let mut lying = block(200.0, 200.0, 5.0);
    lying.contact_span_mm = 0.0;
    assert_eq!(
        refused(vec![lying.clone()]),
        Some(("contact_span_mm", Problem::Inconsistent))
    );
    lying.contact_span_mm = 150.0;
    assert!(refused(vec![lying]).is_some());
    // A piece that stands on the bed has a height.
    let mut flat = calm();
    flat.height_mm = 0.0;
    flat.slenderness = 0.0;
    assert_eq!(
        refused(vec![flat]),
        Some(("height_mm", Problem::Inconsistent))
    );
    // A part of the overhang is not larger than the whole.
    let mut over = hang(vec![OverhangRegion::free(30.0, 6.0, false)]);
    over.fit_overhang_mm2 = 31.0;
    assert_eq!(
        refused(vec![over]),
        Some(("fit_overhang_mm2", Problem::Inconsistent))
    );
    let mut over = hang(vec![OverhangRegion::free(30.0, 6.0, false)]);
    over.overhang_over_part_mm2 = 31.0;
    assert_eq!(
        refused(vec![over]),
        Some(("overhang_over_part_mm2", Problem::Inconsistent))
    );
    // The facts the engine rounds stay inside the slack.
    let disc = foot(78.5, 31.4, 10.0, 8.0);
    assert_eq!(refused(vec![disc]), None);
    let mut nothing_flat = foot(0.0, 0.0, 0.0, 20.0);
    nothing_flat.strip_width_mm = 0.0;
    nothing_flat.slenderness = 1e4;
    assert_eq!(refused(vec![nothing_flat]), None);
}

/// The slenderness the rules use is never below height over the width of the footing,
/// however small the stated contact is.
#[test]
fn a_tiny_contact_cannot_hide_a_tall_piece() {
    let tall = |contact: f64| {
        let side = contact.sqrt();
        let mut piece = block(side, side, 1000.0);
        piece.slenderness = 0.0;
        piece
    };
    for contact in [0.25, 0.99, 1.0, 4.0] {
        let piece = tall(contact);
        assert!(piece.tipping() >= 500.0, "{contact}: {}", piece.tipping());
        let plan = plan(&pla(vec![piece]));
        assert_eq!(whole(&plan, Field::Level), 2, "{contact}");
        assert_eq!(whole(&plan, Field::BrimWidth), 10, "{contact}");
        assert_eq!(whole(&plan, Field::AccelCap), 4000, "{contact}");
        assert!(
            plan.warnings
                .iter()
                .any(|warning| warning.code == "turn_or_split"),
            "{contact}"
        );
    }
    let mut nothing_flat = foot(0.0, 0.0, 0.0, 20.0);
    nothing_flat.strip_width_mm = 0.0;
    nothing_flat.slenderness = 0.0;
    assert_eq!(nothing_flat.tipping(), 1e4);
}

fn facts_text(pieces: &str) -> String {
    format!(
        r#"{{"family": "PLA", "piece_count": 1, "total_contact_mm2": 2400.0, "pieces": [{pieces}]}}"#
    )
}

const PIECE: &str = r#"{"contact_mm2": 2400.0, "contact_perimeter_mm": 200.0,
    "strip_width_mm": 24.0, "contact_span_mm": 60.0, "height_mm": 20.0, "slenderness": 0.41,
    "overhang_mm2": 0.0, "fit_overhang_mm2": 0.0, "overhang_over_part_mm2": 0.0,
    "overhang_regions": [], "inner_loops": [], "fit_at_bed": [], "min_wall_mm": null}"#;

/// A key that is stated twice is refused: two readers of the same text must not be able to
/// read two plates.
#[test]
fn a_key_stated_twice_is_refused() {
    assert!(PlateFacts::from_json(&facts_text(PIECE)).is_ok());
    let twice = PIECE.replacen(
        r#""contact_mm2": 2400.0,"#,
        r#""contact_mm2": 50.0, "contact_mm2": 2400.0,"#,
        1,
    );
    assert_eq!(
        PlateFacts::from_json(&facts_text(&twice))
            .unwrap_err()
            .problem,
        Problem::Malformed
    );
    let family_twice = facts_text(PIECE).replacen(
        r#""family": "PLA","#,
        r#""family": "TPU", "family": "PLA","#,
        1,
    );
    assert_eq!(
        PlateFacts::from_json(&family_twice).unwrap_err().problem,
        Problem::Malformed
    );
}

/// An oversized answer is refused before it is read, and a list that is too long is
/// refused when its bound is passed, not after all of it was built.
#[test]
fn an_oversized_answer_is_refused_without_being_built() {
    use hotend_settings::facts::{MAX_ENTRIES, MAX_FACTS_BYTES, MAX_PIECES};
    // Longer than any plate can be.
    let mut long = facts_text(PIECE);
    long.push_str(&" ".repeat(MAX_FACTS_BYTES));
    let error = PlateFacts::from_json(&long).unwrap_err();
    assert_eq!((error.fact, error.problem), ("facts", Problem::OutOfBounds));
    // The reader stops at the bound: what follows it is never looked at, so it is the
    // count that is reported, not the junk behind it.
    let pieces = vec![PIECE; MAX_PIECES + 1].join(",") + r#", {"junk": 1}, 7"#;
    let error = PlateFacts::from_json(&facts_text(&pieces)).unwrap_err();
    assert_eq!((error.fact, error.problem), ("pieces", Problem::WrongCount));
    let loops = vec![r#"{"min_width_mm": 5.0, "fit": false}"#; MAX_ENTRIES + 1].join(",")
        + r#", {"junk": 1}"#;
    let piece = PIECE.replacen(
        r#""inner_loops": []"#,
        &format!(r#""inner_loops": [{loops}]"#),
        1,
    );
    let error = PlateFacts::from_json(&facts_text(&piece)).unwrap_err();
    assert_eq!(error.problem, Problem::WrongCount);
    // A plate as large as a plate can be is still read.
    let full = vec![PIECE; MAX_PIECES].join(",");
    let text = facts_text(&full)
        .replacen(
            r#""piece_count": 1"#,
            &format!(r#""piece_count": {MAX_PIECES}"#),
            1,
        )
        .replacen(
            r#""total_contact_mm2": 2400.0"#,
            &format!(r#""total_contact_mm2": {}"#, 2400.0 * MAX_PIECES as f64),
            1,
        );
    assert_eq!(
        PlateFacts::from_json(&text).unwrap().pieces().len(),
        MAX_PIECES
    );
}

/// The example files the engine's tests write are read as they are, and give a plan.
#[test]
fn the_engines_example_facts_are_accepted() {
    for (name, text) in [
        (
            "ring-gauge-facts.json",
            include_str!("../../../python/tests/engine/data/ring-gauge-facts.json"),
        ),
        (
            "overhang-kinds-facts.json",
            include_str!("../../../python/tests/engine/data/overhang-kinds-facts.json"),
        ),
    ] {
        let facts = PlateFacts::from_json(text).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(!facts.pieces().is_empty(), "{name}");
        let mut inputs = pla(vec![calm()]);
        assert_eq!(facts.family(), Family::Pla, "{name}");
        inputs.facts = facts;
        assert!(plan(&inputs).blocked.is_none(), "{name}");
    }
}

// ---- Proposals ----------------------------------------------------------------------------

fn propose(inputs: &PlanInputs, by: Asker, changes: serde_json::Value) -> ProposalOutcome {
    book()
        .propose_json(inputs, by, &json!({"summary": "", "changes": changes}))
        .unwrap()
}

/// What the person chose to make the print hold better stays until the person takes it
/// back: the agent can add to it, and cannot undo it.
#[test]
fn the_agent_cannot_take_back_what_the_person_made_safer() {
    let base = inputs(
        Family::Petg,
        PlateType::TexturedPei,
        vec![common::strip(2.0)],
    );
    let rule_plan = plan(&base);
    assert_eq!(whole(&rule_plan, Field::Level), 1);
    assert_eq!(whole(&rule_plan, Field::BrimWidth), 5);
    let safer = propose(
        &base,
        Asker::Person,
        json!([
            {"field": "adhesion.level", "value": 2, "reason": "it came loose"},
            {"field": "adhesion.brim_width_mm", "value": 10, "reason": "it came loose"},
            {"field": "first_layer.bed_c", "value": 80, "reason": "it came loose"},
            {"field": "first_layer.wall_speed_mm_s", "value": 10, "reason": "it came loose"},
            {"field": "cooling.aux_fan_max_pct", "value": 0, "reason": "it came loose"},
            {"field": "shell.infill_pct", "value": 30, "reason": "stiffer"},
        ]),
    );
    assert!(
        safer.items.iter().all(|item| item.code == "accepted"),
        "{:?}",
        safer.items
    );
    let fields = [
        Field::Level,
        Field::BrimWidth,
        Field::FirstLayerBed,
        Field::FirstLayerWallSpeed,
        Field::AuxFanMax,
    ];
    let chosen: Vec<Value> = fields
        .iter()
        .map(|field| value(&safer.plan, *field))
        .collect();
    assert_eq!(
        chosen,
        [
            Value::Whole(2),
            Value::Whole(10),
            Value::Whole(80),
            Value::Whole(10),
            Value::Whole(0)
        ]
    );

    // The agent asks for less of everything.
    let weaker = propose(
        &safer.inputs,
        Asker::Agent,
        json!([
            {"field": "adhesion.level", "value": 0, "reason": "faster"},
            {"field": "adhesion.brim_width_mm", "value": 0, "reason": "faster"},
            {"field": "first_layer.bed_c", "value": 65, "reason": "faster"},
            {"field": "first_layer.wall_speed_mm_s", "value": 50, "reason": "faster"},
            {"field": "cooling.aux_fan_max_pct", "value": 100, "reason": "faster"},
            {"field": "shell.infill_pct", "value": 20, "reason": "faster"},
        ]),
    );
    for item in &weaker.items[..5] {
        assert_eq!(
            (item.outcome, item.code, item.applied),
            (Outcome::Refused, "kept_by_person", None),
            "{:?}",
            item.field
        );
        assert!(!item.message.is_empty() && !item.message.contains('{'));
    }
    // A field that does not hold the print is still the newest's.
    assert_eq!(weaker.items[5].code, "accepted");
    assert_eq!(whole(&weaker.plan, Field::InfillPct), 20);
    let kept: Vec<Value> = fields
        .iter()
        .map(|field| value(&weaker.plan, *field))
        .collect();
    assert_eq!(kept, chosen);
    assert_eq!(weaker.inputs.level, safer.inputs.level);
    assert_eq!(weaker.inputs.level.unwrap().by, Asker::Person);
    for field in &fields[1..] {
        assert_eq!(weaker.inputs.changes.get(*field).unwrap().by, Asker::Person);
    }
    assert!(weaker.dropped.is_empty());
    assert_eq!(book().verify(&weaker.inputs, &weaker.plan), Ok(()));

    // The agent may ask for the same or for more, and the person may take it back.
    let more = propose(
        &safer.inputs,
        Asker::Agent,
        json!([
            {"field": "first_layer.wall_speed_mm_s", "value": 10, "reason": "the same"},
            {"field": "first_layer.infill_speed_mm_s", "value": 20, "reason": "more"},
        ]),
    );
    assert!(
        more.items.iter().all(|item| item.code == "accepted"),
        "{:?}",
        more.items
    );
    let back = propose(
        &safer.inputs,
        Asker::Person,
        json!([
            {"field": "adhesion.brim_width_mm", "value": 6, "reason": "too wide"},
            {"field": "adhesion.level", "value": 1, "reason": "enough"},
        ]),
    );
    assert!(back.items.iter().all(|item| item.code == "accepted"));
    assert_eq!(whole(&back.plan, Field::BrimWidth), 6);
    // What the agent chose itself, the agent may change again.
    let own = propose(
        &base,
        Asker::Agent,
        json!([{"field": "adhesion.brim_width_mm", "value": 9, "reason": "wider"}]),
    );
    let less = propose(
        &own.inputs,
        Asker::Agent,
        json!([{"field": "adhesion.brim_width_mm", "value": 7, "reason": "narrower"}]),
    );
    assert_eq!(less.items[0].code, "accepted");
    assert_eq!(whole(&less.plan, Field::BrimWidth), 7);
}

/// A note is one line of characters that can be seen. What cannot be seen, what belongs to
/// nobody and what stacks up on one letter does not get through.
#[test]
fn a_note_holds_only_what_can_be_seen() {
    let note = |text: &str| Note::new(text, 1, 240).map(|note| note.as_str().to_owned());
    // Kept as written.
    for text in [
        "wider brim, please",
        "Größe 5 mm – größer",
        "ブリムを広く",
        "é",
        "a\u{301}",
    ] {
        assert_eq!(note(text).as_deref(), Some(text), "{text:?}");
    }
    // Invisible, unassigned, private and filler characters become a space.
    for hidden in [
        '\u{E0049}',
        '\u{00AD}',
        '\u{061C}',
        '\u{180E}',
        '\u{FE0F}',
        '\u{E0100}',
        '\u{3164}',
        '\u{115F}',
        '\u{1160}',
        '\u{FFA0}',
        '\u{2800}',
        '\u{FFFE}',
        '\u{FFFF}',
        '\u{E000}',
        '\u{F0000}',
        '\u{0378}',
        '\u{202E}',
        '\u{2028}',
        '\u{FFF9}',
        '\u{200B}',
        '\u{034F}',
    ] {
        assert_eq!(
            note(&format!("a{hidden}b")).as_deref(),
            Some("a b"),
            "U+{:04X}",
            hidden as u32
        );
    }
    // Control characters refuse the note, as before.
    for control in ['\u{0085}', '\u{000B}', '\n', '\u{7}'] {
        assert_eq!(note(&format!("a{control}b")), None);
    }
    // At most four marks on one letter.
    let stacked = format!("a{}b", "\u{301}".repeat(238));
    assert_eq!(
        note(&stacked).as_deref(),
        Some("a\u{301}\u{301}\u{301}\u{301}b")
    );
    // A note of nothing visible is no reason.
    assert_eq!(note("\u{200B}\u{3164}"), None);
    assert_eq!(note("   "), None);
    assert!(Note::new("", 0, 240).is_some());
}

/// A call that is refused whole says nothing back of what was sent: no note, no summary.
#[test]
fn a_refused_call_carries_no_notes() {
    let base = pla(vec![calm()]);
    let outcome = book()
        .propose_json(
            &base,
            Asker::Agent,
            &json!({"summary": "a summary", "changes": [
                {"field": "adhesion.brim_width_mm", "value": 4, "reason": "a valid reason"},
                {"field": "no.such.field", "value": 4, "reason": "another reason"},
            ]}),
        )
        .unwrap();
    assert_eq!(outcome.refused.as_ref().unwrap().code, "invalid_arguments");
    assert_eq!(outcome.summary, None);
    assert!(outcome.items.iter().all(|item| item.note.is_none()));
    let said = serde_json::to_string(&outcome).unwrap();
    assert!(!said.contains("a valid reason") && !said.contains("a summary"));
    // A call that is answered keeps its notes.
    let outcome = propose(
        &base,
        Asker::Agent,
        json!([{"field": "adhesion.brim_width_mm", "value": 4, "reason": "a valid reason"}]),
    );
    assert_eq!(
        outcome.items[0].note.as_ref().map(Note::as_str),
        Some("a valid reason")
    );
}

/// 5.8.5: an inner brim is offered only when every hole on the plate has room for it.
#[test]
fn an_inner_brim_needs_room_in_every_hole() {
    let holed = |width: f64| {
        let mut piece = block(20.0, 10.0, 5.0);
        piece.inner_loops = vec![InnerLoop {
            min_width_mm: width,
            fit: false,
        }];
        piece
    };
    let kinds = |pieces: Vec<PieceFacts>| {
        let mut inputs = pla(pieces);
        inputs.capabilities = everything();
        allowed(&plan(&inputs), Field::BrimKind)
    };
    let both = Value::Token("outer_and_inner");
    assert!(kinds(vec![holed(40.0)]).contains(&both));
    assert!(kinds(vec![holed(40.0), holed(30.0)]).contains(&both));
    assert!(!kinds(vec![holed(40.0), holed(4.0)]).contains(&both));
    assert!(!kinds(vec![holed(4.0)]).contains(&both));
    // A piece without a hole does not stand in the way; a plate without any hole has
    // nothing to put an inner brim into.
    assert!(kinds(vec![holed(40.0), block(20.0, 10.0, 5.0)]).contains(&both));
    assert!(!kinds(vec![block(20.0, 10.0, 5.0)]).contains(&both));
}

// ---- The document ------------------------------------------------------------------------

/// Three codes of this crate need a sentence and a mapping of their own. The document for the
/// consumers of the crate therefore names every code a plan or an answer can carry.
#[test]
fn the_document_names_every_code() {
    use hotend_settings::explain::{ANSWERS, NOTICES};
    let document = include_str!("../../../docs/PRINT_SETTINGS.md");
    let named = |code: &str| document.contains(&format!("`{code}`"));
    for (code, _) in NOTICES {
        assert!(named(code), "docs/PRINT_SETTINGS.md does not name `{code}`");
    }
    // The codes of an answer: the ids without a dot, and the two that group several ids.
    for code in ANSWERS
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| !id.contains('.'))
        .chain(["invalid_arguments", "not_allowed_here"])
    {
        assert!(named(code), "docs/PRINT_SETTINGS.md does not name `{code}`");
    }
    // And the paragraph on the codes that need their own mapping names each of them.
    let at = document
        .find("Codes that need their own mapping")
        .expect("the paragraph on the additional codes");
    let paragraph = &document[at..at + 1600];
    for code in [
        "nozzle_size_unproven",
        "bed_limit_below_window",
        "plan_blocked",
        "kept_by_person",
    ] {
        assert!(paragraph.contains(&format!("`{code}`")), "{code}");
    }
}
