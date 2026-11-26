//! Print plans computed by `hotend-settings` through its public API, for the tests that
//! slice or judge with Hotend's print settings.

use hotend_settings::facts::{PieceFacts, PlateFacts};
use hotend_settings::inputs::{
    Capabilities, History, PlanInputs, PrinterFacts, Setup, SlotFeed, Spool,
};
use hotend_settings::model::{Colour, Family, MaterialId, PlateType, ProcessPreset, SpoolSlot};
use hotend_settings::{Changes, PrintPlan, PrintSettings, RuleBook};

/// A plan for one piece of PLA on the Textured PEI plate: one grey spool of `pla-generic` in
/// AMS slot 1, the X2D as the core derives it, the standard preset installed.
pub fn plan_for(piece: PieceFacts) -> PrintPlan {
    let material = MaterialId::parse("pla-generic").unwrap();
    let feeds = vec![SlotFeed {
        slot: SpoolSlot::Ams(1),
        nozzle_id: 1,
        main: true,
    }];
    let inputs = PlanInputs {
        facts: PlateFacts::new(Family::Pla, vec![piece]).unwrap(),
        material: material.clone(),
        setup: Setup::new(
            Some(PlateType::TexturedPei),
            vec![Spool {
                slot: SpoolSlot::Ams(1),
                material_id: material,
                family: Family::Pla,
                colour: Colour::parse("#898989").unwrap(),
                silk: false,
            }],
        )
        .unwrap(),
        spool_choice: None,
        printer: PrinterFacts::new(0.4, 0.28, 70, feeds).unwrap(),
        history: History::default(),
        calibration: None,
        gauge: false,
        intent: None,
        level: None,
        changes: Changes::none(),
        capabilities: Capabilities::none().with_preset(ProcessPreset::Standard020),
        baseline: None,
    };
    RuleBook::builtin().unwrap().plan(&inputs).unwrap()
}

/// A piece by contact area, contact perimeter, contact span and height, with no overhang.
pub fn piece_facts(contact: f64, perimeter: f64, span: f64, height: f64) -> PieceFacts {
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

/// The proven holder recipe as a plan gives it: a thin strip on the bed (2 mm wide) fires the
/// thin-wall brim rule, so level 1: outer brim 5 mm with no gap, first layer 20 and 30 mm/s,
/// bed 65 and 60 C, part fan off for 3 layers, auxiliary fans 0 on the first layer and at most
/// 30 percent, three walls, one grey Generic PLA.
pub fn recipe_settings() -> PrintSettings {
    plan_for(piece_facts(400.0, 400.0, 60.0, 10.0))
        .settings
        .expect("the recipe plan is not blocked")
}

/// A plan with no brim rule: level 0, the card's values for a calm piece.
pub fn stock_settings() -> PrintSettings {
    plan_for(piece_facts(2400.0, 200.0, 60.0, 20.0))
        .settings
        .expect("the calm plan is not blocked")
}

/// A calm piece with walls of 1.6 mm: level 0 (first layer 30 and 50 mm/s) with three wall
/// loops (MAT-1), as the bundled example is planned.
pub fn calm_thin_walled_settings() -> PrintSettings {
    let mut piece = piece_facts(2400.0, 200.0, 60.0, 20.0);
    piece.min_wall_mm = Some(1.6);
    plan_for(piece)
        .settings
        .expect("the calm plan is not blocked")
}

/// A very slender piece (12 by 12 mm, 80 mm tall): level 2 with gentle motion, whose motion
/// limits have never been sliced with.
pub fn slender_settings() -> PrintSettings {
    plan_for(piece_facts(144.0, 48.0, 12.0, 80.0))
        .settings
        .expect("the slender plan is not blocked")
}

/// The settings block a slicer would echo for `settings`, as `(key, value)` lines: the asked
/// value for every field that applies, a made-up value inside the field range for the others,
/// the sticking baseline for an unwritten sticking field. Six-entry keys are echoed per
/// extruder. Keys are Hotend keymap keys; every value is made up here.
pub fn echo_lines(settings: &PrintSettings) -> Vec<(String, String)> {
    use hotend_settings::model::Field;
    use hotend_slicer::keymap::{Form, Keymap, Shape};
    let keymap = Keymap::builtin().unwrap();
    let bed = hotend_slicer::SystemProfiles::builtin()
        .unwrap()
        .bed_type(settings.plate_type().unwrap().as_str())
        .unwrap()
        .clone();
    let mut lines = Vec::new();
    for row in keymap.rows().iter().filter(|row| row.enabled) {
        let value = settings.get(row.field);
        let asked = (Keymap::applies(row, settings) && !value.is_unset())
            .then(|| row.format(&value, &bed.slicer_value))
            .flatten();
        let text = asked.unwrap_or_else(|| match (row.field, row.form) {
            (Field::ElephantFoot, _) => "0.15".to_owned(),
            (Field::FirstLayerHeight, _) => "0.2".to_owned(),
            (Field::FirstLayerLineWidth, _) => "0.5".to_owned(),
            (_, Form::Integer | Form::Decimal2) => {
                let (min, max) = row.range.unwrap();
                hotend_slicer::keymap::decimal_text(((min + max) / 2.0).round())
            }
            (_, Form::Percent) => "15%".to_owned(),
            (_, Form::Flag01) => "0".to_owned(),
            (_, Form::Enum) => row.tokens[0].1.clone(),
            (_, Form::Colour) => "#898989".to_owned(),
            (_, Form::Plate) => bed.slicer_value.clone(),
        });
        let echoed = match row.shape {
            Shape::PerVariant => format!("{text},{text}"),
            _ => text,
        };
        lines.push((row.key.clone(), echoed));
    }
    lines
}

/// [`echo_lines`] as the comment lines of a settings block.
pub fn echo_block(settings: &PrintSettings) -> String {
    echo_lines(settings)
        .iter()
        .map(|(key, value)| format!("; {key} = {value}\n"))
        .collect()
}
