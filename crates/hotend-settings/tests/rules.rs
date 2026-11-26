//! The rules at their boundaries, the properties that hold for every plan, and the data
//! files (without the proposal properties, which are in `properties.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::{
    agent, block, book, calm, corpus, everything, foot, gauge, generated, inputs, person, plan,
    signature, token, value, whole, with_regions,
};
use hotend_settings::RuleBook;
use hotend_settings::bounds::{self, Kind};
use hotend_settings::explain::{DECISIONS, LINES, NOTICES};
use hotend_settings::facts::{FitAtBed, FitSide, InnerLoop, OverhangRegion, PieceFacts};
use hotend_settings::inputs::{History, PlanInputs, Setup, Spool};
use hotend_settings::model::{
    Envelope, Family, Field, Intent, PlateType, PrintPlan, ProcessPreset, Source, SpoolSlot, Value,
};

const RULES: &str = include_str!("../data/print-rules.toml");
const CARDS: &str = include_str!("../data/print-cards.toml");
const INTENTS: &str = include_str!("../data/intents.toml");
const SENTENCES: &str = include_str!("../data/sentences.toml");

fn pla(piece: PieceFacts) -> PlanInputs {
    inputs(Family::Pla, PlateType::TexturedPei, vec![piece])
}

fn allowed(plan: &PrintPlan, field: Field) -> Vec<Value> {
    match &plan.decision(field).unwrap().envelope {
        Envelope::Set { allowed } => allowed.clone(),
        other => panic!("{field} has no allowed set: {other:?}"),
    }
}

fn fired(plan: &PrintPlan) -> Vec<&'static str> {
    plan.derived
        .as_ref()
        .unwrap()
        .fired
        .iter()
        .map(|rule| rule.as_str())
        .collect()
}

/// A ring-like foot with exactly this strip width.
fn strip(width: f64) -> PieceFacts {
    foot(600.0, 1200.0 / width, 90.0, 5.0)
}

// ---- Brim rules and the level, at their boundaries ---------------------------------------

#[test]
fn ad1_thin_strip_fires_below_3_mm() {
    let thin = plan(&pla(strip(2.99)));
    assert_eq!(fired(&thin), ["AD-1"]);
    assert_eq!(
        (
            token(&thin, Field::BrimKind),
            whole(&thin, Field::BrimWidth),
            whole(&thin, Field::Level)
        ),
        ("outer", 5, 1)
    );
    assert_eq!(value(&thin, Field::BrimGap), Value::Decimal(0.0));
    // MAT-1 comes with it: a thin strip prints as walls only.
    assert_eq!(whole(&thin, Field::WallLoops), 3);
    assert_eq!(
        thin.decision(Field::WallLoops).unwrap().rule.as_str(),
        "MAT-1"
    );

    let wide = plan(&pla(strip(3.0)));
    assert!(fired(&wide).is_empty());
    assert_eq!(
        (
            token(&wide, Field::BrimKind),
            whole(&wide, Field::BrimWidth),
            whole(&wide, Field::Level)
        ),
        ("none", 0, 0)
    );
    assert_eq!(value(&wide, Field::BrimGap), Value::Decimal(0.1));
    assert_eq!(whole(&wide, Field::WallLoops), 2);
}

#[test]
fn ad2_small_foot_fires_below_300_and_below_100_mm2() {
    for (contact, width, level) in [(99.0, 5, 1), (100.0, 3, 1), (299.0, 3, 1), (300.0, 0, 0)] {
        // A square foot, low enough not to be slender.
        let side = f64::sqrt(contact);
        let plan = plan(&pla(foot(contact, 4.0 * side, side, 5.0)));
        assert_eq!(whole(&plan, Field::BrimWidth), width, "contact {contact}");
        assert_eq!(whole(&plan, Field::Level), level, "contact {contact}");
        assert_eq!(
            fired(&plan),
            if width > 0 { vec!["AD-2"] } else { Vec::new() },
            "contact {contact}"
        );
    }
}

#[test]
fn ad3_slenderness_fires_above_2_4_and_6() {
    // A foot of 20 by 20 mm: the slenderness is the height over 20.
    for (slenderness, width, level, gentle, warned) in [
        (2.0, 0, 0, false, false),
        (2.01, 5, 1, false, false),
        (4.0, 5, 1, false, false),
        (4.01, 10, 2, true, false),
        (6.0, 10, 2, true, false),
        (6.01, 10, 2, true, true),
    ] {
        let plan = plan(&pla(block(20.0, 20.0, 20.0 * slenderness)));
        assert_eq!(
            whole(&plan, Field::BrimWidth),
            width,
            "slenderness {slenderness}"
        );
        assert_eq!(
            whole(&plan, Field::Level),
            level,
            "slenderness {slenderness}"
        );
        let derived = plan.derived.as_ref().unwrap();
        assert_eq!(derived.gentle_motion, gentle, "slenderness {slenderness}");
        // 8 mm by the rule, then 3 mm wider at level 2, 10 at most.
        assert_eq!(
            derived.asked_brim_width_mm,
            if level == 2 { 8 } else { width }
        );
        assert_eq!(
            plan.warnings.iter().any(|w| w.code == "turn_or_split"),
            warned,
            "slenderness {slenderness}"
        );
        // SPD-2 comes with level 2 by slenderness.
        assert_eq!(
            value(&plan, Field::AccelCap),
            if gentle {
                Value::Whole(4000)
            } else {
                Value::Unset
            }
        );
        assert_eq!(
            value(&plan, Field::OuterWallAccelCap),
            if gentle {
                Value::Whole(2000)
            } else {
                Value::Unset
            }
        );
        assert_eq!(
            value(&plan, Field::OuterWallSpeedCap),
            if gentle {
                Value::Whole(100)
            } else {
                Value::Unset
            }
        );
    }
}

#[test]
fn ad4_long_part_fires_above_180_mm_for_low_and_120_mm_for_medium_warp() {
    for (family, span, width) in [
        (Family::Pla, 180.0, 0),
        (Family::Pla, 180.1, 3),
        (Family::Petg, 120.0, 0),
        (Family::Petg, 120.1, 5),
        // PLA is not held to the medium limit.
        (Family::Pla, 121.0, 0),
    ] {
        let plan = plan(&inputs(
            family,
            PlateType::TexturedPei,
            vec![block(span, 30.0, 5.0)],
        ));
        assert_eq!(whole(&plan, Field::BrimWidth), width, "{family} {span}");
        assert_eq!(
            whole(&plan, Field::Level),
            i64::from(width > 0),
            "{family} {span}"
        );
    }
}

#[test]
fn ad5_and_ad6_follow_the_material() {
    // Warp class high: 5 mm at least, on any geometry, with gap 0.
    for family in [Family::Abs, Family::Asa] {
        let plan = plan(&inputs(family, PlateType::TexturedPei, vec![calm()]));
        assert_eq!(fired(&plan), ["AD-5"]);
        assert_eq!(
            (whole(&plan, Field::BrimWidth), whole(&plan, Field::Level)),
            (5, 1)
        );
        assert_eq!(value(&plan, Field::BrimGap), Value::Decimal(0.0));
        assert_eq!(plan.derived.as_ref().unwrap().brim_width_max_mm, 15);
    }
    // Flexible: no brim; 3 mm when the strip is thin or the piece tall. The level is not
    // changed by AD-6, and the gap stays at the card's value.
    let tpu = |piece: PieceFacts| plan(&inputs(Family::Tpu, PlateType::TexturedPei, vec![piece]));
    let small = tpu(block(20.0, 10.0, 5.0));
    assert_eq!(fired(&small), ["AD-2", "AD-6"]);
    assert_eq!(
        (whole(&small, Field::BrimWidth), whole(&small, Field::Level)),
        (0, 1)
    );
    assert_eq!(whole(&small, Field::FirstLayerBed), 40);
    for piece in [strip(2.0), block(20.0, 20.0, 60.0)] {
        let plan = tpu(piece);
        assert_eq!(
            (
                token(&plan, Field::BrimKind),
                whole(&plan, Field::BrimWidth)
            ),
            ("outer", 3)
        );
        assert_eq!(value(&plan, Field::BrimGap), Value::Decimal(0.1));
        assert_eq!(
            plan.decision(Field::BrimWidth).unwrap().rule.as_str(),
            "AD-6"
        );
    }
}

#[test]
fn the_plate_takes_the_widest_brim_any_piece_asks_for() {
    let plan = plan(&inputs(
        Family::Pla,
        PlateType::TexturedPei,
        vec![calm(), block(20.0, 10.0, 5.0), strip(2.0)],
    ));
    assert_eq!(fired(&plan), ["AD-1", "AD-2"]);
    assert_eq!(whole(&plan, Field::BrimWidth), 5);
    assert_eq!(
        plan.decision(Field::BrimWidth).unwrap().rule.as_str(),
        "AD-1"
    );
    // The order of the pieces does not change the plan's values.
    let turned = common::plan(&inputs(
        Family::Pla,
        PlateType::TexturedPei,
        vec![strip(2.0), block(20.0, 10.0, 5.0), calm()],
    ));
    assert_eq!(plan.settings, turned.settings);
}

#[test]
fn the_level_is_raised_by_silk_history_and_a_request_and_never_lowered() {
    let silk = |mut inputs: PlanInputs| {
        let spool = Spool {
            silk: true,
            ..inputs.setup.spools()[0].clone()
        };
        inputs.setup = Setup::new(inputs.setup.plate_type(), vec![spool]).unwrap();
        inputs
    };
    assert_eq!(whole(&plan(&silk(pla(calm()))), Field::Level), 1);
    assert_eq!(whole(&plan(&silk(pla(strip(2.0)))), Field::Level), 2);
    // Silk raises to 2 at most.
    assert_eq!(
        whole(&plan(&silk(pla(block(20.0, 20.0, 100.0)))), Field::Level),
        2
    );

    for (min_level, expected) in [(0u8, 1), (1, 1), (2, 2), (3, 3)] {
        let mut inputs = pla(strip(2.0));
        inputs.history = History::new(min_level, None).unwrap();
        let plan = plan(&inputs);
        assert_eq!(whole(&plan, Field::Level), expected);
        if min_level > 1 {
            assert_eq!(plan.decision(Field::Level).unwrap().source, Source::History);
        }
    }
    // Level 3 changes no setting beyond level 2; it adds the note and two lines.
    let mut at_two = pla(calm());
    at_two.history = History::new(2, None).unwrap();
    let mut at_three = pla(calm());
    at_three.history = History::new(3, None).unwrap();
    let (two, three) = (plan(&at_two), plan(&at_three));
    for (field, value) in two
        .settings
        .as_ref()
        .unwrap()
        .iter()
        .filter(|(field, _)| *field != Field::Level)
    {
        assert_eq!(three.value(field), Some(value), "{field}");
    }
    assert!(three.notes.iter().any(|note| note.code == "level_3"));
    let lines: Vec<&str> = three
        .checklist
        .lines(None, false)
        .iter()
        .map(|line| line.id)
        .collect();
    assert!(lines.contains(&"plate.check") && lines.contains(&"run_calibration"));

    // A request raises the level to 2 at most and cannot lower it; the rules' level is kept
    // in the decision.
    let mut asked = pla(strip(2.0));
    asked.level = agent(2);
    let plan_two = plan(&asked);
    let decision = plan_two.decision(Field::Level).unwrap();
    assert_eq!(
        (decision.value, decision.rule_value, decision.source),
        (Value::Whole(2), Value::Whole(1), Source::Agent)
    );
    assert_eq!(decision.rule_rule.as_str(), "LVL-1");
    assert_eq!(whole(&plan_two, Field::BrimWidth), 8);
    for (wanted, got) in [(0u8, 1), (1, 1), (3, 2)] {
        let mut inputs = pla(strip(2.0));
        inputs.level = person(wanted);
        let plan = plan(&inputs);
        assert_eq!(whole(&plan, Field::Level), got);
        let decision = plan.decision(Field::Level).unwrap();
        assert_eq!(
            decision.asked,
            (i64::from(wanted) != got).then_some(Value::Whole(i64::from(wanted)))
        );
    }
}

// ---- Supports --------------------------------------------------------------------------

fn hang(regions: Vec<OverhangRegion>) -> PrintPlan {
    plan(&pla(with_regions(calm(), regions)))
}

#[test]
fn sup1_switches_at_4_20_and_100_mm2() {
    let on = Value::Flag(true);
    let off = Value::Flag(false);
    for (area, enabled, set) in [
        (3.9, false, vec![off]),
        (4.0, false, vec![off, on]),
        (19.9, false, vec![off, on]),
        (20.0, true, vec![on, off]),
        (100.0, true, vec![on, off]),
        (100.1, true, vec![on]),
    ] {
        let plan = hang(vec![OverhangRegion::free(area, 10.0, false)]);
        assert_eq!(
            value(&plan, Field::SupportsEnabled),
            Value::Flag(enabled),
            "area {area}"
        );
        assert_eq!(allowed(&plan, Field::SupportsEnabled), set, "area {area}");
        assert_eq!(
            plan.decision(Field::SupportsEnabled).unwrap().rule.as_str(),
            "SUP-1"
        );
    }
    // The limits for a flexible material are 50 and 200.
    for (area, enabled, set) in [
        (49.9, false, vec![off, on]),
        (50.0, true, vec![on, off]),
        (200.0, true, vec![on, off]),
        (200.1, true, vec![on]),
    ] {
        let piece = with_regions(
            foot(1200.0, 480.0, 160.0, 12.0),
            vec![OverhangRegion::free(area, 10.0, false)],
        );
        let plan = plan(&inputs(Family::Tpu, PlateType::TexturedPei, vec![piece]));
        assert_eq!(
            value(&plan, Field::SupportsEnabled),
            Value::Flag(enabled),
            "area {area}"
        );
        assert_eq!(allowed(&plan, Field::SupportsEnabled), set, "area {area}");
    }
}

#[test]
fn sup1_does_not_count_short_bridges_small_holes_and_short_reaches() {
    let counted = |region: OverhangRegion| hang(vec![region]).derived.as_ref().unwrap().support_mm2;
    // A bridge of 20 and of 21 mm.
    assert_eq!(counted(OverhangRegion::bridge(300.0, 20.0, false)), 0.0);
    assert_eq!(counted(OverhangRegion::bridge(300.0, 21.0, false)), 300.0);
    // A cross hole of 8 and of 9 mm.
    assert_eq!(counted(OverhangRegion::hole_ceiling(300.0, 8.0, true)), 0.0);
    assert_eq!(
        counted(OverhangRegion::hole_ceiling(300.0, 9.0, true)),
        300.0
    );
    // A bottom fillet of 2 mm and a reach of 2.1 mm.
    assert_eq!(counted(OverhangRegion::free(300.0, 2.0, false)), 0.0);
    assert_eq!(counted(OverhangRegion::free(300.0, 2.1, false)), 300.0);

    // What does not count never forces supports, but they may still be switched on.
    let plan = hang(vec![
        OverhangRegion::bridge(300.0, 20.0, true),
        OverhangRegion::hole_ceiling(125.0, 8.0, true),
    ]);
    assert_eq!(value(&plan, Field::SupportsEnabled), Value::Flag(false));
    assert_eq!(
        allowed(&plan, Field::SupportsEnabled),
        [Value::Flag(false), Value::Flag(true)]
    );
    // SUP-3 ignores them too: a cross hole alone never puts supports inside the part.
    assert_eq!(token(&plan, Field::SupportsWhere), "bed_only");
}

#[test]
fn unclassified_overhang_counts_in_full_and_keeps_off_allowed_at_every_area() {
    for area in [0.0, 3.0, 4.0, 20.0, 100.0, 101.0, 5000.0, 1.0e6] {
        let mut piece = calm();
        piece.overhang_mm2 = area;
        piece.overhang_regions = None;
        let plan = plan(&pla(piece));
        assert_eq!(plan.derived.as_ref().unwrap().support_mm2, area);
        assert_eq!(plan.derived.as_ref().unwrap().support_regions, None);
        assert_eq!(
            value(&plan, Field::SupportsEnabled),
            Value::Flag(area >= 20.0),
            "area {area}"
        );
        assert!(
            allowed(&plan, Field::SupportsEnabled).contains(&Value::Flag(false)),
            "area {area}"
        );
    }
}

#[test]
fn sup3_and_sup4_place_and_choose_the_supports() {
    // Counted overhang above the part itself: supports everywhere, and bed-only is gone.
    let over = hang(vec![OverhangRegion::free(60.0, 10.0, true)]);
    assert_eq!(token(&over, Field::SupportsWhere), "everywhere");
    assert_eq!(
        allowed(&over, Field::SupportsWhere),
        [Value::Token("everywhere")]
    );
    assert!(
        over.decision(Field::SupportsWhere)
            .unwrap()
            .sentence
            .ends_with("Some may be built inside holes.")
    );
    // Under 4 mm2 above the part: bed only.
    let little = hang(vec![
        OverhangRegion::free(60.0, 10.0, false),
        OverhangRegion::free(3.9, 10.0, true),
    ]);
    assert_eq!(token(&little, Field::SupportsWhere), "bed_only");
    assert_eq!(
        allowed(&little, Field::SupportsWhere),
        [Value::Token("everywhere"), Value::Token("bed_only")]
    );
    assert_eq!(whole(&over, Field::SupportsThreshold), 30);

    // Normal supports under a mating face, only when that value is verified.
    let mut piece = with_regions(calm(), vec![OverhangRegion::free(60.0, 10.0, false)]);
    piece.fit_overhang_mm2 = 4.0;
    let mut inputs = pla(piece.clone());
    assert_eq!(token(&plan(&inputs), Field::SupportsKind), "tree");
    assert_eq!(
        allowed(&plan(&inputs), Field::SupportsKind),
        [Value::Token("tree")]
    );
    inputs.capabilities = everything();
    assert_eq!(token(&plan(&inputs), Field::SupportsKind), "normal");
    piece.fit_overhang_mm2 = 3.9;
    let mut inputs = pla(piece);
    inputs.capabilities = everything();
    assert_eq!(token(&plan(&inputs), Field::SupportsKind), "tree");
    assert_eq!(
        allowed(&plan(&inputs), Field::SupportsKind),
        [Value::Token("tree"), Value::Token("normal")]
    );
}

// ---- Brim kind and gap -----------------------------------------------------------------

#[test]
fn an_inner_brim_needs_a_verified_value_a_wide_hole_and_no_bore_of_a_fit() {
    let holed = |width: f64, fit: bool| {
        let mut piece = calm();
        piece.inner_loops = vec![InnerLoop {
            min_width_mm: width,
            fit,
        }];
        piece
    };
    let all = [
        Value::Token("none"),
        Value::Token("outer"),
        Value::Token("inner"),
        Value::Token("outer_and_inner"),
    ];
    let plain = [Value::Token("none"), Value::Token("outer")];
    // Not verified: never offered.
    assert_eq!(
        allowed(&plan(&pla(holed(40.0, false))), Field::BrimKind),
        plain
    );
    let with = |piece: PieceFacts| {
        let mut inputs = pla(piece);
        inputs.capabilities = everything();
        plan(&inputs)
    };
    // No brim asked: the hole must be 10 mm wide.
    assert_eq!(allowed(&with(holed(10.0, false)), Field::BrimKind), all);
    assert_eq!(allowed(&with(holed(9.9, false)), Field::BrimKind), plain);
    // FIT-1: a bore of a declared fit takes the inner kinds off the plate.
    assert_eq!(allowed(&with(holed(40.0, true)), Field::BrimKind), plain);
    // A required brim of 5 mm: the hole must be 2 x 5 + 10 mm wide, `none` is gone, and
    // `inner` alone is never offered because it would take the outer brim away.
    let ring = |hole: f64| {
        let mut piece = strip(2.0);
        piece.inner_loops = vec![InnerLoop {
            min_width_mm: hole,
            fit: false,
        }];
        with(piece)
    };
    assert_eq!(
        allowed(&ring(20.0), Field::BrimKind),
        [Value::Token("outer"), Value::Token("outer_and_inner")]
    );
    assert_eq!(
        allowed(&ring(19.9), Field::BrimKind),
        [Value::Token("outer")]
    );
}

#[test]
fn a_required_brim_on_a_rigid_family_always_has_gap_0() {
    let mut seen = 0;
    for family in [Family::Pla, Family::Petg, Family::Abs, Family::Asa] {
        for plate in [
            PlateType::TexturedPei,
            PlateType::HighTemp,
            PlateType::Engineering,
        ] {
            for piece in [
                strip(2.0),
                block(9.0, 9.0, 5.0),
                block(20.0, 20.0, 100.0),
                block(200.0, 30.0, 5.0),
                calm(),
            ] {
                for level in 0..=3u8 {
                    let mut inputs = inputs(family, plate, vec![piece.clone()]);
                    inputs.history = History::new(level, None).unwrap();
                    let plan = plan(&inputs);
                    if plan.derived.as_ref().unwrap().brim_required {
                        seen += 1;
                        assert_eq!(
                            value(&plan, Field::BrimGap),
                            Value::Decimal(0.0),
                            "{family} {plate} L{level}"
                        );
                        assert!(!allowed(&plan, Field::BrimKind).contains(&Value::Token("none")));
                        assert_eq!(token(&plan, Field::BrimKind), "outer");
                    } else {
                        assert_eq!(whole(&plan, Field::BrimWidth), 0);
                    }
                }
            }
        }
    }
    assert!(seen > 100);
    // A calibrated elephant foot below 0.1 mm leaves too little overlap: the card's gap.
    let mut piece = strip(2.0);
    piece.fit_at_bed = vec![FitAtBed {
        pair: "axle".to_owned(),
        side: FitSide::Outside,
    }];
    let mut inputs = pla(piece);
    inputs.calibration = Some(signature(|parts| parts.elephant_foot_mm = 0.05));
    assert_eq!(value(&plan(&inputs), Field::BrimGap), Value::Decimal(0.1));
}

// ---- FIT-5: material, plate and bed -----------------------------------------------------

#[test]
fn a_plate_with_fit_faces_on_the_bed_heats_the_other_layers_only_at_level_2() {
    for family in Family::ALL.iter().copied() {
        for plate in [
            PlateType::TexturedPei,
            PlateType::HighTemp,
            PlateType::Engineering,
            PlateType::Cool,
        ] {
            let bed = |fit: bool, level: u8| {
                let mut piece = calm();
                if fit {
                    piece.fit_at_bed = vec![FitAtBed {
                        pair: "axle".to_owned(),
                        side: FitSide::Outside,
                    }];
                }
                let mut inputs = inputs(family, plate, vec![piece]);
                inputs.level = person(level);
                let plan = plan(&inputs);
                plan.blocked.is_none().then(|| {
                    (
                        whole(&plan, Field::Level),
                        whole(&plan, Field::FirstLayerBed),
                        whole(&plan, Field::LayersBed),
                    )
                })
            };
            let Some((_, _, normal_other)) = bed(false, 0) else {
                continue;
            };
            for asked in 0..=2u8 {
                let (level, first, other) = bed(true, asked).unwrap();
                let (_, plain_first, plain_other) = bed(false, asked).unwrap();
                // The first layer is heated as on any plate.
                assert_eq!(first, plain_first, "{family} {plate} L{level}");
                if level < 2 {
                    assert_eq!(other, normal_other, "{family} {plate} L{level}");
                } else {
                    assert_eq!(other, plain_other, "{family} {plate} L{level}");
                }
                assert!(other <= first);
            }
        }
    }
    // The measured case: PLA on Textured PEI, 65 then 55 with fit faces, 65 then 60 without.
    let mut fit = strip(2.0);
    fit.fit_at_bed = vec![FitAtBed {
        pair: "rim".to_owned(),
        side: FitSide::Outside,
    }];
    let with = plan(&pla(fit));
    assert_eq!(
        (
            whole(&with, Field::FirstLayerBed),
            whole(&with, Field::LayersBed)
        ),
        (65, 55)
    );
    let without = plan(&pla(strip(2.0)));
    assert_eq!(
        (
            whole(&without, Field::FirstLayerBed),
            whole(&without, Field::LayersBed)
        ),
        (65, 60)
    );
    // FIT-4: level 2 does not thicken the first layer under mating faces.
    let mut fit = calm();
    fit.fit_at_bed = vec![FitAtBed {
        pair: "rim".to_owned(),
        side: FitSide::Outside,
    }];
    let mut inputs = pla(fit);
    inputs.level = person(2);
    assert_eq!(value(&plan(&inputs), Field::FirstLayerHeight), Value::Unset);
    let mut inputs = pla(calm());
    inputs.level = person(2);
    assert_eq!(
        value(&plan(&inputs), Field::FirstLayerHeight),
        Value::Decimal(0.25)
    );
}

#[test]
fn the_bed_never_passes_the_limit_of_the_printer_and_the_filament() {
    let limited = corpus()
        .into_iter()
        .find(|(name, _)| name == "bed-limited")
        .unwrap()
        .1;
    let plan = plan(&limited);
    // The card asks for 75 then 70 at level 1; the limit is 72.
    assert_eq!(
        (
            whole(&plan, Field::FirstLayerBed),
            whole(&plan, Field::LayersBed)
        ),
        (72, 70)
    );
    assert_eq!(
        plan.decision(Field::FirstLayerBed).unwrap().rule.as_str(),
        "PLT-2"
    );
    assert_eq!(plan.derived.as_ref().unwrap().bed_window_c, [65, 72]);
    assert!(
        plan.warnings
            .iter()
            .any(|warning| warning.code == "bed_limited")
    );
}

#[test]
fn vnt1_carries_the_venting_line_with_its_condition() {
    let vent = |family: Family, plate: PlateType, level: u8| {
        let mut inputs = inputs(family, plate, vec![calm()]);
        inputs.history = History::new(level, None).unwrap();
        let plan = plan(&inputs);
        let long = plan
            .checklist
            .lines(Some(61), false)
            .iter()
            .any(|line| line.id == "vent");
        let short = plan
            .checklist
            .lines(Some(60), false)
            .iter()
            .any(|line| line.id == "vent");
        assert!(
            !short,
            "{family} {plate}: a print of an hour needs no venting"
        );
        long
    };
    // PLA above 45 C, PETG above 70 C, TPU above 45 C, never ABS or ASA.
    assert!(vent(Family::Pla, PlateType::TexturedPei, 0));
    assert!(!vent(Family::Pla, PlateType::Cool, 1));
    assert!(!vent(Family::Petg, PlateType::TexturedPei, 0));
    assert!(vent(Family::Petg, PlateType::TexturedPei, 1));
    assert!(!vent(Family::Tpu, PlateType::TexturedPei, 1));
    assert!(!vent(Family::Abs, PlateType::TexturedPei, 2));
    assert!(!vent(Family::Asa, PlateType::HighTemp, 2));
}

// ---- Intents and STK-1 -----------------------------------------------------------------

/// The sticking values a sliced file must state: what the plan writes, joined with the
/// baseline the proof demands for what it leaves unwritten.
fn effective_sticking(plan: &PrintPlan) -> Vec<(Field, Value)> {
    let baseline = &book().data().baseline;
    bounds::sticking_fields()
        .map(|field| {
            let written = value(plan, field);
            let effective = match (field, written) {
                (Field::ElephantFoot, Value::Unset) => Value::decimal(baseline.elephant_foot_mm),
                (Field::FirstLayerHeight, Value::Unset) => {
                    Value::decimal(baseline.first_layer_height_mm)
                }
                (Field::FirstLayerLineWidth, Value::Unset) => {
                    Value::decimal(baseline.first_layer_line_width_mm)
                }
                (Field::FirstLayerAccel, Value::Unset) => {
                    Value::Whole(baseline.first_layer_accel_mm_s2)
                }
                (_, other) => other,
            };
            (field, effective)
        })
        .collect()
}

#[test]
fn no_intent_moves_a_sticking_value_in_the_model_or_in_what_the_file_states() {
    let plates: Vec<PlanInputs> = vec![
        pla(calm()),
        pla(strip(2.0)),
        inputs(
            Family::Petg,
            PlateType::TexturedPei,
            vec![block(12.0, 12.0, 150.0)],
        ),
        inputs(Family::Abs, PlateType::HighTemp, vec![calm()]),
        inputs(
            Family::Tpu,
            PlateType::TexturedPei,
            vec![foot(1200.0, 480.0, 160.0, 12.0)],
        ),
    ];
    for base in plates {
        for caps in [common::measured(), everything()] {
            let with = |intent: Intent| {
                let mut inputs = base.clone();
                inputs.intent = agent(intent);
                inputs.capabilities = caps.clone();
                plan(&inputs)
            };
            let balanced = effective_sticking(&with(Intent::Balanced));
            for intent in Intent::ALL.iter().copied() {
                let plan = with(intent);
                assert_eq!(effective_sticking(&plan), balanced, "{intent}");
                // STK-1: with another preset the four baseline values are written.
                let stock = token(&plan, Field::Process) == "standard-0.20mm";
                for field in [
                    Field::ElephantFoot,
                    Field::FirstLayerHeight,
                    Field::FirstLayerLineWidth,
                    Field::FirstLayerAccel,
                ] {
                    let decision = plan.decision(field).unwrap();
                    if decision.rule.as_str() == "STK-1" {
                        assert_eq!(decision.value.is_unset(), stock, "{intent} {field}");
                    }
                }
            }
        }
    }
}

#[test]
fn the_intent_table_and_its_fallbacks() {
    let with = |intent: Intent, caps| {
        let mut inputs = pla(calm());
        inputs.intent = person(intent);
        inputs.capabilities = caps;
        plan(&inputs)
    };
    let shell = |plan: &PrintPlan| {
        (
            token(plan, Field::Process),
            whole(plan, Field::WallLoops),
            whole(plan, Field::InfillPct),
            token(plan, Field::InfillPattern),
            token(plan, Field::Seam),
        )
    };
    let all = everything();
    assert_eq!(
        shell(&with(Intent::Balanced, all.clone())),
        ("standard-0.20mm", 2, 15, "grid", "aligned")
    );
    assert_eq!(
        shell(&with(Intent::Strong, all.clone())),
        ("standard-0.20mm", 5, 25, "gyroid", "aligned")
    );
    assert_eq!(
        shell(&with(Intent::BestSurface, all.clone())),
        ("fine-0.12mm", 3, 15, "gyroid", "back")
    );
    assert_eq!(
        shell(&with(Intent::Fast, all.clone())),
        ("draft-0.24mm", 2, 10, "grid", "aligned")
    );
    assert_eq!(
        shell(&with(Intent::FitCritical, all.clone())),
        ("standard-0.20mm", 3, 15, "grid", "aligned")
    );
    let surface = with(Intent::BestSurface, all.clone());
    assert_eq!(
        (
            whole(&surface, Field::OuterWallSpeedCap),
            whole(&surface, Field::OuterWallAccelCap)
        ),
        (60, 2000)
    );
    assert!(surface.warnings.is_empty());
    assert!(
        surface
            .checklist
            .items
            .iter()
            .any(|item| item.id == "calibration")
    );
    assert_eq!(
        surface.decision(Field::Intent).unwrap().source,
        Source::Person
    );
    assert_eq!(
        surface.decision(Field::Intent).unwrap().rule_value,
        Value::Token("balanced")
    );

    // Not installed, not verified: the balanced value with a warning, never a failure.
    let today = common::measured();
    let surface = with(Intent::BestSurface, today.clone());
    assert_eq!(
        shell(&surface),
        ("standard-0.20mm", 3, 15, "grid", "aligned")
    );
    assert!(
        surface
            .warnings
            .iter()
            .any(|warning| warning.code == "intent_value_unavailable")
    );
    assert_eq!(
        surface.decision(Field::Process).unwrap().rule.as_str(),
        "INT-2"
    );
    assert_eq!(
        shell(&with(Intent::Fast, today.clone())).0,
        "standard-0.20mm"
    );
    assert_eq!(
        shell(&with(Intent::Strong, today)),
        ("standard-0.20mm", 5, 25, "grid", "aligned")
    );
    // A value that is not verified is not offered either.
    assert_eq!(
        allowed(&surface, Field::InfillPattern),
        [Value::Token("grid")]
    );
    assert_eq!(
        allowed(&with(Intent::Balanced, all), Field::Seam),
        [Value::Token("aligned"), Value::Token("back")]
    );

    // A gauge is always fit-critical, whatever is asked.
    let mut inputs = gauge();
    inputs.intent = agent(Intent::Fast);
    let plan = plan(&inputs);
    assert_eq!(token(&plan, Field::Intent), "fit_critical");
    assert_eq!(
        plan.decision(Field::Intent).unwrap().asked,
        Some(Value::Token("fast"))
    );
    // Rules can only raise what an intent gives: MAT-1 lifts the balanced 2 walls to 3.
    let mut thin = calm();
    thin.min_wall_mm = Some(2.4);
    assert_eq!(whole(&plan_of(pla(thin.clone())), Field::WallLoops), 3);
    thin.min_wall_mm = Some(2.5);
    assert_eq!(whole(&plan_of(pla(thin)), Field::WallLoops), 2);
}

fn plan_of(inputs: PlanInputs) -> PrintPlan {
    plan(&inputs)
}

// ---- The fit signature ------------------------------------------------------------------

#[test]
fn a_product_is_printed_like_its_gauge_and_warned_when_it_is_not() {
    let product = |fit: bool| {
        let mut piece = calm();
        if fit {
            piece.fit_at_bed = vec![FitAtBed {
                pair: "axle".to_owned(),
                side: FitSide::Outside,
            }];
        }
        let mut inputs = pla(piece);
        inputs.intent = person(Intent::FitCritical);
        inputs
    };
    let differs = |plan: &PrintPlan| {
        plan.warnings
            .iter()
            .any(|warning| warning.code == "fit_signature_differs")
    };

    // The gauge's own values: the three calibrated fields are set, the bed pair is taken,
    // and there is no warning.
    let mut inputs = product(true);
    inputs.calibration = Some(common::gauge_signature());
    let plan = plan(&inputs);
    for (field, expected) in [
        (Field::ElephantFoot, 0.15),
        (Field::FirstLayerHeight, 0.2),
        (Field::FirstLayerLineWidth, 0.5),
    ] {
        let decision = plan.decision(field).unwrap();
        assert_eq!(
            (decision.value, decision.source),
            (Value::Decimal(expected), Source::Calibration)
        );
        assert_eq!(decision.rule.as_str(), "SIG-1");
    }
    // The product alone would be level 0 (60 then 55); it takes the gauge's 65 then 55.
    assert_eq!(
        (
            whole(&plan, Field::Level),
            whole(&plan, Field::FirstLayerBed),
            whole(&plan, Field::LayersBed)
        ),
        (0, 65, 55)
    );
    assert_eq!(
        plan.decision(Field::FirstLayerBed).unwrap().source,
        Source::Calibration
    );
    assert!(!differs(&plan));
    assert!(plan.derived.as_ref().unwrap().calibrated);
    // The fit-critical intent reproduces the gauge's signature. This product needs no brim,
    // so its brim gap is the card's and acts on nothing; every other part is the gauge's.
    assert_eq!(token(&plan, Field::BrimKind), "none");
    let expected = book().expected_signature(&plan).unwrap();
    let mut parts = expected.parts().clone();
    parts.brim_gap_mm = common::gauge_signature().parts().brim_gap_mm;
    assert_eq!(
        hotend_settings::signature::FitSignature::new(parts).unwrap(),
        common::gauge_signature()
    );

    // Without fit faces on the bed the bed pair is the card's.
    let mut inputs = product(false);
    inputs.calibration = Some(common::gauge_signature());
    let plan_without = common::plan(&inputs);
    assert_eq!(
        (
            whole(&plan_without, Field::FirstLayerBed),
            whole(&plan_without, Field::LayersBed)
        ),
        (60, 55)
    );
    // Both beds are within 5 C of the gauge's, so there is no warning.
    assert!(!differs(&plan_without));

    // Another preset, layer height or outer wall cap, or a bed more than 5 C off: warned.
    let mut inputs = product(true);
    inputs.intent = person(Intent::Balanced);
    inputs.calibration = Some(common::gauge_signature());
    assert!(
        differs(&common::plan(&inputs)),
        "the balanced intent has no outer wall cap"
    );
    for change in [
        (|parts: &mut hotend_settings::signature::SignatureParts| {
            parts.process = ProcessPreset::Fine012
        }) as fn(&mut _),
        |parts| parts.layer_height_mm = 0.16,
        |parts| parts.outer_wall_cap_mm_s = Some(80),
    ] {
        let mut inputs = product(true);
        inputs.calibration = Some(signature(change));
        assert!(differs(&common::plan(&inputs)));
    }
    // History may still raise the bed; then the warning is shown. (A gauge colder than the
    // card's pair of the level never cools the bed: tests/review.rs.)
    let mut inputs = product(true);
    inputs.calibration = Some(signature(|parts| {
        parts.first_layer_bed_c = 62;
        parts.layers_bed_c = 55;
    }));
    assert_eq!(whole(&common::plan(&inputs), Field::FirstLayerBed), 62);
    inputs.history = History::new(2, None).unwrap();
    let raised = common::plan(&inputs);
    assert_eq!(
        (
            whole(&raised, Field::FirstLayerBed),
            whole(&raised, Field::LayersBed)
        ),
        (65, 60)
    );
    assert_eq!(
        raised.decision(Field::FirstLayerBed).unwrap().source,
        Source::History
    );
    assert!(differs(&raised));
    // FIT-4: the calibrated fields stay the gauge's even at level 2.
    assert_eq!(value(&raised, Field::FirstLayerHeight), Value::Decimal(0.2));

    // A signature for another material or nozzle is not this product's calibration.
    let mut inputs = product(true);
    inputs.calibration = Some(signature(|parts| parts.nozzle_id = 0));
    let other = common::plan(&inputs);
    assert_eq!(value(&other, Field::ElephantFoot), Value::Unset);
    assert!(!other.derived.as_ref().unwrap().calibrated);
    // A gauge takes no calibration: it defines one.
    let mut inputs = gauge();
    inputs.calibration = Some(signature(|parts| parts.elephant_foot_mm = 0.1));
    assert_eq!(
        value(&common::plan(&inputs), Field::ElephantFoot),
        Value::Unset
    );
    // Recorded first layer values that this nozzle cannot print are not used, and
    // the plan says that the fit is not printed as calibrated.
    let unusable = corpus()
        .into_iter()
        .find(|(name, _)| name == "calibrated-values-this-nozzle-cannot-print")
        .unwrap()
        .1;
    let plan = common::plan(&unusable);
    assert_eq!(value(&plan, Field::FirstLayerHeight), Value::Unset);
    assert!(!plan.derived.as_ref().unwrap().calibrated);
    assert!(differs(&plan));
    // Out-of-range signature values cannot be built at all.
    let mut parts = common::gauge_signature().parts().clone();
    parts.first_layer_bed_c = 300;
    assert!(hotend_settings::signature::FitSignature::new(parts).is_err());
}

// ---- Refusals --------------------------------------------------------------------------

#[test]
fn what_cannot_be_printed_is_blocked_with_a_code_and_a_sentence() {
    let expected = [
        (
            "blocked/no-plate",
            "bed_type_not_chosen",
            Some("question.plate_type"),
        ),
        (
            "blocked/no-spools",
            "spools_not_declared",
            Some("question.spools"),
        ),
        (
            "blocked/other-material",
            "spools_not_declared",
            Some("question.spools"),
        ),
        (
            "blocked/two-spools",
            "spool_not_chosen",
            Some("question.spool_choice"),
        ),
        ("blocked/tpu-85a", "material_not_printable", None),
        ("blocked/tpu-in-ams", "material_not_allowed", None),
        ("blocked/auxiliary-nozzle", "nozzle_choice_unproven", None),
        ("blocked/other-nozzle", "nozzle_size_unproven", None),
        (
            "PLA/cool-supertack/L0",
            "plate_not_supported",
            Some("question.plate_type"),
        ),
        ("PETG/cool/L0", "plate_material_conflict", None),
        ("TPU/cool/L1", "plate_material_conflict", None),
        ("ABS/cool/L2", "plate_material_conflict", None),
    ];
    let corpus = corpus();
    for (name, code, question) in expected {
        let inputs = &corpus
            .iter()
            .find(|(known, _)| known == name)
            .unwrap_or_else(|| panic!("{name}"))
            .1;
        let plan = plan(inputs);
        let blocked = plan
            .blocked
            .as_ref()
            .unwrap_or_else(|| panic!("{name} is not blocked"));
        assert_eq!(blocked.code, code, "{name}");
        assert!(
            blocked.sentence.ends_with('.') && !blocked.sentence.contains('{'),
            "{name}"
        );
        assert_eq!(
            plan.questions.iter().map(|q| q.code).collect::<Vec<_>>(),
            question.into_iter().collect::<Vec<_>>(),
            "{name}"
        );
        // A blocked plan delivers nothing to print.
        assert!(
            plan.settings.is_none() && plan.decisions.is_empty() && plan.checklist.items.is_empty()
        );
        assert!(plan.facts.is_some());
    }
    let petg = plan(
        &corpus
            .iter()
            .find(|(name, _)| name == "PETG/cool/L0")
            .unwrap()
            .1,
    );
    assert_eq!(
        petg.blocked.unwrap().sentence,
        "PETG must not be printed on the Cool Plate. Put the Textured PEI plate on and tell Hotend."
    );
    let supertack = plan(
        &corpus
            .iter()
            .find(|(name, _)| name == "PLA/cool-supertack/L0")
            .unwrap()
            .1,
    );
    assert_eq!(
        supertack.blocked.unwrap().sentence,
        "Hotend has no proven settings for this plate yet. Put the Textured PEI plate on."
    );
    let refused = book().refused_facts().unwrap();
    assert_eq!(refused.blocked.as_ref().unwrap().code, "facts_invalid");
    assert!(refused.facts.is_none() && refused.settings.is_none());

    // The softest grade prints with a larger nozzle; 90A and 95A print with 0.4 mm.
    let soft = &corpus
        .iter()
        .find(|(name, _)| name == "blocked/tpu-85a")
        .unwrap()
        .1;
    for (id, blocked) in [
        ("tpu-85a", true),
        ("tpu-90a", false),
        ("tpu-95a", false),
        ("tpu-generic", false),
    ] {
        let plan = plan(&common::inputs_with_material(soft.clone(), id));
        assert_eq!(plan.blocked.is_some(), blocked, "{id}");
        if !blocked {
            // The 90A grade has a flow cap, and so has a filament whose hardness the id
            // does not name; 95A is harder than every grade of the card.
            assert_eq!(
                value(&plan, Field::MaxFlowCap),
                if id == "tpu-95a" {
                    Value::Unset
                } else {
                    Value::Decimal(2.5)
                }
            );
        }
    }
    // The chosen spool among two decides slot and colour; a choice that names no matching
    // spool is no choice.
    let two = &corpus
        .iter()
        .find(|(name, _)| name == "two-spools")
        .unwrap()
        .1;
    let plan_two = plan(two);
    assert_eq!(
        value(&plan_two, Field::Spool),
        Value::Slot(SpoolSlot::Ams(3))
    );
    assert_eq!(
        plan_two.decision(Field::Spool).unwrap().source,
        Source::Person
    );
    assert!(plan_two.checklist.items.iter().any(|item| {
        item.text
            .starts_with("This print uses your red PLA from slot 3.")
    }));
    let mut wrong = two.clone();
    wrong.spool_choice = Some(SpoolSlot::Ams(9));
    assert_eq!(plan(&wrong).blocked.unwrap().code, "spool_not_chosen");
}

// ---- Properties over many plates (the generator is `common::generated`) ------------------

#[test]
fn every_value_lies_in_its_hard_range_and_in_its_allowed_set() {
    let mut plans = 0;
    for inputs in generated(600)
        .into_iter()
        .chain(corpus().into_iter().map(|(_, inputs)| inputs))
    {
        let plan = plan(&inputs);
        let Some(settings) = &plan.settings else {
            continue;
        };
        plans += 1;
        assert_eq!(plan.decisions.len(), Field::ALL.len());
        for (decision, field) in plan.decisions.iter().zip(Field::ALL.iter().copied()) {
            assert_eq!(decision.field, field);
            assert_eq!(decision.value, settings.get(field));
            assert_eq!(bounds::check(field, &decision.value), Ok(()), "{field}");
            assert_eq!(
                bounds::check(field, &decision.rule_value),
                Ok(()),
                "{field}"
            );
            assert_eq!(decision.unit, bounds::spec(field).unit);
            if let Envelope::Set { allowed } = &decision.envelope {
                assert!(
                    allowed.contains(&decision.value),
                    "{field}: {:?} not in {allowed:?}",
                    decision.value
                );
                for value in allowed {
                    assert_eq!(bounds::check(field, value), Ok(()), "{field}");
                }
            }
            // Only the level, the intent and a field somebody asked to change can differ
            // from what the rules gave; the brim kind follows a changed brim width.
            if decision.value != decision.rule_value {
                let changed = |field: Field| inputs.changes.get(field).is_some();
                assert!(
                    matches!(field, Field::Level | Field::Intent)
                        || changed(field)
                        || (field == Field::BrimKind && changed(Field::BrimWidth)),
                    "{field}"
                );
            }
            // An unset value is left to the profile, and says so.
            assert_eq!(
                decision.value.is_unset(),
                decision.source == Source::Profile,
                "{field}"
            );
        }
        // The cross-field rules hold for the rule plan.
        let derived = plan.derived.as_ref().unwrap();
        let (first, other) = (
            whole(&plan, Field::FirstLayerBed),
            whole(&plan, Field::LayersBed),
        );
        assert!(other <= first && first <= inputs.printer.bed_limit_c());
        assert!(derived.bed_window_c[0] <= other && first <= derived.bed_window_c[1]);
        assert_eq!(value(&plan, Field::PrimeTower), Value::Flag(false));
        assert_eq!(settings.filaments().len(), 1);
        assert!(whole(&plan, Field::WallLoops) >= derived.wall_loops_floor);
        assert_eq!(
            token(&plan, Field::BrimKind) == "none",
            whole(&plan, Field::BrimWidth) == 0
        );
        assert!(whole(&plan, Field::BrimWidth) >= derived.asked_brim_width_mm);
        assert!(whole(&plan, Field::BrimWidth) <= derived.brim_width_max_mm);
        if let Some(height) = value(&plan, Field::FirstLayerHeight).number() {
            assert!(height <= 0.75 * inputs.printer.nozzle_diameter_mm() + 1e-9);
            assert!(height <= inputs.printer.max_layer_height_mm() + 1e-9);
        }
    }
    assert!(plans > 400, "only {plans} plans were not blocked");
}

#[test]
fn the_plan_is_a_pure_function_of_its_inputs() {
    let fresh = RuleBook::from_toml(RULES, CARDS, INTENTS, SENTENCES).unwrap();
    for inputs in generated(120)
        .into_iter()
        .chain(corpus().into_iter().map(|(_, inputs)| inputs))
    {
        let first = plan(&inputs);
        let again = plan(&inputs.clone());
        let other_book = fresh.plan(&inputs).unwrap();
        let bytes = serde_json::to_vec(&first).unwrap();
        assert_eq!(bytes, serde_json::to_vec(&again).unwrap());
        assert_eq!(bytes, serde_json::to_vec(&other_book).unwrap());
        assert_eq!(first.sha256().unwrap(), other_book.sha256().unwrap());
        assert_eq!(first.sha256().unwrap().len(), 64);
    }
    // Different inputs give a different hash.
    assert_ne!(
        plan(&gauge()).sha256().unwrap(),
        plan(&pla(calm())).sha256().unwrap()
    );
}

#[test]
fn a_higher_level_never_weakens_what_holds_the_first_layer() {
    for base in generated(250) {
        let at = |asked: Option<u8>, history: u8| {
            let mut inputs = base.clone();
            inputs.level = asked.and_then(person);
            inputs.history = History::new(history, None).unwrap();
            plan(&inputs)
        };
        let steps = [
            at(None, 0),
            at(Some(1), 0),
            at(Some(2), 0),
            at(None, 1),
            at(None, 2),
            at(None, 3),
        ];
        if steps[0].blocked.is_some() {
            continue;
        }
        for (low, high) in [(0, 1), (1, 2), (0, 3), (3, 4), (4, 5)] {
            let (low, high) = (&steps[low], &steps[high]);
            assert!(whole(high, Field::Level) >= whole(low, Field::Level));
            for field in [
                Field::BrimWidth,
                Field::FirstLayerBed,
                Field::LayersBed,
                Field::PartFanOffLayers,
            ] {
                assert!(
                    whole(high, field) >= whole(low, field),
                    "{field} fell with the level"
                );
            }
            for field in [
                Field::FirstLayerWallSpeed,
                Field::FirstLayerInfillSpeed,
                Field::AuxFanFirstLayers,
            ] {
                assert!(
                    whole(high, field) <= whole(low, field),
                    "{field} rose with the level"
                );
            }
            if let (Some(low), Some(high)) = (
                value(low, Field::AuxFanMax).whole(),
                value(high, Field::AuxFanMax).whole(),
            ) {
                assert!(high <= low);
            }
            // A brim, once required, stays required and attached.
            if low.derived.as_ref().unwrap().brim_required {
                assert!(high.derived.as_ref().unwrap().brim_required);
                assert!(
                    value(high, Field::BrimGap).number() <= value(low, Field::BrimGap).number()
                );
            }
        }
    }
}

#[test]
fn hostile_facts_can_only_ask_for_more_adhesion() {
    // A thin ring whose strip width and slenderness the engine overstates and understates.
    let honest = plan(&pla(strip(2.0)));
    let mut lying = strip(2.0);
    lying.strip_width_mm = 13.0;
    assert_eq!(plan(&pla(lying.clone())).settings, honest.settings);
    // A width no contact of this area can have is refused outright (tests/review.rs).
    lying.strip_width_mm = 500.0;
    assert_eq!(
        hotend_settings::facts::PlateFacts::new(Family::Pla, vec![lying])
            .unwrap_err()
            .problem,
        hotend_settings::facts::Problem::Inconsistent
    );
    let mut tall = block(12.0, 12.0, 150.0);
    let honest = plan(&inputs(
        Family::Petg,
        PlateType::TexturedPei,
        vec![tall.clone()],
    ));
    tall.slenderness = 0.0;
    assert_eq!(
        plan(&inputs(Family::Petg, PlateType::TexturedPei, vec![tall])).settings,
        honest.settings
    );
}

// ---- Sentences and the list -------------------------------------------------------------

/// The profile keys of the slicer mapping table. No sentence may name one.
const SLICER_KEYS: &[&str] = &[
    "curr_bed_type",
    "brim_type",
    "brim_width",
    "brim_object_gap",
    "skirt_loops",
    "elefant_foot_compensation",
    "initial_layer_print_height",
    "initial_layer_line_width",
    "initial_layer_infill_line_width",
    "initial_layer_speed",
    "initial_layer_infill_speed",
    "initial_layer_acceleration",
    "textured_plate_temp",
    "cool_plate_temp",
    "hot_plate_temp",
    "eng_plate_temp",
    "nozzle_temperature",
    "close_fan_the_first_x_layers",
    "first_x_layer_fan_speed",
    "additional_cooling_fan_speed",
    "wall_loops",
    "sparse_infill_density",
    "sparse_infill_pattern",
    "seam_position",
    "enable_support",
    "support_type",
    "support_threshold_angle",
    "support_on_build_plate_only",
    "outer_wall_speed",
    "inner_wall_speed",
    "sparse_infill_speed",
    "internal_solid_infill_speed",
    "top_surface_speed",
    "gap_infill_speed",
    "default_acceleration",
    "outer_wall_acceleration",
    "filament_max_volumetric_speed",
    "outer_wall_jerk",
    "inner_wall_jerk",
    "top_surface_jerk",
    "infill_jerk",
    "initial_layer_jerk",
    "enable_prime_tower",
    "filament_colour",
    "outer_only",
    "no_brim",
    "inner_only",
    "auto_brim",
    "tree(auto)",
    "normal(auto)",
    "_gcode",
];

#[test]
fn every_decision_has_a_sentence_and_every_sentence_is_reached() {
    let mut decisions = BTreeSet::new();
    let mut notices = BTreeSet::new();
    let mut lines = BTreeSet::new();
    let check = |text: &str| {
        assert!(
            !text.is_empty() && !text.contains(['{', '}', '\n']),
            "{text:?}"
        );
        for key in SLICER_KEYS {
            assert!(!text.contains(key), "{text:?} names the profile key {key}");
        }
    };
    let extra = generated(300);
    let named = corpus();
    let proven = common::proven_corpus();
    for inputs in named
        .iter()
        .chain(&proven)
        .map(|(_, inputs)| inputs)
        .chain(&extra)
    {
        let plan = plan(inputs);
        for decision in &plan.decisions {
            check(&decision.sentence);
            decisions.insert(decision.sentence_id);
        }
        for notice in plan
            .blocked
            .iter()
            .chain(&plan.questions)
            .chain(&plan.warnings)
            .chain(&plan.notes)
        {
            check(&notice.sentence);
            notices.insert(notice.code);
        }
        for item in &plan.checklist.items {
            check(&item.text);
            lines.insert(item.id);
            assert_eq!(book().catalogue().line_class(item.id), Some(item.class));
        }
    }
    notices.insert(book().refused_facts().unwrap().blocked.unwrap().code);
    let missing = |declared: &[(&'static str, &[&str])],
                   seen: &BTreeSet<&'static str>|
     -> Vec<&'static str> {
        declared
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| !seen.contains(id))
            .collect()
    };
    assert_eq!(
        missing(DECISIONS, &decisions),
        Vec::<&str>::new(),
        "decision sentences no test plate reaches"
    );
    assert_eq!(
        missing(NOTICES, &notices),
        Vec::<&str>::new(),
        "notices no test plate reaches"
    );
    assert_eq!(
        missing(LINES, &lines),
        Vec::<&str>::new(),
        "list lines no test plate reaches"
    );
    // The catalogue itself names no profile key either.
    for text in book().catalogue().texts() {
        for key in SLICER_KEYS {
            assert!(!text.contains(key), "{text:?} names the profile key {key}");
        }
    }
}

#[test]
fn the_list_never_drops_a_line_of_classes_1_to_3() {
    let mut longest = 0;
    let mut dropped = 0;
    for inputs in corpus()
        .into_iter()
        .map(|(_, inputs)| inputs)
        .chain(generated(400))
    {
        let plan = plan(&inputs);
        if plan.blocked.is_some() {
            continue;
        }
        let flexible = inputs.facts.family() == Family::Tpu;
        // Level 3 adds three lines of its own, and the list is that much longer there.
        let extra = if whole(&plan, Field::Level) >= 3 {
            3
        } else {
            0
        };
        assert_eq!(plan.checklist.limit, if flexible { 10 } else { 8 } + extra);
        // The items are in class order.
        assert!(
            plan.checklist
                .items
                .windows(2)
                .all(|pair| pair[0].class <= pair[1].class)
        );
        for (minutes, project) in [
            (None, false),
            (Some(10), false),
            (Some(600), true),
            (None, true),
        ] {
            let shown = plan.checklist.lines(minutes, project);
            let ids: Vec<&str> = shown.iter().map(|line| line.id).collect();
            let wanted: Vec<&hotend_settings::model::ChecklistItem> = plan
                .checklist
                .items
                .iter()
                .filter(|item| match item.id {
                    "vent" => minutes.is_none_or(|minutes| minutes > 60),
                    "project" => project,
                    _ => true,
                })
                .collect();
            // Every line of classes 1 to 3 that applies is shown.
            for item in wanted.iter().filter(|item| item.class <= 3) {
                assert!(ids.contains(&item.id), "{} was dropped", item.id);
            }
            // Of the lines that may go, what is shown is a prefix of what applies: lines go
            // from the end only. (The lines beyond the classes 1 to 3 that never go: see
            // tests/review.rs.)
            let may_go = |item: &&hotend_settings::model::ChecklistItem| {
                !hotend_settings::model::Checklist::never_dropped(item)
            };
            let shown_may_go: Vec<&str> = shown
                .iter()
                .copied()
                .filter(may_go)
                .map(|item| item.id)
                .collect();
            assert_eq!(
                shown_may_go,
                wanted
                    .iter()
                    .copied()
                    .filter(may_go)
                    .take(shown_may_go.len())
                    .map(|item| item.id)
                    .collect::<Vec<_>>()
            );
            let kept: usize = wanted.iter().copied().filter(|item| !may_go(item)).count();
            assert!(shown.len() <= plan.checklist.limit.max(kept));
            // A flexible plan fits into ten lines, a rigid one into eight; level 3 adds
            // its three.
            assert!(shown.len() <= plan.checklist.limit, "{ids:?}");
            longest = longest.max(shown.len());
            dropped += wanted.len() - shown.len();
            assert_eq!(ids.contains(&"project"), project);
        }
    }
    // A flexible plate at level 3; below level 3 the longest lists are 8 and 10 lines
    // (tests/review.rs).
    assert_eq!(longest, 13);
    assert!(dropped > 0, "no test plate has more lines than fit");
}

// ---- The data files ---------------------------------------------------------------------

#[test]
fn every_plate_a_print_is_checked_for_is_a_plate_type_of_the_settings() {
    // The printer layer names the build plates it can check a sliced file for by the ids
    // the application's `plate_type` setting uses. Each must be a token here too, so that
    // one answer about the plate serves the plan and the check.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../hotend-printer/profiles/bambu.toml");
    let profile: toml::Table = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let plates = profile["printer"].as_array().unwrap()[0]["plate"]
        .as_array()
        .unwrap();
    assert!(!plates.is_empty());
    for plate in plates {
        let id = plate["id"].as_str().unwrap();
        assert!(
            PlateType::from_token(id).is_some(),
            "`{id}` is not a plate type of the print settings"
        );
    }
}

#[test]
fn every_family_has_a_card_inside_the_printer_limits() {
    let cards = book().cards();
    let families: Vec<Family> = cards.all().iter().map(|card| card.family).collect();
    assert_eq!(families.len(), Family::ALL.len());
    for family in Family::ALL {
        assert!(cards.card(*family).is_some(), "{family} has no card");
    }
    // The limits of the printer data, read from the profile file of hotend-printer.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../hotend-printer/profiles/bambu.toml");
    let profile: toml::Table = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let limits = &profile["printer"].as_array().unwrap()[0]["limits"];
    let printer_nozzle = limits["nozzle_max_c"].as_float().unwrap();
    let printer_bed = limits["bed_max_c"].as_float().unwrap();
    assert_eq!(
        bounds::BED_C.1 as f64,
        printer_bed,
        "the bed range ends at the printer's limit"
    );
    assert!(bounds::NOZZLE_C.1 as f64 <= printer_nozzle);
    for card in cards.all() {
        let filament = profile["filament"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["family"].as_str() == Some(card.family.as_str()))
            .unwrap_or_else(|| panic!("the printer data have no limits for {}", card.family));
        // The nozzle range is a constant of the card, inside `nozzle_max_c`.
        let [low, high] = card.nozzle_range_c;
        assert!(
            low < high && high as f64 <= filament["nozzle_max_c"].as_float().unwrap(),
            "{}",
            card.family
        );
        assert!(high as f64 <= printer_nozzle);
        for plate in hotend_settings::cards::CARD_PLATES {
            let Some(bed) = card.on(plate).unwrap().bed_values() else {
                continue;
            };
            let limit = filament["bed_max_c"].as_float().unwrap().min(printer_bed);
            assert!(bed.window[1] as f64 <= limit, "{} on {plate}", card.family);
        }
    }
    let pla = cards.card(Family::Pla).unwrap();
    assert_eq!(pla.nozzle_range_c, [190, 240]);
    // The one hard range that is not a single pair of numbers.
    assert!(matches!(
        bounds::spec(Field::FirstLayerBed).kind,
        Kind::Whole { min: 30, max: 120 }
    ));
}

fn edited(file: &str, from: &str, to: &str) -> String {
    assert!(
        file.contains(from),
        "the data file no longer holds {from:?}"
    );
    file.replacen(from, to, 1)
}

#[test]
fn a_data_file_cannot_widen_a_range_or_weaken_a_level() {
    let load = |rules: &str, cards: &str, intents: &str, sentences: &str| {
        RuleBook::from_toml(rules, cards, intents, sentences)
            .map(|_| ())
            .map_err(|error| error.file)
    };
    assert_eq!(load(RULES, CARDS, INTENTS, SENTENCES), Ok(()));
    let rules = |from: &str, to: &str| load(&edited(RULES, from, to), CARDS, INTENTS, SENTENCES);
    let cards = |from: &str, to: &str| load(RULES, &edited(CARDS, from, to), INTENTS, SENTENCES);
    let intents = |from: &str, to: &str| load(RULES, CARDS, &edited(INTENTS, from, to), SENTENCES);
    let sentences =
        |from: &str, to: &str| load(RULES, CARDS, INTENTS, &edited(SENTENCES, from, to));

    // Values outside a hard range.
    assert_eq!(
        rules(
            "below_mm = 3.0\nwidth_mm = 5",
            "below_mm = 3.0\nwidth_mm = 16"
        ),
        Err("print-rules.toml")
    );
    assert_eq!(
        rules("elephant_foot_mm = 0.15", "elephant_foot_mm = 0.5"),
        Err("print-rules.toml")
    );
    assert_eq!(
        rules(
            "first_layer_height_mm = 0.25",
            "first_layer_height_mm = 0.4"
        ),
        Err("print-rules.toml")
    );
    assert_eq!(
        rules("accel_cap_mm_s2 = 4000", "accel_cap_mm_s2 = 40000"),
        Err("print-rules.toml")
    );
    assert_eq!(
        rules("below_mm = 3.0", "below_mm = -3.0"),
        Err("print-rules.toml")
    );
    assert_eq!(
        rules("below_mm = 3.0", "below_mm = nan"),
        Err("print-rules.toml")
    );
    assert_eq!(
        rules("on_from_mm2 = 20.0", "on_from_mm2 = 200.0"),
        Err("print-rules.toml")
    );
    assert_eq!(
        cards("walls_mm_s = [30, 20, 15]", "walls_mm_s = [30, 20, 60]"),
        Err("print-cards.toml")
    );
    assert_eq!(
        cards("bed = [60, 55]", "bed = [160, 55]"),
        Err("print-cards.toml")
    );
    assert_eq!(
        cards("nozzle_range_c = [190, 240]", "nozzle_range_c = [190, 340]"),
        Err("print-cards.toml")
    );
    assert_eq!(
        cards("window = [55, 65]", "window = [55, 125]"),
        Err("print-cards.toml")
    );
    assert_eq!(
        intents("wall_loops = 5", "wall_loops = 9"),
        Err("intents.toml")
    );
    assert_eq!(
        intents("infill_pct = 25", "infill_pct = 80"),
        Err("intents.toml")
    );
    // A higher level may not be weaker than a lower one.
    assert_eq!(
        cards("walls_mm_s = [30, 20, 15]", "walls_mm_s = [30, 20, 25]"),
        Err("print-cards.toml")
    );
    assert_eq!(
        cards("bed_raised = [65, 60]", "bed_raised = [58, 55]"),
        Err("print-cards.toml")
    );
    assert_eq!(
        cards("pct = [50, 30, 30]", "pct = [50, 30, 40]"),
        Err("print-cards.toml")
    );
    assert_eq!(
        cards("bed = [60, 55]", "bed = [55, 60]"),
        Err("print-cards.toml")
    );
    // Unknown keys, unknown tokens, missing families and sentences.
    assert_eq!(
        rules("[venting]", "[venting]\ngcode = \"M104 S300\""),
        Err("print-rules.toml")
    );
    assert_eq!(
        cards("warp_class = \"low\"", "warp_class = \"none\""),
        Err("print-cards.toml")
    );
    assert_eq!(
        cards("family = \"ASA\"", "family = \"ABS\""),
        Err("print-cards.toml")
    );
    assert_eq!(
        cards("list = [\"after.cool\"]", "list = [\"after.party\"]"),
        Err("print-cards.toml")
    );
    assert_eq!(
        intents("process = \"fine-0.12mm\"", "process = \"fine-0.08mm\""),
        Err("intents.toml")
    );
    assert_eq!(
        intents("id = \"fast\"", "id = \"strong\""),
        Err("intents.toml")
    );
    assert_eq!(
        sentences("\"fixed.skirt\" = \"No skirt is drawn.\"\n", ""),
        Err("sentences.toml")
    );
    assert_eq!(
        sentences("\"fixed.skirt\" =", "\"fixed.skirts\" ="),
        Err("sentences.toml")
    );
    assert_eq!(
        sentences(
            "is {width} mm wide because the thinnest",
            "is {wide} mm wide because the thinnest"
        ),
        Err("sentences.toml")
    );
    assert_eq!(
        sentences("id = \"outcome\"\nclass = 6", "id = \"outcome\"\nclass = 7"),
        Err("sentences.toml")
    );
    assert_eq!(sentences("grey = \"grey\"\n", ""), Err("sentences.toml"));
    assert_eq!(sentences("schema = 1", "schema = 2"), Err("sentences.toml"));
}

#[test]
fn inputs_take_only_their_closed_values() {
    use hotend_settings::inputs::{Capabilities, InputError, PrinterFacts};
    assert_eq!(History::new(4, None), Err(InputError::Level));
    assert_eq!(History::new(0, Some(9)), Err(InputError::Level));
    let spool = pla(calm()).setup.spools()[0].clone();
    assert_eq!(
        Setup::new(None, vec![spool.clone(), spool.clone()]),
        Err(InputError::DuplicateSlot)
    );
    assert_eq!(
        Capabilities::none()
            .with_value(Field::BrimKind, "auto")
            .unwrap_err(),
        InputError::Token(Field::BrimKind)
    );
    assert_eq!(
        Capabilities::none()
            .with_value(Field::BrimWidth, "5")
            .unwrap_err(),
        InputError::Token(Field::BrimWidth)
    );
    for (nozzle, layer, bed) in [
        (f64::NAN, 0.28, 70),
        (0.4, f64::INFINITY, 70),
        (0.4, 0.28, 200),
        (5.0, 0.28, 70),
    ] {
        assert!(PrinterFacts::new(nozzle, layer, bed, Vec::new()).is_err());
    }
    // A slot the printer data do not know cannot be printed from.
    let mut inputs = pla(calm());
    inputs.printer = PrinterFacts::new(0.4, 0.28, 70, Vec::new()).unwrap();
    assert_eq!(
        plan(&inputs).blocked.unwrap().code,
        "nozzle_choice_unproven"
    );
}
