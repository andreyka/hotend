//! The proof: a sliced file judged against the print settings Hotend asked for
//! (a first, narrow version).
//!
//! The sliced G-code is hostile input. [`scan`] reads it once, with the limits of the output
//! reader, and keeps only parsed values: the settings block (with the keys of values it had to
//! drop), the plate, the filament list, and per first layer the state in force at every
//! extruding move (bed target, part, auxiliary and chamber fans, feed rate per feature). The
//! state is tracked from the first line, so what the start routine sets counts. [`judge`] is
//! pure: it compares that scan with the settings and gives a [`ProofReport`], which holds only
//! numbers, flags, Hotend's own tokens, ids of Hotend's tables and codes. No text of the file
//! is copied into it.
//!
//! Order, each a precondition of the next:
//! 1. completeness: the scan read the whole file, the settings block ended inside the bytes
//!    kept for it, every enabled keymap key is stated once and not dropped, the first two
//!    layer markers and a third layer were seen, every extruding move of the first layer
//!    carries a feature of the closed list, and its feed rate, bed target and fan states were
//!    commanded. Otherwise `proof_incomplete`: a row never passes because nothing was seen;
//! 2. the echo: every enabled keymap row's stated value lies in its field's hard range
//!    (`profile_value_out_of_range`); every field Hotend asked for states the asked value
//!    (a limit: at most the limit, and one of the values the helper wrote); an unwritten
//!    sticking field states the recorded baseline of rule STK-1 (`setting_not_applied`);
//! 3. the measured facts, read from the commands: the plate, one filament, the bed target at
//!    every extruding move of layer 1 and at the first of layer 3, the part fan off and the
//!    auxiliary fans at most the asked percentage on layer 1, the part fan first on after the
//!    asked number of layers, the feed rates of layer 1 per feature, the brim present exactly
//!    when one is asked for, and no prime tower.
//!
//! The marker words, the fan words and the feature list are printer-family facts. They are in
//! a slicer-side data table (`profiles/bambu-gcode-facts.toml`) for now, because this crate does
//! not depend on `hotend-printer`, where they belong.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::OnceLock;

use hotend_settings::model::{Field, PrintSettings, Value};
use serde::{Deserialize, Serialize};

use crate::error::SlicerError;
use crate::keymap::{Echo, Keymap, Op, Row};
use crate::output::gcode::ScanLimits;
use crate::system::{BedType, SystemProfiles};

const FACTS_TOML: &str = include_str!("../profiles/bambu-gcode-facts.toml");
const FACTS_FILE: &str = "profiles/bambu-gcode-facts.toml";

/// Most settings kept from one settings block.
const MAX_CONFIG_KEYS: usize = 4096;
/// Longest setting value kept; a longer one is recorded as dropped, by its key.
const MAX_CONFIG_VALUE: usize = 512;
/// Most entries an echoed value may have.
const MAX_ECHO_ENTRIES: usize = 16;
/// How far a commanded bed temperature may lie from the asked one.
const BED_SLACK_C: f64 = 1.0;
/// How far a first layer feed rate may lie above the asked speed.
const SPEED_SLACK_MM_S: f64 = 0.5;

/// Which first layer speed holds for a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bound {
    Wall,
    Fill,
}

/// One feature of the closed list.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feature {
    pub name: String,
    pub bound: Bound,
    pub verified: bool,
}

/// The printer-family facts the proof reads (`profiles/bambu-gcode-facts.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GcodeFacts {
    schema: u32,
    pub revision: String,
    pub measured_on: String,
    pub layer_marker: String,
    pub feature_prefix: String,
    pub config_start: String,
    pub config_end: String,
    pub part_fans: Vec<u8>,
    pub aux_fans: Vec<u8>,
    pub chamber_fan: u8,
    pub tower_word: String,
    pub brim_feature: String,
    pub feature: Vec<Feature>,
}

impl GcodeFacts {
    /// The table compiled into this build.
    pub fn builtin() -> Result<&'static GcodeFacts, SlicerError> {
        static BUILTIN: OnceLock<Result<GcodeFacts, String>> = OnceLock::new();
        BUILTIN
            .get_or_init(|| GcodeFacts::from_toml(FACTS_TOML))
            .as_ref()
            .map_err(|message| SlicerError::Data {
                file: FACTS_FILE,
                message: message.clone(),
            })
    }

    /// Parses and checks a table in the format of the data file.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let facts: GcodeFacts = toml::from_str(text).map_err(|e| e.to_string())?;
        if facts.schema != 1 {
            return Err(format!("unsupported schema {}", facts.schema));
        }
        let word = |text: &str| {
            (1..=40).contains(&text.len())
                && text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | ' '))
                && !text.starts_with(' ')
                && !text.ends_with(' ')
        };
        let words = [
            &facts.layer_marker,
            &facts.feature_prefix,
            &facts.config_start,
            &facts.config_end,
            &facts.tower_word,
            &facts.brim_feature,
        ];
        if !words.iter().all(|text| word(text)) {
            return Err("a marker word has a character that is not allowed".into());
        }
        if facts.feature.is_empty()
            || facts.feature.iter().any(|feature| {
                !word(&feature.name) || feature.name.contains(':') || !feature.verified
            })
        {
            return Err("every feature is a plain, measured name".into());
        }
        if !facts
            .feature
            .iter()
            .any(|feature| feature.name == facts.brim_feature)
        {
            return Err("the brim feature is not in the feature list".into());
        }
        let fan_ok = |index: &u8| *index <= 15;
        if facts.part_fans.is_empty()
            || facts.aux_fans.is_empty()
            || !facts.part_fans.iter().chain(&facts.aux_fans).all(fan_ok)
            || !fan_ok(&facts.chamber_fan)
        {
            return Err("the fan words are not plain fan indices".into());
        }
        Ok(facts)
    }

    fn feature_index(&self, name: &str) -> Option<usize> {
        self.feature.iter().position(|feature| feature.name == name)
    }
}

/// The first layer as the commands set it: the state in force at its extruding moves.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FirstLayer {
    /// Extruding moves in X or Y.
    pub moves: u64,
    /// Moves under no feature or a feature that is not in the closed list.
    pub unlisted: u64,
    /// The highest feed rate per feature of the closed list, in mm/s.
    pub feed_max_mm_s: BTreeMap<usize, f64>,
    /// Moves with no feed rate commanded before them.
    pub feed_unknown: u64,
    /// The lowest and highest bed target in force.
    pub bed_c: Option<[f64; 2]>,
    pub bed_unknown: u64,
    /// The highest part fan value (0 to 255) in force.
    pub part_fan_max: Option<f64>,
    pub part_fan_unknown: u64,
    /// The highest auxiliary fan value (0 to 255) in force.
    pub aux_fan_max: Option<f64>,
    pub aux_fan_unknown: u64,
    /// The highest chamber fan value (0 to 255) in force.
    pub chamber_fan_max: Option<f64>,
    /// Moves under the brim feature.
    pub brim_moves: u64,
}

/// What one G-code stream shows the proof. Parsed values only.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProofScan {
    /// The settings block, by key. Values are kept only as long as a value may be; they are
    /// parsed by [`judge`] and never copied into a report.
    pub config: BTreeMap<String, String>,
    /// Keys stated twice.
    pub duplicate: BTreeSet<String>,
    /// Keys whose value was too long to keep.
    pub dropped: BTreeSet<String>,
    /// The settings block started and ended inside the bytes kept for it.
    pub config_complete: bool,
    /// The filament numbers of the header's `; filament:` line.
    pub filament_ids: Vec<u32>,
    /// Layer markers seen.
    pub layers_seen: u32,
    pub first_layer: FirstLayer,
    /// The bed target at the first extruding move of layer 3: `Some(None)` when it was never
    /// commanded, `None` when layer 3 had no extruding move.
    pub layer_3_bed_c: Option<Option<f64>>,
    /// The layer of the first part fan command above 0 after the first layer marker.
    pub first_part_fan_on_layer: Option<u32>,
    /// A feature whose name contains the tower word was seen.
    pub tower_seen: bool,
    /// The stream was longer than the scan limit or could not be read to its end.
    pub incomplete: bool,
}

struct State<'a> {
    facts: &'a GcodeFacts,
    scan: ProofScan,
    in_config: bool,
    config_started: bool,
    layer: u32,
    feature: Option<Option<usize>>,
    feature_is_brim: bool,
    feed_mm_min: Option<f64>,
    bed_c: Option<f64>,
    fans: [Option<f64>; 16],
    e_absolute: bool,
    e: f64,
}

impl State<'_> {
    fn comment(&mut self, body: &str, in_header: bool, too_long: bool) {
        let facts = self.facts;
        if body == facts.config_start && in_header {
            self.in_config = true;
            self.config_started = true;
            return;
        }
        if body == facts.config_end {
            if self.in_config && in_header {
                self.scan.config_complete = self.config_started;
            }
            self.in_config = false;
            return;
        }
        if self.in_config && in_header {
            let (key, value) = match body.split_once(" = ") {
                Some((key, value)) => (key.trim(), Some(value.trim())),
                None => match body.strip_suffix(" =") {
                    Some(key) => (key.trim(), Some("")),
                    None => (body.split(' ').next().unwrap_or(""), None),
                },
            };
            let plain = !key.is_empty()
                && key.len() <= 64
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !plain {
                return;
            }
            match value {
                Some(value) if !too_long && value.len() <= MAX_CONFIG_VALUE => {
                    if self.scan.config.contains_key(key) {
                        self.scan.duplicate.insert(key.to_owned());
                    } else if self.scan.config.len() < MAX_CONFIG_KEYS {
                        self.scan.config.insert(key.to_owned(), value.to_owned());
                    } else {
                        self.scan.dropped.insert(key.to_owned());
                    }
                }
                _ if self.scan.dropped.len() < MAX_CONFIG_KEYS => {
                    self.scan.dropped.insert(key.to_owned());
                }
                _ => {}
            }
            return;
        }
        if too_long {
            return;
        }
        if body == facts.layer_marker {
            self.layer = self.layer.saturating_add(1);
            self.scan.layers_seen = self.layer;
            return;
        }
        if let Some(name) = body.strip_prefix(facts.feature_prefix.as_str()) {
            let name = name.trim();
            if name
                .to_ascii_lowercase()
                .contains(&facts.tower_word.to_ascii_lowercase())
            {
                self.scan.tower_seen = true;
            }
            self.feature = Some(facts.feature_index(name));
            self.feature_is_brim = name == facts.brim_feature;
            return;
        }
        if in_header
            && !self.config_started
            && self.scan.filament_ids.is_empty()
            && let Some(rest) = body.strip_prefix("filament:")
        {
            let ids: Option<Vec<u32>> = rest
                .split(',')
                .take(65)
                .map(|id| {
                    id.trim()
                        .parse::<u32>()
                        .ok()
                        .filter(|id| (1..=64).contains(id))
                })
                .collect();
            self.scan.filament_ids = ids.filter(|ids| ids.len() <= 64).unwrap_or_default();
        }
    }

    fn command(&mut self, line: &str) {
        let mut words = line.split_whitespace();
        let Some(word) = words.next() else {
            return;
        };
        let mut params: [Option<f64>; 7] = [None; 7];
        const LETTERS: [char; 7] = ['X', 'Y', 'E', 'F', 'S', 'P', 'Z'];
        for token in words.take(16) {
            let mut chars = token.chars();
            let Some(letter) = chars.next().map(|c| c.to_ascii_uppercase()) else {
                continue;
            };
            if let Some(index) = LETTERS.iter().position(|known| *known == letter)
                && params[index].is_none()
            {
                params[index] = chars
                    .as_str()
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite() && value.abs() <= 100_000.0);
            }
        }
        let [x, y, e, f, s, p, _z] = params;
        match word.to_ascii_uppercase().as_str() {
            "M140" | "M190" => {
                if let Some(s) = s.filter(|s| (0.0..=1000.0).contains(s)) {
                    self.bed_c = Some(s);
                }
            }
            "M106" | "M107" => {
                let index = p.map_or(Some(0), |p| {
                    (p.fract() == 0.0 && (0.0..16.0).contains(&p)).then_some(p as usize)
                });
                let Some(index) = index else {
                    return;
                };
                let value = if word.eq_ignore_ascii_case("M107") {
                    Some(0.0)
                } else {
                    s.filter(|s| (0.0..=255.0).contains(s))
                };
                if let Some(value) = value {
                    self.fans[index] = Some(value);
                    let part = self
                        .facts
                        .part_fans
                        .iter()
                        .any(|fan| usize::from(*fan) == index);
                    if part
                        && value > 0.0
                        && self.layer >= 1
                        && self.scan.first_part_fan_on_layer.is_none()
                    {
                        self.scan.first_part_fan_on_layer = Some(self.layer);
                    }
                }
            }
            "M82" => self.e_absolute = true,
            "M83" => self.e_absolute = false,
            "G92" => {
                if let Some(e) = e {
                    self.e = e;
                }
            }
            "G0" | "G1" | "G2" | "G3" => {
                if let Some(f) = f.filter(|f| *f > 0.0) {
                    self.feed_mm_min = Some(f);
                }
                let extruded = match e {
                    None => 0.0,
                    Some(e) if self.e_absolute => {
                        let delta = e - self.e;
                        self.e = e;
                        delta
                    }
                    Some(e) => e,
                };
                if extruded > 0.0 && (x.is_some() || y.is_some()) {
                    self.extruding_move();
                }
            }
            _ => {}
        }
    }

    fn extruding_move(&mut self) {
        let facts = self.facts;
        if self.layer == 3 && self.scan.layer_3_bed_c.is_none() {
            self.scan.layer_3_bed_c = Some(self.bed_c);
        }
        if self.layer != 1 {
            return;
        }
        let first = &mut self.scan.first_layer;
        first.moves += 1;
        match self.feature {
            Some(Some(index)) => match self.feed_mm_min {
                Some(feed) => {
                    let speed = feed / 60.0;
                    let known = first.feed_max_mm_s.entry(index).or_insert(speed);
                    *known = known.max(speed);
                }
                None => first.feed_unknown += 1,
            },
            _ => first.unlisted += 1,
        }
        if self.feature_is_brim {
            first.brim_moves += 1;
        }
        match self.bed_c {
            Some(bed) => {
                first.bed_c = Some(
                    first
                        .bed_c
                        .map_or([bed, bed], |[low, high]| [low.min(bed), high.max(bed)]),
                );
            }
            None => first.bed_unknown += 1,
        }
        let max = |known: &mut Option<f64>, value: f64| {
            *known = Some(known.map_or(value, |known| known.max(value)));
        };
        for fan in &facts.part_fans {
            match (self.fans[usize::from(*fan)], *fan) {
                (Some(value), _) => max(&mut first.part_fan_max, value),
                // `M106` without `P` must be known; a part fan word that was never used is
                // not one the file drives.
                (None, 0) => first.part_fan_unknown += 1,
                (None, _) => {}
            }
        }
        for fan in &facts.aux_fans {
            match self.fans[usize::from(*fan)] {
                Some(value) => max(&mut first.aux_fan_max, value),
                None => first.aux_fan_unknown += 1,
            }
        }
        if let Some(value) = self.fans[usize::from(facts.chamber_fan)] {
            max(&mut first.chamber_fan_max, value);
        }
    }
}

/// Scans a G-code stream for the proof. `limits` are those of the output reader: the header
/// and the settings block are read only from the first `header_bytes`, at most `scan_bytes`
/// are read, and lines over `line_bytes` are not looked at (the key of a setting on such a
/// line is recorded as dropped).
pub fn scan<R: Read>(mut reader: R, facts: &GcodeFacts, limits: &ScanLimits) -> ProofScan {
    let mut state = State {
        facts,
        scan: ProofScan::default(),
        in_config: false,
        config_started: false,
        layer: 0,
        feature: None,
        feature_is_brim: false,
        feed_mm_min: None,
        bed_c: None,
        fans: [None; 16],
        e_absolute: true,
        e: 0.0,
    };
    let mut line: Vec<u8> = Vec::new();
    let mut too_long = false;
    let mut line_start: u64 = 0;
    let mut offset: u64 = 0;
    let mut chunk = vec![0u8; 64 * 1024];
    let finish = |state: &mut State, line: &mut Vec<u8>, too_long: bool, start: u64| {
        // A line over the cap keeps its first bytes, enough to name the key it holds.
        let text = String::from_utf8_lossy(line);
        let text = text.trim();
        match text.strip_prefix(';') {
            Some(body) => state.comment(body.trim(), start < limits.header_bytes, too_long),
            None if !too_long => {
                let code = text.split(';').next().unwrap_or("");
                if !code.is_empty() {
                    state.command(code);
                }
            }
            None => {}
        }
        line.clear();
    };
    loop {
        let wanted = (limits.scan_bytes - offset).min(chunk.len() as u64) as usize;
        if wanted == 0 {
            let mut probe = [0u8; 1];
            state.scan.incomplete = !matches!(reader.read(&mut probe), Ok(0));
            break;
        }
        let read = match reader.read(&mut chunk[..wanted]) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => {
                state.scan.incomplete = true;
                break;
            }
        };
        for &byte in &chunk[..read] {
            offset += 1;
            if byte == b'\n' {
                finish(&mut state, &mut line, too_long, line_start);
                too_long = false;
                line_start = offset;
            } else if line.len() < limits.line_bytes {
                line.push(byte);
            } else {
                too_long = true;
            }
        }
    }
    if !state.scan.incomplete {
        finish(&mut state, &mut line, too_long, line_start);
    }
    state.scan
}

/// The values of rule STK-1 that an unwritten sticking field must state.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Baseline {
    pub elephant_foot_mm: f64,
    pub first_layer_height_mm: f64,
    pub first_layer_line_width_mm: f64,
    /// The first layer acceleration must be at most this.
    pub first_layer_accel_mm_s2: f64,
}

impl Baseline {
    /// The baseline of `hotend-settings`' rule data.
    pub fn builtin() -> Result<Self, SlicerError> {
        let book = hotend_settings::RuleBook::builtin().map_err(|error| SlicerError::Data {
            file: "hotend-settings data/print-rules.toml",
            message: error.to_string(),
        })?;
        let baseline = &book.data().baseline;
        Ok(Self {
            elephant_foot_mm: baseline.elephant_foot_mm,
            first_layer_height_mm: baseline.first_layer_height_mm,
            first_layer_line_width_mm: baseline.first_layer_line_width_mm,
            first_layer_accel_mm_s2: baseline.first_layer_accel_mm_s2 as f64,
        })
    }

    /// The value an unwritten `field` must state, and whether it is a ceiling.
    fn of(&self, field: Field) -> Option<(f64, bool)> {
        match field {
            Field::ElephantFoot => Some((self.elephant_foot_mm, false)),
            Field::FirstLayerHeight => Some((self.first_layer_height_mm, false)),
            Field::FirstLayerLineWidth => Some((self.first_layer_line_width_mm, false)),
            Field::FirstLayerAccel => Some((self.first_layer_accel_mm_s2, true)),
            _ => None,
        }
    }
}

/// What a sliced file must show.
#[derive(Debug, Clone, PartialEq)]
pub struct Expected {
    /// The settings that were asked for.
    pub settings: PrintSettings,
    /// The plate the slice was for.
    pub bed: BedType,
    /// The new entries the profile helper wrote, by key; `None` for a file sliced elsewhere.
    pub written: Option<BTreeMap<String, Vec<String>>>,
    pub baseline: Baseline,
}

impl Expected {
    /// The expectation for `settings`, with the plate of the settings from Hotend's table.
    pub fn for_settings(
        settings: &PrintSettings,
        written: Option<BTreeMap<String, Vec<String>>>,
    ) -> Result<Self, SlicerError> {
        let plate = settings.plate_type().ok_or_else(|| {
            SlicerError::InvalidJob("the print settings name no plate type".into())
        })?;
        let bed = SystemProfiles::builtin()?.bed_type(plate.as_str())?.clone();
        Ok(Self {
            settings: settings.clone(),
            bed,
            written,
            baseline: Baseline::builtin()?,
        })
    }
}

/// How a proof ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofStatus {
    Passed,
    Failed,
    Incomplete,
}

/// One keymap row: what was asked and what the file states, parsed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EchoRow {
    pub field: Field,
    /// The key, from Hotend's keymap.
    pub key: String,
    pub asked: Option<Echo>,
    pub echoed: Vec<Echo>,
    pub ok: bool,
    pub code: Option<&'static str>,
}

/// One measured fact.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FactRow {
    pub id: &'static str,
    pub asked: Option<f64>,
    pub measured: Option<f64>,
    pub ok: bool,
    pub code: Option<&'static str>,
}

/// The outcome of a proof. Parsed values, Hotend's tokens and codes only.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProofReport {
    pub status: ProofStatus,
    /// The code of the first failure.
    pub code: Option<&'static str>,
    /// What made the proof incomplete, in Hotend's words (keys of its own keymap at most).
    pub missing: Vec<String>,
    pub echo: Vec<EchoRow>,
    pub facts: Vec<FactRow>,
}

impl ProofReport {
    /// Whether every row passed.
    pub fn passed(&self) -> bool {
        self.status == ProofStatus::Passed
    }

    /// A plain sentence on what failed, built from field names and parsed values.
    pub fn sentence(&self) -> String {
        if self.status == ProofStatus::Incomplete {
            return format!(
                "the sliced file does not show enough to compare it with the print settings \
                 ({}), so it is not used",
                self.missing.join("; ")
            );
        }
        let mut parts = Vec::new();
        for row in self.echo.iter().filter(|row| !row.ok).take(4) {
            let echoed: Vec<String> = row.echoed.iter().map(ToString::to_string).collect();
            let what = match (row.code, &row.asked) {
                (Some("profile_value_out_of_range"), _) => format!(
                    "the {} is {} in the sliced file, outside what Hotend allows for it",
                    row.field.plain_name(),
                    echoed.join(" and ")
                ),
                (_, Some(asked)) => format!(
                    "the {}: asked {asked}, the sliced file states {}",
                    row.field.plain_name(),
                    echoed.join(" and ")
                ),
                _ => format!(
                    "the {} is {} in the sliced file, not the value Hotend counts on; the \
                     installed profile holds another value",
                    row.field.plain_name(),
                    echoed.join(" and ")
                ),
            };
            parts.push(what);
        }
        for fact in self.facts.iter().filter(|fact| !fact.ok).take(4) {
            parts.push(fact_sentence(fact));
        }
        format!("{}. Nothing of this slice is used", parts.join("; "))
    }

    /// The error for a proof that did not pass.
    pub fn error(&self) -> SlicerError {
        SlicerError::Proof {
            code: self.code.unwrap_or("proof_incomplete"),
            message: self.sentence(),
        }
    }
}

fn number(value: Option<f64>) -> String {
    value.map_or_else(|| "nothing".to_owned(), crate::keymap::decimal_text)
}

fn fact_sentence(fact: &FactRow) -> String {
    let (asked, measured) = (number(fact.asked), number(fact.measured));
    match fact.id {
        "plate" => "the sliced file is not for the plate that was asked for".to_owned(),
        "one_filament" => format!("the sliced file uses {measured} filaments instead of one"),
        "bed_first_layer" => {
            format!("the bed is at {measured} C during the first layer instead of {asked} C")
        }
        "bed_layer_3" => format!("the bed is at {measured} C on layer 3 instead of {asked} C"),
        "part_fan_first_layer" => format!(
            "the part cooling fan runs at {measured} of 255 during the first layer instead of \
             being off"
        ),
        "aux_fans_first_layer" => format!(
            "the auxiliary fans run at {measured} of 255 during the first layer, above the \
             asked {asked} percent"
        ),
        "part_fan_delay" => format!(
            "the part cooling fan starts on layer {measured}, before layer {asked} would allow"
        ),
        "first_layer_wall_speed" => format!(
            "brim and walls of the first layer are printed at up to {measured} mm/s instead of \
             at most {asked} mm/s"
        ),
        "first_layer_fill_speed" => format!(
            "the bottom of the first layer is printed at up to {measured} mm/s instead of at \
             most {asked} mm/s"
        ),
        "brim" if fact.asked.is_some_and(|width| width > 0.0) => {
            "the first layer has no brim although one was asked for".to_owned()
        }
        "brim" => "the first layer has a brim although none was asked for".to_owned(),
        "prime_tower" => "the sliced file prints a prime tower".to_owned(),
        _ => format!("{}: asked {asked}, measured {measured}", fact.id),
    }
}

/// The asked number of a whole or decimal field.
fn asked_number(settings: &PrintSettings, field: Field) -> Option<f64> {
    settings.get(field).number()
}

fn echo_row(row: &Row, expected: &Expected, beds: &[BedType], text: &str) -> EchoRow {
    let settings = &expected.settings;
    let entries = row.echo_entries(text);
    let echoed: Vec<Echo> = if entries.is_empty() || entries.len() > MAX_ECHO_ENTRIES {
        vec![Echo::Unreadable]
    } else {
        entries
            .iter()
            .map(|entry| row.parse_echo(entry, beds))
            .collect()
    };
    let value = settings.get(row.field);
    let asked = (Keymap::applies(row, settings) && !value.is_unset())
        .then(|| row.expected(&value, &expected.bed.id))
        .flatten();
    let mut code = None;
    if !echoed.iter().all(|echo| row.echo_in_range(echo)) {
        code = Some("profile_value_out_of_range");
    } else if let Some(asked) = &asked {
        let ok = match (row.op, asked) {
            (Op::Cap, Echo::Number(cap)) => {
                let written: Option<Vec<f64>> = expected
                    .written
                    .as_ref()
                    .and_then(|written| written.get(&row.key))
                    .map(|new| {
                        new.iter()
                            .filter_map(|text| crate::keymap::plain_number(text))
                            .collect()
                    });
                echoed.iter().all(|echo| match echo {
                    Echo::Number(number) => {
                        *number <= cap + 1e-9
                            && written.as_ref().is_none_or(|written| {
                                written.iter().any(|value| (value - number).abs() < 0.005)
                            })
                    }
                    _ => false,
                })
            }
            _ => echoed.iter().all(|echo| echo.same(asked)),
        };
        if !ok {
            code = Some("setting_not_applied");
        }
    } else if value.is_unset()
        && let Some((baseline, ceiling)) = expected.baseline.of(row.field)
    {
        let ok = echoed.iter().all(|echo| match echo {
            Echo::Number(number) if ceiling => *number <= baseline + 1e-9,
            Echo::Number(number) => (number - baseline).abs() < 0.005,
            _ => false,
        });
        if !ok {
            code = Some("setting_not_applied");
        }
    }
    EchoRow {
        field: row.field,
        key: row.key.clone(),
        asked,
        echoed,
        ok: code.is_none(),
        code,
    }
}

/// Judges a scan against what was asked. Pure.
pub fn judge(
    expected: &Expected,
    scan: &ProofScan,
    keymap: &Keymap,
    facts: &GcodeFacts,
    beds: &[BedType],
) -> ProofReport {
    let settings = &expected.settings;
    let rows: Vec<&Row> = keymap.rows().iter().filter(|row| row.enabled).collect();

    // 1. Completeness.
    let mut missing = Vec::new();
    if scan.incomplete {
        missing
            .push("the file is longer than Hotend reads, or could not be read to its end".into());
    }
    if !scan.config_complete {
        missing.push("the settings block is missing or does not end where Hotend reads it".into());
    }
    let absent: Vec<&str> = rows
        .iter()
        .filter(|row| !scan.config.contains_key(&row.key))
        .map(|row| row.key.as_str())
        .collect();
    if scan.config_complete && !absent.is_empty() {
        missing.push(format!("settings not stated: {}", absent.join(", ")));
    }
    let doubtful: Vec<&str> = rows
        .iter()
        .filter(|row| scan.duplicate.contains(&row.key) || scan.dropped.contains(&row.key))
        .map(|row| row.key.as_str())
        .collect();
    if !doubtful.is_empty() {
        missing.push(format!(
            "settings stated twice or too long to read: {}",
            doubtful.join(", ")
        ));
    }
    let first = &scan.first_layer;
    if scan.layers_seen < 3 {
        missing.push("fewer than three layer markers".into());
    } else if first.moves == 0 {
        missing.push("no extruding move on the first layer".into());
    }
    if first.unlisted > 0 {
        missing.push(
            "a first layer move under a feature Hotend has not measured, or under none".into(),
        );
    }
    if first.feed_unknown > 0 {
        missing.push("a first layer move without a commanded speed".into());
    }
    if scan.layers_seen >= 3 && scan.layer_3_bed_c.is_none() {
        missing.push("no extruding move on layer 3".into());
    }
    if scan.filament_ids.is_empty() {
        missing.push("the header does not list the filaments".into());
    }
    if !missing.is_empty() {
        return ProofReport {
            status: ProofStatus::Incomplete,
            code: Some("proof_incomplete"),
            missing,
            echo: Vec::new(),
            facts: Vec::new(),
        };
    }

    // 2. The echo.
    let echo: Vec<EchoRow> = rows
        .iter()
        .map(|row| echo_row(row, expected, beds, &scan.config[&row.key]))
        .collect();

    // 3. The measured facts.
    let mut fact_rows = Vec::new();
    let mut fact = |id: &'static str,
                    asked: Option<f64>,
                    measured: Option<f64>,
                    ok: bool,
                    code: &'static str| {
        fact_rows.push(FactRow {
            id,
            asked,
            measured,
            ok,
            code: (!ok).then_some(code),
        });
    };
    let plate_ok = scan
        .config
        .get("curr_bed_type")
        .is_some_and(|stated| *stated == expected.bed.slicer_value);
    fact("plate", None, None, plate_ok, "bed_type_not_applied");
    let filaments = scan.filament_ids.len() as f64;
    fact(
        "one_filament",
        Some(1.0),
        Some(filaments),
        scan.filament_ids == [1],
        "unexpected_filament_change",
    );
    let first_bed = asked_number(settings, Field::FirstLayerBed);
    let worst = |range: Option<[f64; 2]>, asked: Option<f64>| match (range, asked) {
        (Some([low, high]), Some(asked)) => Some(if (low - asked).abs() > (high - asked).abs() {
            low
        } else {
            high
        }),
        (Some([_, high]), None) => Some(high),
        _ => None,
    };
    let measured = worst(first.bed_c, first_bed);
    let bed_ok = first.bed_unknown == 0
        && matches!((measured, first_bed), (Some(m), Some(a)) if (m - a).abs() <= BED_SLACK_C);
    fact(
        "bed_first_layer",
        first_bed,
        measured,
        bed_ok,
        "bed_temperature_not_applied",
    );
    let layers_bed = asked_number(settings, Field::LayersBed);
    let layer_3 = scan.layer_3_bed_c.flatten();
    let bed_3_ok =
        matches!((layer_3, layers_bed), (Some(m), Some(a)) if (m - a).abs() <= BED_SLACK_C);
    fact(
        "bed_layer_3",
        layers_bed,
        layer_3,
        bed_3_ok,
        "bed_temperature_not_applied",
    );
    let part_ok = first.part_fan_unknown == 0 && first.part_fan_max.is_some_and(|max| max == 0.0);
    fact(
        "part_fan_first_layer",
        Some(0.0),
        first.part_fan_max,
        part_ok,
        "fan_on_first_layer",
    );
    let aux_pct = asked_number(settings, Field::AuxFanFirstLayers);
    let aux_ok = first.aux_fan_unknown == 0
        && matches!((first.aux_fan_max, aux_pct), (Some(max), Some(pct)) if max / 255.0 * 100.0 <= pct + 0.5);
    fact(
        "aux_fans_first_layer",
        aux_pct,
        first.aux_fan_max,
        aux_ok,
        "fan_on_first_layer",
    );
    let off_layers = asked_number(settings, Field::PartFanOffLayers);
    let delay_ok = match (scan.first_part_fan_on_layer, off_layers) {
        (None, Some(_)) => true,
        (Some(layer), Some(off)) => f64::from(layer) > off,
        _ => false,
    };
    fact(
        "part_fan_delay",
        off_layers.map(|off| off + 1.0),
        scan.first_part_fan_on_layer.map(f64::from),
        delay_ok,
        "fan_on_first_layer",
    );
    for (id, bound, field) in [
        (
            "first_layer_wall_speed",
            Bound::Wall,
            Field::FirstLayerWallSpeed,
        ),
        (
            "first_layer_fill_speed",
            Bound::Fill,
            Field::FirstLayerInfillSpeed,
        ),
    ] {
        let asked = asked_number(settings, field);
        let measured = first
            .feed_max_mm_s
            .iter()
            .filter(|(index, _)| facts.feature.get(**index).is_some_and(|f| f.bound == bound))
            .map(|(_, speed)| *speed)
            .fold(None, |known: Option<f64>, speed| {
                Some(known.map_or(speed, |k| k.max(speed)))
            });
        let ok = match (measured, asked) {
            (Some(measured), Some(asked)) => measured <= asked + SPEED_SLACK_MM_S,
            (None, Some(_)) => true,
            _ => false,
        };
        fact(id, asked, measured, ok, "first_layer_too_fast");
    }
    let width = asked_number(settings, Field::BrimWidth).unwrap_or(0.0);
    let brim_asked = width > 0.0 && settings.get(Field::BrimKind) != Value::Token("none");
    let brim_ok = (first.brim_moves > 0) == brim_asked;
    fact(
        "brim",
        Some(if brim_asked { width } else { 0.0 }),
        Some(first.brim_moves as f64),
        brim_ok,
        if brim_asked {
            "brim_missing"
        } else {
            "brim_unexpected"
        },
    );
    fact(
        "prime_tower",
        Some(0.0),
        Some(if scan.tower_seen { 1.0 } else { 0.0 }),
        !scan.tower_seen,
        "unexpected_prime_tower",
    );
    // Reported, never judged: the start routine sets it and no key controls it.
    fact(
        "chamber_fan_first_layer",
        None,
        first.chamber_fan_max,
        true,
        "",
    );

    let code = echo
        .iter()
        .find_map(|row| row.code)
        .or_else(|| fact_rows.iter().find_map(|fact| fact.code));
    ProofReport {
        status: if code.is_some() {
            ProofStatus::Failed
        } else {
            ProofStatus::Passed
        },
        code,
        missing: Vec::new(),
        echo,
        facts: fact_rows,
    }
}

/// Reads the sliced `.gcode.3mf` at `path` as hostile input and judges it. A file Hotend did
/// not make is read the same way.
pub fn check_sliced_file(
    path: &std::path::Path,
    plate: u32,
    expected: &Expected,
) -> Result<ProofReport, SlicerError> {
    let keymap = Keymap::builtin()?;
    let facts = GcodeFacts::builtin()?;
    let beds = &SystemProfiles::builtin()?.bed_types;
    let file = std::fs::File::open(path).map_err(|e| SlicerError::io("open", path, e))?;
    let scan = crate::output::bambu::scan_for_proof(
        file,
        plate,
        facts,
        &crate::output::OutputLimits::default(),
    )?;
    Ok(judge(expected, &scan, keymap, facts, beds))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn limits() -> ScanLimits {
        ScanLimits {
            header_bytes: 1 << 20,
            scan_bytes: 1 << 30,
            line_bytes: 4096,
        }
    }

    #[test]
    fn the_builtin_facts_load_and_a_bad_table_does_not() {
        let facts = GcodeFacts::builtin().unwrap();
        assert_eq!(facts.layer_marker, "CHANGE_LAYER");
        // Brim, outer and inner wall, bottom surface, and gap fill (measured 18 November 2026).
        assert_eq!(facts.feature.len(), 5);
        for (from, to) in [
            ("schema = 1", "schema = 2"),
            (
                "layer_marker = \"CHANGE_LAYER\"",
                "layer_marker = \"CHANGE;LAYER\"",
            ),
            (
                "name = \"Brim\"\nbound = \"wall\"\nverified = true",
                "name = \"Brim\"\nbound = \"wall\"\nverified = false",
            ),
            ("chamber_fan = 3", "chamber_fan = 99"),
            ("brim_feature = \"Brim\"", "brim_feature = \"Skirt\""),
        ] {
            assert!(FACTS_TOML.contains(from), "{from}");
            assert!(
                GcodeFacts::from_toml(&FACTS_TOML.replacen(from, to, 1)).is_err(),
                "{to}"
            );
        }
    }

    #[test]
    fn the_state_in_force_at_each_first_layer_move_is_recorded() {
        let facts = GcodeFacts::builtin().unwrap();
        // Written by hand: the marker words are the real ones, every value is made up.
        let text = "\
; HEADER_BLOCK_START
; filament: 1
; HEADER_BLOCK_END
; CONFIG_BLOCK_START
; wall_loops = 3
; wall_loops = 4
; CONFIG_BLOCK_END
M140 S61
M106 P2 S100
M106 S0
M83
; CHANGE_LAYER
M106 P2 S0
; FEATURE: Brim
G1 X1 Y1 F1200
G1 X2 Y1 E0.5
; FEATURE: Bottom surface
G1 X3 Y1 E0.5 F1800
G1 E.8 F3000
; CHANGE_LAYER
M140 S57
M106 S255
; CHANGE_LAYER
G1 X4 Y1 E0.5
; CHANGE_LAYER
";
        let scan = scan(text.as_bytes(), facts, &limits());
        assert!(scan.config_complete);
        assert_eq!(scan.duplicate, BTreeSet::from(["wall_loops".to_owned()]));
        assert_eq!(scan.filament_ids, [1]);
        assert_eq!(scan.layers_seen, 4);
        let first = &scan.first_layer;
        assert_eq!(first.moves, 2);
        assert_eq!(first.unlisted, 0);
        assert_eq!(first.brim_moves, 1);
        assert_eq!(first.bed_c, Some([61.0, 61.0]));
        assert_eq!(first.part_fan_max, Some(0.0));
        assert_eq!(first.aux_fan_max, Some(0.0));
        // The feed rates per feature, in mm/s; the move without X or Y does not count.
        let brim = facts.feature_index("Brim").unwrap();
        let bottom = facts.feature_index("Bottom surface").unwrap();
        assert_eq!(first.feed_max_mm_s[&brim], 20.0);
        assert_eq!(first.feed_max_mm_s[&bottom], 30.0);
        assert_eq!(scan.layer_3_bed_c, Some(Some(57.0)));
        assert_eq!(scan.first_part_fan_on_layer, Some(2));
    }

    #[test]
    fn a_setting_too_long_to_keep_is_recorded_by_its_key() {
        let facts = GcodeFacts::builtin().unwrap();
        let text = format!(
            "; CONFIG_BLOCK_START\n; brim_width = {}\n; skirt_loops = {}\n; CONFIG_BLOCK_END\n",
            "5".repeat(600),
            "0".repeat(5000)
        );
        let scanned = scan(text.as_bytes(), facts, &limits());
        assert!(scanned.config.is_empty());
        assert_eq!(
            scanned.dropped,
            BTreeSet::from(["brim_width".to_owned(), "skirt_loops".to_owned()])
        );
        // A settings block that does not end inside the header bytes is not complete.
        let late = scan(
            "; CONFIG_BLOCK_START\n; a = 1\n".as_bytes(),
            facts,
            &limits(),
        );
        assert!(!late.config_complete);
    }
}
