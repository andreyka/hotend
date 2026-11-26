//! The model: every hard range is a constant and is enforced, and the proposal schema is
//! generated from the same table.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hotend_settings::bounds::{self, Kind, RangeError};
use hotend_settings::facts::{
    FactsError, FitAtBed, FitSide, OverhangRegion, PieceFacts, PlateFacts, Problem,
};
use hotend_settings::model::{Colour, Family, Field, MaterialId, SpoolSlot, Value};
use hotend_settings::schema;

fn snapshot(name: &str, actual: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name);
    if std::env::var_os("HOTEND_UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, actual).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        expected == actual,
        "snapshot {name} differs; run with HOTEND_UPDATE_SNAPSHOTS=1 and review the diff"
    );
}

#[test]
fn field_names_are_unique_and_round_trip() {
    let mut names: Vec<&str> = Field::ALL.iter().map(|field| field.as_str()).collect();
    assert_eq!(names.len(), 39);
    for field in Field::ALL {
        assert_eq!(Field::from_token(field.as_str()), Some(*field));
        assert_eq!(Field::ALL[field.index()], *field);
    }
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), Field::ALL.len());
    assert_eq!(Field::from_token("brim_type"), None);
    assert_eq!(Field::from_token("adhesion.level "), None);
}

#[test]
fn every_field_has_a_plain_name_without_a_key_or_a_token() {
    let mut plain: Vec<&str> = Field::ALL.iter().map(|field| field.plain_name()).collect();
    for name in &plain {
        // Lower-case words and spaces only: no slicer key, no field token, no number.
        assert!(
            !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == ' '),
            "{name}"
        );
        assert!(!name.starts_with(' ') && !name.ends_with(' ') && !name.contains("  "));
    }
    plain.sort_unstable();
    plain.dedup();
    assert_eq!(plain.len(), Field::ALL.len());
}

#[test]
fn every_numeric_field_accepts_its_bounds_and_refuses_the_next_value() {
    for field in Field::ALL.iter().copied() {
        match bounds::spec(field).kind {
            Kind::Whole { min, max } => {
                assert!(min <= max, "{field}");
                assert_eq!(bounds::check(field, &Value::Whole(min)), Ok(()), "{field}");
                assert_eq!(bounds::check(field, &Value::Whole(max)), Ok(()), "{field}");
                for outside in [min - 1, max + 1, i64::MIN, i64::MAX] {
                    assert!(
                        matches!(
                            bounds::check(field, &Value::Whole(outside)),
                            Err(RangeError::OutOfRange { .. })
                        ),
                        "{field} {outside}"
                    );
                }
                assert_eq!(
                    bounds::check(field, &Value::Decimal(min as f64)),
                    Err(RangeError::WrongKind(field))
                );
            }
            Kind::Decimal { min, max } => {
                assert!(min < max, "{field}");
                assert_eq!(
                    bounds::check(field, &Value::decimal(min)),
                    Ok(()),
                    "{field}"
                );
                assert_eq!(
                    bounds::check(field, &Value::decimal(max)),
                    Ok(()),
                    "{field}"
                );
                for outside in [min - 0.01, max + 0.01, -1e300, 1e300] {
                    assert!(
                        matches!(
                            bounds::check(field, &Value::Decimal(outside)),
                            Err(RangeError::OutOfRange { .. })
                        ),
                        "{field} {outside}"
                    );
                }
                for odd in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                    assert_eq!(
                        bounds::check(field, &Value::Decimal(odd)),
                        Err(RangeError::NotFinite(field))
                    );
                }
                assert_eq!(
                    bounds::check(field, &Value::Whole(0)),
                    Err(RangeError::WrongKind(field))
                );
            }
            Kind::Tokens(tokens) => {
                assert!(!tokens.is_empty(), "{field}");
                for token in tokens {
                    assert_eq!(
                        bounds::check(field, &Value::Token(token)),
                        Ok(()),
                        "{field}"
                    );
                }
                assert_eq!(
                    bounds::check(field, &Value::Token("auto")),
                    Err(RangeError::UnknownToken(field))
                );
                assert_eq!(
                    bounds::check(field, &Value::Flag(true)),
                    Err(RangeError::WrongKind(field))
                );
            }
            Kind::Flag => {
                assert_eq!(bounds::check(field, &Value::Flag(false)), Ok(()));
                assert_eq!(
                    bounds::check(field, &Value::Whole(1)),
                    Err(RangeError::WrongKind(field))
                );
            }
            Kind::Colour => {
                let colour = Colour::parse("#898989").unwrap();
                assert_eq!(bounds::check(field, &Value::Colour(colour)), Ok(()));
                assert_eq!(
                    bounds::check(field, &Value::Token("grey")),
                    Err(RangeError::WrongKind(field))
                );
            }
            Kind::Slot => {
                assert_eq!(
                    bounds::check(field, &Value::Slot(SpoolSlot::ExternalMain)),
                    Ok(())
                );
                assert_eq!(
                    bounds::check(field, &Value::Whole(1)),
                    Err(RangeError::WrongKind(field))
                );
            }
        }
    }
}

#[test]
fn the_hard_ranges_are_the_documented_ones() {
    let whole = |field: Field| match bounds::spec(field).kind {
        Kind::Whole { min, max } => (min, max),
        other => panic!("{field}: {other:?}"),
    };
    let decimal = |field: Field| match bounds::spec(field).kind {
        Kind::Decimal { min, max } => (min, max),
        other => panic!("{field}: {other:?}"),
    };
    assert_eq!(whole(Field::Level), (0, 3));
    assert_eq!(whole(Field::BrimWidth), (0, 15));
    assert_eq!(decimal(Field::BrimGap), (0.0, 0.3));
    assert_eq!(whole(Field::SkirtLoops), (0, 0));
    assert_eq!(decimal(Field::ElephantFoot), (0.0, 0.3));
    assert_eq!(decimal(Field::FirstLayerHeight), (0.16, 0.28));
    assert_eq!(decimal(Field::FirstLayerLineWidth), (0.42, 0.60));
    assert_eq!(whole(Field::FirstLayerWallSpeed), (10, 50));
    assert_eq!(whole(Field::FirstLayerInfillSpeed), (15, 105));
    assert_eq!(whole(Field::FirstLayerAccel), (250, 500));
    assert_eq!(whole(Field::PartFanOffLayers), (1, 10));
    assert_eq!(whole(Field::AuxFanFirstLayers), (0, 40));
    assert_eq!(whole(Field::AuxFanMax), (0, 100));
    assert_eq!(whole(Field::WallLoops), (2, 6));
    assert_eq!(whole(Field::InfillPct), (5, 50));
    assert_eq!(whole(Field::SupportsThreshold), (20, 45));
    assert_eq!(whole(Field::OuterWallSpeedCap), (20, 200));
    assert_eq!(whole(Field::SpeedCap), (20, 300));
    assert_eq!(whole(Field::AccelCap), (500, 10_000));
    assert_eq!(whole(Field::OuterWallAccelCap), (500, 5000));
    assert_eq!(decimal(Field::MaxFlowCap), (1.0, 25.0));
    assert_eq!(whole(Field::JerkCap), (1, 9));
    assert_eq!(whole(Field::FirstLayerBed), whole(Field::LayersBed));
}

#[test]
fn only_fields_the_table_calls_optional_may_be_left_to_the_profile() {
    let optional: Vec<&str> = Field::ALL
        .iter()
        .filter(|field| bounds::check(**field, &Value::Unset).is_ok())
        .map(|field| field.as_str())
        .collect();
    assert_eq!(
        optional,
        [
            "adhesion.elephant_foot_mm",
            "first_layer.height_mm",
            "first_layer.line_width_mm",
            "first_layer.accel_mm_s2",
            "first_layer.nozzle_c",
            "layers.nozzle_c",
            "cooling.aux_fan_max_pct",
            "motion.outer_wall_speed_cap_mm_s",
            "motion.speed_cap_mm_s",
            "motion.accel_cap_mm_s2",
            "motion.outer_wall_accel_cap_mm_s2",
            "motion.max_flow_cap_mm3_s",
            "motion.jerk_cap_mm_s",
        ]
    );
    assert_eq!(
        bounds::check(Field::BrimKind, &Value::Unset),
        Err(RangeError::Required(Field::BrimKind))
    );
}

#[test]
fn the_adjustable_fields_are_the_a_fields_of_the_tables() {
    let names: Vec<&str> = bounds::adjustable_fields()
        .map(|field| field.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "intent",
            "adhesion.level",
            "adhesion.brim_kind",
            "adhesion.brim_width_mm",
            "first_layer.wall_speed_mm_s",
            "first_layer.infill_speed_mm_s",
            "first_layer.accel_mm_s2",
            "first_layer.bed_c",
            "cooling.part_fan_off_layers",
            "cooling.aux_fan_max_pct",
            "shell.wall_loops",
            "shell.infill_pct",
            "shell.infill_pattern",
            "shell.seam",
            "supports.enabled",
            "supports.kind",
            "supports.threshold_deg",
            "supports.where",
            "motion.outer_wall_speed_cap_mm_s",
            "motion.speed_cap_mm_s",
            "motion.accel_cap_mm_s2",
            "motion.outer_wall_accel_cap_mm_s2",
            "motion.max_flow_cap_mm3_s",
        ]
    );
    assert_eq!(bounds::sticking_fields().count(), 18);
}

#[test]
fn colours_slots_and_material_ids_take_only_their_fixed_shape() {
    assert_eq!(Colour::parse("#89aBcD").unwrap().to_string(), "#89ABCD");
    for bad in [
        "898989",
        "#89898",
        "#8989899",
        "#89898g",
        "#89 898",
        "",
        "#٨٩٨٩٨٩",
    ] {
        assert_eq!(Colour::parse(bad), None, "{bad}");
    }
    assert_eq!(SpoolSlot::parse("1"), Some(SpoolSlot::Ams(1)));
    assert_eq!(SpoolSlot::parse("16"), Some(SpoolSlot::Ams(16)));
    assert_eq!(
        SpoolSlot::parse("external_main"),
        Some(SpoolSlot::ExternalMain)
    );
    for bad in [
        "0",
        "17",
        "01",
        "+1",
        "1 ",
        "external",
        "external-main",
        "",
        "١",
    ] {
        assert_eq!(SpoolSlot::parse(bad), None, "{bad}");
    }
    assert_eq!(MaterialId::parse("tpu-90a").unwrap().shore_a(), Some(90));
    assert_eq!(MaterialId::parse("tpu-95a-hf").unwrap().shore_a(), Some(95));
    assert_eq!(MaterialId::parse("pla-basic").unwrap().shore_a(), None);
    // Read like the fit library reads it (tests/data/shore-table.json): digits before the
    // two do not hide the hardness. Only a flexible card uses it.
    assert_eq!(MaterialId::parse("pla-190a").unwrap().shore_a(), Some(90));
    for bad in [
        "",
        "PLA",
        "pla basic",
        "pla/../x",
        "-pla",
        "pla\n",
        &"a".repeat(41),
    ] {
        assert_eq!(MaterialId::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn the_proposal_schema_is_generated_from_the_field_table() {
    let schema = schema::proposal_schema();
    let item = &schema["properties"]["changes"]["items"];
    let names: Vec<&str> = item["properties"]["field"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let adjustable: Vec<&str> = bounds::adjustable_fields()
        .map(|field| field.as_str())
        .collect();
    assert_eq!(names, adjustable);
    let branches = item["oneOf"].as_array().unwrap();
    assert_eq!(branches.len(), adjustable.len());
    for (branch, field) in branches.iter().zip(bounds::adjustable_fields()) {
        assert_eq!(branch["properties"]["field"]["const"], field.as_str());
        let value = &branch["properties"]["value"];
        match bounds::spec(field).kind {
            Kind::Whole { min, max } => {
                assert_eq!(value["type"], "integer");
                assert_eq!(value["minimum"], min);
                assert_eq!(value["maximum"], max);
            }
            Kind::Decimal { min, max } => {
                assert_eq!(value["type"], "number");
                assert_eq!(value["minimum"], min);
                assert_eq!(value["maximum"], max);
            }
            Kind::Tokens(tokens) => assert_eq!(value["enum"], serde_json::json!(tokens)),
            Kind::Flag => assert_eq!(value["type"], "boolean"),
            Kind::Colour | Kind::Slot => panic!("{field} is not adjustable"),
        }
    }
    // Unknown properties are refused at both levels, and the sizes are bounded.
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(item["additionalProperties"], false);
    assert_eq!(schema["properties"]["changes"]["maxItems"], 16);
    assert_eq!(schema["properties"]["changes"]["minItems"], 1);
    assert_eq!(item["properties"]["reason"]["minLength"], 1);
    assert_eq!(item["properties"]["reason"]["maxLength"], 240);
    assert_eq!(schema["properties"]["summary"]["maxLength"], 300);
    // No slicer key and no setup field can be named.
    for forbidden in [
        "setup.plate_type",
        "filament.spool",
        "shell.process",
        "prime_tower.enabled",
        "brim_type",
    ] {
        assert!(!names.contains(&forbidden), "{forbidden}");
    }
    snapshot(
        "proposal-schema.json",
        &(serde_json::to_string_pretty(&schema).unwrap() + "\n"),
    );
}

fn piece() -> PieceFacts {
    PieceFacts {
        contact_mm2: 400.0,
        contact_perimeter_mm: 80.0,
        strip_width_mm: 10.0,
        contact_span_mm: 20.0,
        height_mm: 10.0,
        slenderness: 0.5,
        overhang_mm2: 0.0,
        fit_overhang_mm2: 0.0,
        overhang_over_part_mm2: 0.0,
        overhang_regions: Some(Vec::new()),
        inner_loops: Vec::new(),
        fit_at_bed: Vec::new(),
        min_wall_mm: None,
    }
}

fn refused(change: impl FnOnce(&mut PieceFacts)) -> FactsError {
    let mut facts = piece();
    change(&mut facts);
    PlateFacts::new(Family::Pla, vec![piece(), facts]).unwrap_err()
}

#[test]
fn facts_outside_their_bounds_are_refused_with_the_fact_named() {
    assert!(PlateFacts::new(Family::Pla, vec![piece()]).is_ok());
    let cases: Vec<(FactsError, &str, Problem)> = vec![
        (
            refused(|p| p.contact_mm2 = -1.0),
            "contact_mm2",
            Problem::Negative,
        ),
        (
            refused(|p| p.contact_mm2 = 1.0e6 + 1.0),
            "contact_mm2",
            Problem::OutOfBounds,
        ),
        (
            refused(|p| p.contact_perimeter_mm = f64::NAN),
            "contact_perimeter_mm",
            Problem::NotFinite,
        ),
        (
            refused(|p| p.contact_perimeter_mm = 1.0e5 + 1.0),
            "contact_perimeter_mm",
            Problem::OutOfBounds,
        ),
        (
            refused(|p| p.strip_width_mm = f64::INFINITY),
            "strip_width_mm",
            Problem::NotFinite,
        ),
        (
            refused(|p| p.strip_width_mm = 1000.1),
            "strip_width_mm",
            Problem::OutOfBounds,
        ),
        (
            refused(|p| p.contact_span_mm = 1001.0),
            "contact_span_mm",
            Problem::OutOfBounds,
        ),
        (
            refused(|p| p.height_mm = -0.1),
            "height_mm",
            Problem::Negative,
        ),
        (
            refused(|p| p.slenderness = 1.0e4 + 1.0),
            "slenderness",
            Problem::OutOfBounds,
        ),
        (
            refused(|p| p.overhang_mm2 = f64::NEG_INFINITY),
            "overhang_mm2",
            Problem::NotFinite,
        ),
        (
            refused(|p| p.fit_overhang_mm2 = 2.0e6),
            "fit_overhang_mm2",
            Problem::OutOfBounds,
        ),
        (
            refused(|p| p.overhang_over_part_mm2 = -5.0),
            "overhang_over_part_mm2",
            Problem::Negative,
        ),
        (
            refused(|p| p.min_wall_mm = Some(1.0e9)),
            "min_wall_mm",
            Problem::OutOfBounds,
        ),
        (
            refused(|p| {
                p.inner_loops = vec![hotend_settings::facts::InnerLoop {
                    min_width_mm: -1.0,
                    fit: false,
                }]
            }),
            "inner_loops.min_width_mm",
            Problem::Negative,
        ),
        (
            refused(|p| {
                p.inner_loops = vec![
                    hotend_settings::facts::InnerLoop {
                        min_width_mm: 1.0,
                        fit: false
                    };
                    65
                ];
            }),
            "inner_loops",
            Problem::WrongCount,
        ),
        (
            refused(|p| {
                p.fit_at_bed = vec![FitAtBed {
                    pair: "ring\nM104 S300".to_owned(),
                    side: FitSide::Outside,
                }];
            }),
            "fit_at_bed.pair",
            Problem::BadName,
        ),
        (
            refused(|p| {
                p.fit_at_bed = vec![
                    FitAtBed {
                        pair: "ring".to_owned(),
                        side: FitSide::Outside
                    };
                    65
                ]
            }),
            "fit_at_bed",
            Problem::WrongCount,
        ),
        (
            // The areas of the regions must sum to the overhang within 1 percent.
            refused(|p| {
                p.overhang_mm2 = 100.0;
                p.overhang_regions = Some(vec![OverhangRegion::free(98.0, 5.0, false)]);
            }),
            "overhang_regions",
            Problem::Inconsistent,
        ),
        (
            refused(|p| {
                p.overhang_mm2 = 65.0;
                p.overhang_regions = Some(vec![OverhangRegion::free(1.0, 5.0, false); 65]);
            }),
            "overhang_regions",
            Problem::WrongCount,
        ),
        (
            refused(|p| {
                p.overhang_mm2 = 10.0;
                p.overhang_regions = Some(vec![OverhangRegion::bridge(10.0, 1000.5, false)]);
            }),
            "overhang_regions.length",
            Problem::OutOfBounds,
        ),
        (
            // A bridge must state its span.
            refused(|p| {
                p.overhang_mm2 = 10.0;
                let mut region = OverhangRegion::bridge(10.0, 5.0, false);
                region.span_mm = None;
                region.reach_mm = Some(5.0);
                p.overhang_regions = Some(vec![region]);
            }),
            "overhang_regions",
            Problem::Malformed,
        ),
    ];
    for (error, fact, problem) in cases {
        assert_eq!(
            (error.piece, error.fact, error.problem),
            (Some(1), fact, problem)
        );
        // The message is Hotend's own: it names the fact and what to do.
        assert!(
            error.to_string().contains(fact)
                && error.to_string().contains("prepare the plate again")
        );
    }
    assert_eq!(
        PlateFacts::new(Family::Pla, Vec::new())
            .unwrap_err()
            .problem,
        Problem::WrongCount
    );
    assert_eq!(
        PlateFacts::new(Family::Pla, vec![piece(); 257])
            .unwrap_err()
            .problem,
        Problem::WrongCount
    );
}

#[test]
fn the_engine_object_is_read_strictly() {
    let good = serde_json::json!({
        "family": "PLA", "piece_count": 1, "total_contact_mm2": 456.0,
        "pieces": [{
            "contact_mm2": 456, "contact_perimeter_mm": 452.0, "strip_width_mm": 2.0, "contact_span_mm": 74.0,
            "height_mm": 6.0, "slenderness": 0.28, "overhang_mm2": 30.0, "fit_overhang_mm2": 0.0,
            "overhang_over_part_mm2": 0.0,
            "overhang_regions": [
                {"area_mm2": 20.0, "kind": "bridge", "span_mm": 12.0, "over_part": false},
                {"area_mm2": 10.0, "kind": "hole_ceiling", "span_mm": null, "diameter_mm": 6.0, "reach_mm": null, "over_part": true}
            ],
            "inner_loops": [{"min_width_mm": 70.0, "fit": true}],
            "fit_at_bed": [{"pair": "ring", "side": "outside"}],
            "min_wall_mm": null
        }]
    });
    let facts = PlateFacts::from_value(good.clone()).unwrap();
    assert_eq!(facts.family(), Family::Pla);
    assert_eq!(facts.pieces().len(), 1);
    assert!(facts.fit_at_bed());
    assert_eq!(PlateFacts::from_json(&good.to_string()).unwrap(), facts);

    let broken = |change: &dyn Fn(&mut serde_json::Value)| {
        let mut value = good.clone();
        change(&mut value);
        PlateFacts::from_value(value).unwrap_err()
    };
    type Change = Box<dyn Fn(&mut serde_json::Value)>;
    let malformed: Vec<Change> = vec![
        Box::new(|v| v["extra"] = 1.into()),
        Box::new(|v| v["pieces"][0]["extra"] = 1.into()),
        Box::new(|v| v["pieces"][0]["overhang_regions"][0]["colour"] = "red".into()),
        Box::new(|v| {
            drop(
                v["pieces"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("contact_mm2"),
            )
        }),
        Box::new(|v| {
            drop(
                v["pieces"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("min_wall_mm"),
            )
        }),
        Box::new(|v| {
            drop(
                v["pieces"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("overhang_regions"),
            )
        }),
        Box::new(|v| drop(v.as_object_mut().unwrap().remove("family"))),
        Box::new(|v| v["pieces"][0]["contact_mm2"] = "456".into()),
        Box::new(|v| v["pieces"][0]["contact_mm2"] = serde_json::Value::Null),
        Box::new(|v| v["pieces"][0]["height_mm"] = true.into()),
        Box::new(|v| v["pieces"][0]["height_mm"] = serde_json::json!([6.0])),
        Box::new(|v| v["pieces"][0]["overhang_regions"][0]["kind"] = "ledge".into()),
        Box::new(|v| v["pieces"][0]["fit_at_bed"][0]["side"] = "both".into()),
        Box::new(|v| v["pieces"][0]["inner_loops"][0]["fit"] = 1.into()),
        Box::new(|v| v["family"] = "PLA; M104 S300".into()),
        Box::new(|v| v["family"] = "pla".into()),
        Box::new(|v| v["piece_count"] = (-1).into()),
        Box::new(|v| v["pieces"] = serde_json::json!({})),
    ];
    for change in &malformed {
        let error = broken(change.as_ref());
        assert_eq!(error.problem, Problem::Malformed, "{error}");
        assert!(!error.to_string().contains("M104"));
    }
    assert_eq!(broken(&|v| v["piece_count"] = 2.into()).fact, "piece_count");
    assert_eq!(
        broken(&|v| v["total_contact_mm2"] = 600.0.into()).fact,
        "total_contact_mm2"
    );
    assert_eq!(
        broken(&|v| v["pieces"][0]["slenderness"] = (-1.0).into()).problem,
        Problem::Negative
    );
    for text in [
        "",
        "[]",
        "{\"family\": \"PLA\"",
        "{\"pieces\": [{\"contact_mm2\": 1e999}]}",
        "NaN",
    ] {
        assert_eq!(
            PlateFacts::from_json(text).unwrap_err().problem,
            Problem::Malformed,
            "{text}"
        );
    }
}

#[test]
fn the_rules_read_the_careful_side_of_a_fact_that_can_be_cross_checked() {
    let mut ring = piece();
    ring.contact_mm2 = 456.0;
    ring.contact_perimeter_mm = 452.0;
    ring.strip_width_mm = 2.0;
    assert_eq!(ring.thin_strip_mm(), 2.0);
    // An overstated strip width cannot hide a thin ring.
    ring.strip_width_mm = 40.0;
    assert!((ring.thin_strip_mm() - 2.0 * 456.0 / 452.0).abs() < 1e-9);
    let mut post = piece();
    post.contact_mm2 = 144.0;
    post.height_mm = 150.0;
    post.slenderness = 12.5;
    assert_eq!(post.tipping(), 12.5);
    // An understated slenderness cannot hide a tall post.
    post.slenderness = 0.5;
    assert_eq!(post.tipping(), 12.5);
}
