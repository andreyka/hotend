//! Shared test inputs: the measured gauge, the worked examples and a
//! small corpus of plates that reaches every rule.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use hotend_settings::facts::{
    FitAtBed, FitSide, InnerLoop, OverhangRegion, PieceFacts, PlateFacts,
};
use hotend_settings::inputs::{
    Asked, Asker, Capabilities, History, PlanInputs, PrinterFacts, Setup, SlotFeed, Spool,
};
use hotend_settings::model::{
    Colour, Family, Field, Intent, MaterialId, PlateType, PrintPlan, ProcessPreset, SpoolSlot,
    Value,
};
use hotend_settings::signature::{FitSignature, SignatureParts};
use hotend_settings::{Changes, RuleBook};

pub fn book() -> &'static RuleBook {
    RuleBook::builtin().expect("the embedded data files load")
}

/// A piece with a solid rectangular foot of `a` by `b` mm and the given height: no overhang,
/// no holes, no fits.
pub fn block(a: f64, b: f64, height: f64) -> PieceFacts {
    foot(a * b, 2.0 * (a + b), a.max(b), height)
}

/// A piece by contact area, contact perimeter, contact span and height.
pub fn foot(contact: f64, perimeter: f64, span: f64, height: f64) -> PieceFacts {
    PieceFacts {
        contact_mm2: contact,
        contact_perimeter_mm: perimeter,
        strip_width_mm: 2.0 * contact / perimeter,
        contact_span_mm: span,
        height_mm: height,
        slenderness: height / contact.sqrt(),
        overhang_mm2: 0.0,
        fit_overhang_mm2: 0.0,
        overhang_over_part_mm2: 0.0,
        overhang_regions: Some(Vec::new()),
        inner_loops: Vec::new(),
        fit_at_bed: Vec::new(),
        min_wall_mm: None,
    }
}

/// A long strip of the given width on the bed: 600 mm2 of contact, 5 mm tall.
pub fn strip(width: f64) -> PieceFacts {
    foot(600.0, 1200.0 / width, 90.0, 5.0)
}

/// A 2 mm strip that closes around a hole of `hole_mm`; `fit` says whether the hole is a
/// mating face.
pub fn strip_ring(hole_mm: f64, fit: bool) -> PieceFacts {
    let mut piece = strip(2.0);
    piece.inner_loops = vec![InnerLoop {
        min_width_mm: hole_mm,
        fit,
    }];
    piece
}

/// Gives a piece these overhang regions; the totals follow from them.
pub fn with_regions(mut piece: PieceFacts, regions: Vec<OverhangRegion>) -> PieceFacts {
    piece.overhang_mm2 = regions.iter().map(|region| region.area_mm2).sum();
    piece.overhang_over_part_mm2 = regions
        .iter()
        .filter(|region| region.over_part)
        .map(|region| region.area_mm2)
        .sum();
    piece.overhang_regions = Some(regions);
    piece
}

/// A piece the rules have nothing to say about.
pub fn calm() -> PieceFacts {
    block(60.0, 40.0, 20.0)
}

/// The printer as the core derives it for the X2D: every AMS slot and the outside holder of
/// the main nozzle feed device extruder 1; the other outside holder feeds extruder 0.
pub fn printer(family: Family) -> PrinterFacts {
    let bed_limit = match family {
        Family::Pla => 70,
        Family::Petg => 90,
        Family::Tpu => 60,
        Family::Abs | Family::Asa => 110,
    };
    let mut feeds: Vec<SlotFeed> = (1..=4)
        .map(|number| SlotFeed {
            slot: SpoolSlot::Ams(number),
            nozzle_id: 1,
            main: true,
        })
        .collect();
    feeds.push(SlotFeed {
        slot: SpoolSlot::ExternalMain,
        nozzle_id: 1,
        main: true,
    });
    feeds.push(SlotFeed {
        slot: SpoolSlot::ExternalAuxiliary,
        nozzle_id: 0,
        main: false,
    });
    PrinterFacts::new(0.4, 0.28, bed_limit, feeds).unwrap()
}

pub fn material(family: Family) -> MaterialId {
    MaterialId::parse(match family {
        Family::Pla => "pla-generic",
        Family::Petg => "petg-generic",
        Family::Tpu => "tpu-90a",
        Family::Abs => "abs-generic",
        Family::Asa => "asa-generic",
    })
    .unwrap()
}

/// What is verified today: the standard preset only.
pub fn measured() -> Capabilities {
    Capabilities::none().with_preset(ProcessPreset::Standard020)
}

/// Everything installed and verified.
pub fn everything() -> Capabilities {
    let mut caps = Capabilities::none();
    for preset in ProcessPreset::ALL {
        caps = caps.with_preset(*preset);
    }
    for (field, value) in [
        (Field::BrimKind, "none"),
        (Field::BrimKind, "inner"),
        (Field::BrimKind, "outer_and_inner"),
        (Field::InfillPattern, "gyroid"),
        (Field::Seam, "back"),
        (Field::SupportsKind, "normal"),
    ] {
        caps = caps.with_value(field, value).unwrap();
    }
    caps
}

/// Inputs for a plate of `family` on `plate`: one grey spool of the material, in slot 1, or
/// on the outside holder of the main nozzle for TPU.
pub fn inputs(family: Family, plate: PlateType, pieces: Vec<PieceFacts>) -> PlanInputs {
    let slot = if family == Family::Tpu {
        SpoolSlot::ExternalMain
    } else {
        SpoolSlot::Ams(1)
    };
    let spool = Spool {
        slot,
        material_id: material(family),
        family,
        colour: Colour::parse("#898989").unwrap(),
        silk: false,
    };
    PlanInputs {
        facts: PlateFacts::new(family, pieces).unwrap(),
        material: material(family),
        setup: Setup::new(Some(plate), vec![spool]).unwrap(),
        spool_choice: None,
        printer: printer(family),
        history: History::default(),
        calibration: None,
        gauge: false,
        intent: None,
        level: None,
        changes: Changes::none(),
        capabilities: measured(),
        baseline: None,
    }
}

/// A set of accepted changes, all asked for by `by`.
pub fn changes(items: &[(Field, Value)], by: Asker) -> Changes {
    items.iter().fold(Changes::none(), |set, (field, value)| {
        set.with(*field, *value, by)
            .unwrap_or_else(|error| panic!("{field}: {error}"))
    })
}

pub fn plan(inputs: &PlanInputs) -> PrintPlan {
    book().plan(inputs).expect("the rules give a plan")
}

pub fn value(plan: &PrintPlan, field: Field) -> Value {
    plan.value(field)
        .unwrap_or_else(|| panic!("the plan is blocked: {:?}", plan.blocked))
}

pub fn whole(plan: &PrintPlan, field: Field) -> i64 {
    value(plan, field)
        .whole()
        .unwrap_or_else(|| panic!("{field} is not a whole number"))
}

pub fn token(plan: &PrintPlan, field: Field) -> &'static str {
    value(plan, field)
        .token()
        .unwrap_or_else(|| panic!("{field} is not a token"))
}

pub fn person<T>(value: T) -> Option<Asked<T>> {
    Some(Asked {
        value,
        by: Asker::Person,
    })
}

pub fn agent<T>(value: T) -> Option<Asked<T>> {
    Some(Asked {
        value,
        by: Asker::Agent,
    })
}

/// A gauge plate: a ring of 456 mm2 with circles of 74 and 70 mm (a strip of 2.0 mm), and
/// two pieces of 150 and 180 mm2. All three carry mating faces that
/// stand on the bed.
pub fn gauge_pieces() -> Vec<PieceFacts> {
    let mut ring = foot(456.0, 452.0, 74.0, 6.0);
    ring.strip_width_mm = 2.0;
    ring.inner_loops = vec![InnerLoop {
        min_width_mm: 70.0,
        fit: true,
    }];
    ring.fit_at_bed = vec![FitAtBed {
        pair: "sample_rim".to_owned(),
        side: FitSide::Outside,
    }];
    let mut small = block(15.0, 10.0, 6.0);
    small.fit_at_bed = vec![FitAtBed {
        pair: "sample_rim".to_owned(),
        side: FitSide::Inside,
    }];
    let mut other = block(18.0, 10.0, 6.0);
    other.fit_at_bed = vec![FitAtBed {
        pair: "sample_rim".to_owned(),
        side: FitSide::Inside,
    }];
    vec![ring, small, other]
}

pub fn gauge() -> PlanInputs {
    let mut inputs = inputs(Family::Pla, PlateType::TexturedPei, gauge_pieces());
    inputs.gauge = true;
    inputs
}

/// A phone sleeve of TPU 90A.
pub fn sleeve() -> PlanInputs {
    inputs(
        Family::Tpu,
        PlateType::TexturedPei,
        vec![foot(1200.0, 480.0, 160.0, 12.0)],
    )
}

/// 5.9: a PETG post, 150 mm tall on 12 mm square.
pub fn post() -> PlanInputs {
    inputs(
        Family::Petg,
        PlateType::TexturedPei,
        vec![block(12.0, 12.0, 150.0)],
    )
}

/// 5.9: a PLA box with a bore on the bed, an 8 mm cross hole and a ledge that reaches 6 mm.
pub fn box_with_bore(ledge: bool) -> PlanInputs {
    let mut piece = foot(2086.0, 262.8, 60.0, 30.0);
    piece.inner_loops = vec![InnerLoop {
        min_width_mm: 20.0,
        fit: true,
    }];
    piece.fit_at_bed = vec![FitAtBed {
        pair: "axle".to_owned(),
        side: FitSide::Inside,
    }];
    let mut regions = vec![OverhangRegion::hole_ceiling(125.0, 8.0, true)];
    if ledge {
        regions.push(OverhangRegion::free(60.0, 6.0, false));
    }
    let mut inputs = inputs(
        Family::Pla,
        PlateType::TexturedPei,
        vec![with_regions(piece, regions)],
    );
    inputs.capabilities = everything();
    inputs
}

/// 5.9: a PLA lid of 100 by 60 mm with a 2 mm bottom fillet and a 20 mm slot.
pub fn lid() -> PlanInputs {
    let piece = with_regions(
        block(100.0, 60.0, 8.0),
        vec![
            OverhangRegion::free(500.0, 2.0, false),
            OverhangRegion::bridge(80.0, 20.0, true),
        ],
    );
    inputs(Family::Pla, PlateType::TexturedPei, vec![piece])
}

/// The signature the gauge plan must produce, as a later product would have it recorded.
pub fn gauge_signature() -> FitSignature {
    book()
        .expected_signature(&plan(&gauge()))
        .expect("the gauge plan has a signature")
}

pub fn signature(change: impl FnOnce(&mut SignatureParts)) -> FitSignature {
    let mut parts = gauge_signature().parts().clone();
    change(&mut parts);
    FitSignature::new(parts).unwrap()
}

/// A corpus of plates that, between them, reach every rule, every refusal and every line.
pub fn corpus() -> Vec<(String, PlanInputs)> {
    let mut all: Vec<(String, PlanInputs)> = Vec::new();
    let mut push = |name: &str, inputs: PlanInputs| all.push((name.to_owned(), inputs));

    push("gauge", gauge());
    push("sleeve", sleeve());
    push("post", post());
    push("box", box_with_bore(true));
    push("box-hole-only", box_with_bore(false));
    push("lid", lid());

    // Every family on every plate at levels 0 to 3 (through the history).
    for family in Family::ALL.iter().copied() {
        for plate in PlateType::ALL.iter().copied() {
            for level in 0..=3u8 {
                let mut inputs = inputs(family, plate, vec![calm()]);
                inputs.history = History::new(level, None).unwrap();
                push(&format!("{family}/{plate}/L{level}"), inputs);
            }
        }
    }
    // Every intent, with what is verified today and with everything.
    for intent in Intent::ALL.iter().copied() {
        for (label, caps) in [("measured", measured()), ("everything", everything())] {
            let mut inputs = inputs(Family::Pla, PlateType::TexturedPei, vec![calm()]);
            inputs.intent = agent(intent);
            inputs.capabilities = caps;
            push(&format!("intent/{intent}/{label}"), inputs);
        }
    }
    // Each brim rule on its own.
    let rigid = |piece: PieceFacts| inputs(Family::Pla, PlateType::TexturedPei, vec![piece]);
    push("ad1", rigid(foot(600.0, 600.0, 90.0, 5.0)));
    push("ad2-small", rigid(block(20.0, 10.0, 5.0)));
    push("ad2-tiny", rigid(block(9.0, 9.0, 5.0)));
    push("ad3", rigid(block(20.0, 20.0, 60.0)));
    push("ad3-very", rigid(block(20.0, 20.0, 100.0)));
    push("ad4-low", rigid(block(200.0, 30.0, 5.0)));
    push(
        "ad4-medium",
        inputs(
            Family::Petg,
            PlateType::TexturedPei,
            vec![block(150.0, 30.0, 5.0)],
        ),
    );
    push(
        "ad6-thin",
        inputs(
            Family::Tpu,
            PlateType::TexturedPei,
            vec![foot(600.0, 600.0, 90.0, 5.0)],
        ),
    );
    push("thin-wall", {
        let mut piece = calm();
        piece.min_wall_mm = Some(2.0);
        rigid(piece)
    });
    // Level by spool, history and request.
    push("silk", {
        let mut inputs = rigid(calm());
        let mut spool = inputs.setup.spools()[0].clone();
        spool.silk = true;
        inputs.setup = Setup::new(Some(PlateType::TexturedPei), vec![spool]).unwrap();
        inputs
    });
    push("asked-2", {
        let mut inputs = rigid(calm());
        inputs.level = person(2);
        inputs
    });
    push("asked-0-on-ring", {
        let mut inputs = gauge();
        inputs.level = agent(0);
        inputs
    });
    // Supports.
    push("unclassified", {
        let mut piece = calm();
        piece.overhang_mm2 = 300.0;
        piece.overhang_over_part_mm2 = 40.0;
        piece.overhang_regions = None;
        rigid(piece)
    });
    push("fit-overhang", {
        let mut piece = with_regions(calm(), vec![OverhangRegion::free(150.0, 12.0, true)]);
        piece.fit_overhang_mm2 = 30.0;
        let mut inputs = rigid(piece);
        inputs.capabilities = everything();
        inputs
    });
    // Calibration: a product with fit faces on the bed, printed like its gauge and not.
    let product = || {
        let mut piece = calm();
        piece.fit_at_bed = vec![FitAtBed {
            pair: "axle".to_owned(),
            side: FitSide::Outside,
        }];
        let mut inputs = rigid(piece);
        inputs.intent = person(Intent::FitCritical);
        inputs
    };
    push("calibrated", {
        let mut inputs = product();
        inputs.calibration = Some(gauge_signature());
        inputs
    });
    push("calibrated-history", {
        let mut inputs = product();
        inputs.calibration = Some(signature(|parts| {
            parts.first_layer_bed_c = 60;
            parts.layers_bed_c = 55;
        }));
        inputs.history = History::new(2, Some(1)).unwrap();
        inputs
    });
    push("calibrated-values-this-nozzle-cannot-print", {
        let mut inputs = product();
        inputs.calibration = Some(signature(|parts| parts.first_layer_height_mm = 0.28));
        inputs.printer = PrinterFacts::new(
            0.4,
            0.24,
            70,
            vec![SlotFeed {
                slot: SpoolSlot::Ams(1),
                nozzle_id: 1,
                main: true,
            }],
        )
        .unwrap();
        inputs
    });
    // A printer whose limit lies below the card's bed temperature.
    push("bed-limited", {
        let mut inputs = inputs(
            Family::Petg,
            PlateType::TexturedPei,
            vec![block(20.0, 10.0, 5.0)],
        );
        inputs.printer = PrinterFacts::new(
            0.4,
            0.28,
            72,
            vec![SlotFeed {
                slot: SpoolSlot::Ams(1),
                nozzle_id: 1,
                main: true,
            }],
        )
        .unwrap();
        inputs
    });
    // A printer or filament whose limit lies below the card's window: nothing to plan.
    push("bed-limit-below-window", {
        let mut inputs = inputs(Family::Pla, PlateType::TexturedPei, vec![calm()]);
        inputs.printer = PrinterFacts::new(
            0.4,
            0.28,
            40,
            vec![SlotFeed {
                slot: SpoolSlot::Ams(1),
                nozzle_id: 1,
                main: true,
            }],
        )
        .unwrap();
        inputs
    });
    // Many flexible pieces.
    push(
        "tpu-crowd",
        inputs(
            Family::Tpu,
            PlateType::TexturedPei,
            vec![foot(1200.0, 480.0, 160.0, 12.0); 5],
        ),
    );
    // An AMS spool chosen among two.
    push("two-spools", {
        let mut inputs = rigid(calm());
        let first = inputs.setup.spools()[0].clone();
        let second = Spool {
            slot: SpoolSlot::Ams(3),
            colour: Colour::parse("#D01010").unwrap(),
            ..first.clone()
        };
        inputs.setup = Setup::new(Some(PlateType::TexturedPei), vec![first, second]).unwrap();
        inputs.spool_choice = Some(SpoolSlot::Ams(3));
        inputs
    });

    // Changes somebody asked for. A 12 mm brim on the lid is held to 10 mm, and the kind
    // follows the width; a slower first layer on the gauge is taken as asked.
    push("change/brim-on-lid", {
        let mut inputs = lid();
        inputs.changes = changes(&[(Field::BrimWidth, Value::Whole(12))], Asker::Agent);
        inputs
    });
    push("change/slower-first-layer", {
        let mut inputs = gauge();
        inputs.changes = changes(
            &[
                (Field::FirstLayerWallSpeed, Value::Whole(15)),
                (Field::OuterWallSpeedCap, Value::Whole(40)),
            ],
            Asker::Person,
        );
        inputs
    });

    // Refusals.
    push("blocked/no-plate", {
        let mut inputs = rigid(calm());
        inputs.setup = Setup::new(None, inputs.setup.spools().to_vec()).unwrap();
        inputs
    });
    push("blocked/no-spools", {
        let mut inputs = rigid(calm());
        inputs.setup = Setup::new(Some(PlateType::TexturedPei), Vec::new()).unwrap();
        inputs
    });
    push(
        "blocked/other-material",
        inputs_with_material(rigid(calm()), "pla-silk"),
    );
    push("blocked/two-spools", {
        let mut inputs = rigid(calm());
        let first = inputs.setup.spools()[0].clone();
        let second = Spool {
            slot: SpoolSlot::Ams(2),
            ..first.clone()
        };
        inputs.setup = Setup::new(Some(PlateType::TexturedPei), vec![first, second]).unwrap();
        inputs
    });
    push("blocked/tpu-85a", inputs_with_material(sleeve(), "tpu-85a"));
    push("blocked/tpu-in-ams", {
        let mut inputs = sleeve();
        let spool = Spool {
            slot: SpoolSlot::Ams(2),
            ..inputs.setup.spools()[0].clone()
        };
        inputs.setup = Setup::new(Some(PlateType::TexturedPei), vec![spool]).unwrap();
        inputs
    });
    push("blocked/auxiliary-nozzle", {
        let mut inputs = rigid(calm());
        let spool = Spool {
            slot: SpoolSlot::ExternalAuxiliary,
            ..inputs.setup.spools()[0].clone()
        };
        inputs.setup = Setup::new(Some(PlateType::TexturedPei), vec![spool]).unwrap();
        inputs
    });
    // The baseline first layer (0.2 mm high, 0.5 mm wide) is not one a 0.2 mm nozzle prints.
    push("blocked/other-nozzle", {
        let mut inputs = rigid(calm());
        inputs.printer = PrinterFacts::new(
            0.2,
            0.14,
            70,
            vec![SlotFeed {
                slot: SpoolSlot::Ams(1),
                nozzle_id: 1,
                main: true,
            }],
        )
        .unwrap();
        inputs
    });
    all
}

/// A small deterministic generator, so that the properties run over the same plates every
/// time without another dependency.
pub struct Lcg(pub u64);

impl Lcg {
    pub fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }

    pub fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[(self.next() * from.len() as f64) as usize % from.len()]
    }

    pub fn piece(&mut self) -> PieceFacts {
        let a = 4.0 + 220.0 * self.next() * self.next();
        let b = 4.0 + 120.0 * self.next() * self.next();
        let mut piece = block(a, b, 1.0 + 250.0 * self.next() * self.next());
        if self.next() < 0.3 {
            // A ring-like foot: more perimeter for the same area.
            piece.contact_perimeter_mm *= 1.0 + 8.0 * self.next();
            piece.strip_width_mm = 2.0 * piece.contact_mm2 / piece.contact_perimeter_mm;
        }
        if self.next() < 0.3 {
            piece.fit_at_bed = vec![FitAtBed {
                pair: "pair".to_owned(),
                side: FitSide::Inside,
            }];
        }
        if self.next() < 0.3 {
            piece.inner_loops = vec![InnerLoop {
                min_width_mm: 60.0 * self.next(),
                fit: self.next() < 0.5,
            }];
        }
        if self.next() < 0.2 {
            piece.min_wall_mm = Some(0.4 + 4.0 * self.next());
        }
        match (self.next() * 4.0) as u32 {
            0 => {}
            1 => {
                piece.overhang_mm2 = 400.0 * self.next();
                piece.overhang_over_part_mm2 = piece.overhang_mm2 * self.next();
                piece.overhang_regions = None;
            }
            _ => {
                let count = 1 + (self.next() * 4.0) as usize;
                let regions = (0..count)
                    .map(|_| {
                        let area = 150.0 * self.next() * self.next();
                        let over = self.next() < 0.4;
                        match (self.next() * 3.0) as u32 {
                            0 => OverhangRegion::bridge(area, 40.0 * self.next(), over),
                            1 => OverhangRegion::hole_ceiling(area, 16.0 * self.next(), over),
                            _ => OverhangRegion::free(area, 6.0 * self.next(), over),
                        }
                    })
                    .collect();
                piece = with_regions(piece, regions);
                piece.fit_overhang_mm2 = piece.overhang_mm2 * self.next() * self.next();
            }
        }
        piece
    }

    pub fn inputs(&mut self) -> PlanInputs {
        let family = self.pick(Family::ALL);
        let plate = self.pick(&[
            PlateType::TexturedPei,
            PlateType::HighTemp,
            PlateType::Engineering,
            PlateType::Cool,
        ]);
        let count = 1 + (self.next() * 3.0) as usize;
        let mut inputs = inputs(family, plate, (0..count).map(|_| self.piece()).collect());
        if self.next() < 0.5 {
            inputs.capabilities = everything();
        }
        if self.next() < 0.5 {
            inputs.intent = agent(self.pick(Intent::ALL));
        }
        if self.next() < 0.2 {
            let spool = Spool {
                silk: true,
                ..inputs.setup.spools()[0].clone()
            };
            inputs.setup = Setup::new(Some(plate), vec![spool]).unwrap();
        }
        if self.next() < 0.2 {
            inputs.calibration = Some(signature(|parts| {
                parts.material_id = inputs.material.clone();
                parts.first_layer_bed_c = 58;
                parts.layers_bed_c = 56;
            }));
        }
        inputs
    }
}

/// Plates with a proven baseline that reach every sentence of the baseline: a calm PLA plate
/// on the High Temp Plate whose baseline is more careful than the rules in every field it
/// holds, and the same plate on Textured PEI with a bed above the window (held there).
pub fn proven_corpus() -> Vec<(String, PlanInputs)> {
    use hotend_settings::inputs::{Baseline, BaselineValues};
    use hotend_settings::model::{BrimKind, Day};
    let values = BaselineValues {
        level: 1,
        brim_kind: BrimKind::Outer,
        brim_width_mm: 6,
        first_layer_wall_speed_mm_s: 18,
        first_layer_infill_speed_mm_s: 28,
        first_layer_bed_c: 63,
        layers_bed_c: 62,
        part_fan_off_layers: 4,
        aux_fan_first_layers_pct: 0,
        aux_fan_max_pct: 20,
        wall_loops: 4,
    };
    let day = Day::parse("2026-11-17").unwrap();
    let mut all = Vec::new();
    for (name, plate, bed) in [
        ("proven-high-temp", PlateType::HighTemp, 63),
        ("proven-textured-held", PlateType::TexturedPei, 80),
    ] {
        let mut plate_inputs = inputs(Family::Pla, plate, vec![calm()]);
        let values = BaselineValues {
            first_layer_bed_c: bed,
            ..values
        };
        plate_inputs.baseline =
            Some(Baseline::new(plate, material(Family::Pla), day, values).unwrap());
        all.push((name.to_owned(), plate_inputs));
    }
    all
}

/// The first `count` plates of the generator, the same every time.
pub fn generated(count: usize) -> Vec<PlanInputs> {
    let mut lcg = Lcg(0x5EED_2026_1118);
    (0..count).map(|_| lcg.inputs()).collect()
}

/// The same inputs with another material id, on the plate and on the spool.
pub fn inputs_with_material(mut inputs: PlanInputs, id: &str) -> PlanInputs {
    let id = MaterialId::parse(id).unwrap();
    let spools: Vec<Spool> = inputs
        .setup
        .spools()
        .iter()
        .map(|spool| Spool {
            material_id: if id.as_str() == "pla-silk" {
                spool.material_id.clone()
            } else {
                id.clone()
            },
            ..spool.clone()
        })
        .collect();
    inputs.setup = Setup::new(inputs.setup.plate_type(), spools).unwrap();
    inputs.material = id;
    inputs
}

pub fn snapshot(name: &str, actual: &str) {
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
