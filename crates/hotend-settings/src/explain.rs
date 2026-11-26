//! The sentence catalogue: `data/sentences.toml`.
//!
//! Guarantees:
//! - every sentence Hotend says about print settings is fixed text of the catalogue. A
//!   place in a sentence is filled with a number of the plan or with a word of the
//!   catalogue's own word tables, never with text from a person, a model or a file;
//! - the code declares which sentences exist and which places each has ([`DECISIONS`],
//!   [`NOTICES`], [`ANSWERS`], [`LINES`]). A catalogue that lacks one, holds an unknown one, or gives a
//!   sentence other places fails to load, so a plan cannot meet a missing sentence.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::model::{ChecklistItem, Colour, Condition, Day, Intent, Notice, PlateType};

/// The layout version this code reads.
const SCHEMA: u32 = 1;
/// The longest sentence, in bytes.
const MAX_SENTENCE: usize = 400;

/// A data file of this crate cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the data file `{file}` cannot be used: {problem}")]
pub struct DataError {
    /// The file under `crates/hotend-settings/data`.
    pub file: &'static str,
    /// What is wrong, for the person who edits the file.
    pub problem: String,
}

impl DataError {
    pub(crate) fn new(file: &'static str, problem: impl Into<String>) -> Self {
        Self {
            file,
            problem: problem.into(),
        }
    }
}

/// A plan could not be computed. Both cases mean that a data file and the code disagree;
/// neither can be caused by an input.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PlanError {
    /// A rule or a data file gave a value outside the hard range of its field.
    #[error(
        "a print rule gave a value Hotend does not allow: {0}; correct the data files of hotend-settings"
    )]
    Range(#[from] crate::bounds::RangeError),
    /// A sentence was asked for that the catalogue cannot give.
    #[error("the sentence `{0}` cannot be written; correct data/sentences.toml of hotend-settings")]
    Sentence(String),
    /// The finished plan breaks a cross-field rule although no change is left to take back.
    #[error(
        "the print rules gave a plan that breaks one of their own rules: {0}; correct the data files of hotend-settings"
    )]
    Cross(crate::envelope::Violation),
}

/// What fills a place of a sentence.
#[derive(Debug, Clone, Copy)]
pub(crate) enum P<'a> {
    /// A whole number.
    Whole(i64),
    /// A decimal, written with at most two places.
    Number(f64),
    /// A word of the catalogue, or a token of this crate.
    Word(&'a str),
    /// A calendar day, written `YYYY-MM-DD`: digits and dashes only.
    Date(Day),
}

/// A place of a sentence with what fills it.
pub(crate) type Places<'a> = &'a [(&'static str, P<'a>)];

/// The sentences of decisions, with the places each one has.
pub const DECISIONS: &[(&str, &[&str])] = &[
    ("setup.plate", &["plate"]),
    ("setup.spool.ams", &["family", "slot"]),
    ("setup.spool.external", &["family"]),
    ("setup.nozzle", &[]),
    ("setup.colour", &[]),
    ("intent.default", &[]),
    ("intent.chosen", &["intent"]),
    ("intent.gauge", &[]),
    ("level.0", &[]),
    ("level.1", &["rule"]),
    ("level.1.slower", &["rule"]),
    ("level.1.hotter", &["rule"]),
    ("level.1.same", &["rule"]),
    ("level.2", &[]),
    ("level.silk", &["level"]),
    ("level.history", &["level"]),
    ("level.asked", &["level"]),
    ("level.3", &[]),
    ("level.proven", &["level", "date"]),
    ("brim.kind.none", &[]),
    ("brim.kind.outer", &[]),
    ("brim.kind.asked", &[]),
    ("brim.width.none", &[]),
    ("brim.width.thin_strip", &["width", "strip"]),
    ("brim.width.small_foot", &["width", "contact"]),
    ("brim.width.slender", &["width", "ratio"]),
    ("brim.width.long", &["width", "span"]),
    ("brim.width.warping", &["width", "family"]),
    ("brim.width.flexible", &["width", "family"]),
    ("brim.width.level2", &["width"]),
    ("brim.gap.attached", &[]),
    ("brim.gap.loose", &["gap"]),
    ("brim.gap.flexible", &["gap"]),
    ("fixed.skirt", &[]),
    ("stock.kept", &["value"]),
    ("stock.written", &["value"]),
    ("calibration.locked", &["value"]),
    ("level2.first_layer_height", &["value"]),
    ("first_layer.wall_speed", &["value", "family", "level"]),
    ("first_layer.infill_speed", &["value", "family", "level"]),
    ("bed.first.card", &["value", "family", "level"]),
    ("bed.other.card", &["value"]),
    ("bed.other.fit", &["value"]),
    ("bed.calibration", &["value"]),
    ("bed.raised_over_calibration", &["value", "level"]),
    ("bed.limited", &["value"]),
    ("stock.nozzle", &[]),
    ("fan.off_layers", &["value"]),
    ("fan.off_layers.level2", &["value"]),
    ("fan.aux_first", &["value"]),
    ("fan.aux_max", &["value", "family", "level"]),
    ("fan.aux_max.none", &[]),
    ("shell.process", &["intent", "layer"]),
    ("shell.process.fallback", &["intent", "layer"]),
    ("shell.walls", &["value", "intent"]),
    ("shell.walls.thin", &["value"]),
    ("shell.infill", &["value", "intent"]),
    ("shell.pattern", &["intent"]),
    ("shell.seam", &["intent"]),
    ("shell.fallback", &["intent"]),
    ("supports.off.none", &[]),
    ("supports.off.small", &["area"]),
    ("supports.on", &["count", "largest"]),
    ("supports.on.one", &["largest"]),
    ("supports.on.unclassified", &["area"]),
    ("supports.kind.tree", &[]),
    ("supports.kind.normal", &[]),
    ("supports.threshold", &["value"]),
    ("supports.where.bed_only", &[]),
    ("supports.where.everywhere", &[]),
    ("motion.none", &[]),
    ("motion.outer_wall.intent", &["value", "intent"]),
    ("motion.outer_wall.card", &["value", "family"]),
    ("motion.gentle", &["value"]),
    ("motion.outer_accel.intent", &["value", "intent"]),
    ("motion.flow.card", &["value"]),
    ("motion.jerk", &[]),
    ("prime_tower.off", &[]),
    ("proven.brim_kind", &["date"]),
    ("proven.brim_width", &["value", "date"]),
    ("proven.wall_speed", &["value", "date"]),
    ("proven.infill_speed", &["value", "date"]),
    ("proven.bed_first", &["value", "date"]),
    ("proven.bed_other", &["value", "date"]),
    ("proven.fan_off", &["value", "date"]),
    ("proven.aux", &["value", "date"]),
    ("proven.walls", &["value", "date"]),
    ("proven.limited", &["asked", "value", "date"]),
    ("change.accepted", &["who"]),
    ("change.clamped", &["who", "asked", "value", "rule"]),
];

/// The codes of refusals, questions, warnings and notes, with the places of each sentence.
pub const NOTICES: &[(&str, &[&str])] = &[
    ("bed_type_not_chosen", &[]),
    ("plate_not_supported", &[]),
    ("spools_not_declared", &["family"]),
    ("spool_not_chosen", &["family"]),
    ("plate_material_conflict", &["family", "plate"]),
    ("material_not_printable", &["shore", "nozzle"]),
    ("material_not_allowed", &["family"]),
    ("nozzle_choice_unproven", &[]),
    ("nozzle_size_unproven", &["nozzle"]),
    (
        "bed_limit_below_window",
        &["family", "plate", "low", "limit"],
    ),
    ("facts_invalid", &[]),
    ("question.plate_type", &[]),
    ("question.spools", &[]),
    ("question.spool_choice", &[]),
    ("plate_not_recommended", &["family", "plate"]),
    ("turn_or_split", &["limit"]),
    ("too_many_flexible_pieces", &["count", "max"]),
    ("intent_value_unavailable", &["intent"]),
    ("fit_signature_differs", &[]),
    ("settings_not_proven", &["family"]),
    ("bed_limited", &[]),
    ("chamber_fan", &[]),
    ("level_3", &[]),
    ("dedicated_hotend", &[]),
    ("proven_baseline", &["date"]),
    ("proven_limited", &["date"]),
];

/// The answers to a proposal, with the places of each sentence. The part before the first dot
/// groups them: `invalid` answers refuse the whole call, `not_allowed` and `conflict` answers
/// refuse one change.
pub const ANSWERS: &[(&str, &[&str])] = &[
    ("accepted", &[]),
    ("clamped", &["asked", "value", "rule"]),
    ("invalid.shape", &[]),
    ("invalid.empty", &[]),
    ("invalid.too_many", &["max"]),
    ("invalid.unknown_field", &[]),
    ("invalid.wrong_type", &[]),
    ("invalid.number", &[]),
    ("invalid.not_whole", &[]),
    ("invalid.out_of_range", &["min", "max"]),
    ("invalid.unknown_value", &[]),
    ("invalid.duplicate_field", &[]),
    ("invalid.reason", &["max"]),
    ("invalid.summary", &["max"]),
    ("invalid.other_item", &[]),
    ("plan_blocked", &[]),
    ("not_settable", &[]),
    ("locked_by_calibration", &[]),
    ("locked", &["rule"]),
    ("kept_by_person", &[]),
    ("not_allowed.brim_required", &["rule"]),
    ("not_allowed.brim_asked", &[]),
    ("not_allowed.brim_inner_alone", &["rule"]),
    ("not_allowed.brim_inner_fit", &[]),
    ("not_allowed.brim_inner_room", &[]),
    ("not_allowed.unverified", &[]),
    ("not_allowed.supports_required", &[]),
    ("not_allowed.supports_nothing", &[]),
    ("not_allowed.supports_over_part", &[]),
    ("conflict", &["rule"]),
    ("conflict.brim_needs_width", &[]),
];

/// The lines of the before-you-print list, with the places of each.
pub const LINES: &[(&str, &[&str])] = &[
    ("unproven", &[]),
    ("plate.textured-pei", &[]),
    ("plate.cool", &[]),
    ("plate.high-temp", &[]),
    ("plate.engineering", &[]),
    ("plate.check", &[]),
    ("plate.not_recommended", &["family"]),
    ("glue.required", &[]),
    ("glue.allowed", &[]),
    ("glue.none", &[]),
    ("spool.ams", &["colour", "family", "slot"]),
    ("spool.external", &["colour", "family"]),
    ("tpu.dry", &[]),
    ("tpu.load", &[]),
    ("open_sliced", &[]),
    ("project", &[]),
    ("no_sync", &[]),
    ("bed_levelling", &[]),
    ("calibration", &[]),
    ("run_calibration", &[]),
    ("vent", &[]),
    ("watch_brim", &["lines"]),
    ("few_pieces", &[]),
    ("after.brim_fit", &[]),
    ("after.brim", &[]),
    ("after.cool", &[]),
    ("after.flexible", &[]),
    ("after.flexible_brim", &[]),
    ("outcome", &[]),
];

/// The tokens of the word table for who asked for a change.
pub const ASKER_WORDS: &[&str] = &["agent", "person"];

/// The colour words of the catalogue.
pub const COLOUR_WORDS: &[&str] = &[
    "black", "white", "grey", "red", "orange", "yellow", "green", "blue", "purple", "pink", "brown",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineRow {
    id: String,
    class: u8,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    schema: u32,
    revision: String,
    word: BTreeMap<String, BTreeMap<String, String>>,
    decision: BTreeMap<String, String>,
    notice: BTreeMap<String, String>,
    answer: BTreeMap<String, String>,
    line: Vec<LineRow>,
}

/// The loaded catalogue.
#[derive(Debug, Clone)]
pub struct Catalogue {
    revision: String,
    words: BTreeMap<String, BTreeMap<String, String>>,
    decisions: BTreeMap<String, String>,
    notices: BTreeMap<String, String>,
    answers: BTreeMap<String, String>,
    /// Class and text by id, with the position in the file for the order inside a class.
    lines: BTreeMap<String, (u8, usize, String)>,
}

const FILE: &str = "sentences.toml";

/// The places `{name}` of a sentence, or what is wrong with its braces.
fn places(text: &str) -> Result<BTreeSet<&str>, &'static str> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let close = after.find('}').ok_or("an opening brace is not closed")?;
        let name = &after[..close];
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        {
            return Err("a place must be lower-case letters between braces");
        }
        found.insert(name);
        rest = &after[close + 1..];
    }
    if rest.contains('}') {
        return Err("a closing brace has no opening brace");
    }
    Ok(found)
}

fn check_text(id: &str, text: &str, declared: &[&str]) -> Result<(), DataError> {
    let bad = |problem: &str| DataError::new(FILE, format!("sentence `{id}`: {problem}"));
    if text.is_empty() || text.len() > MAX_SENTENCE || text.chars().any(char::is_control) {
        return Err(bad("it must be one line of 1 to 400 bytes"));
    }
    let found = places(text).map_err(bad)?;
    let declared: BTreeSet<&str> = declared.iter().copied().collect();
    if found != declared {
        return Err(bad("its places are not the ones the code fills"));
    }
    Ok(())
}

/// Checks that `given` holds exactly the sentences of `declared`.
fn check_table(
    what: &str,
    declared: &[(&str, &[&str])],
    given: &BTreeMap<String, String>,
) -> Result<(), DataError> {
    for (id, places) in declared {
        let text = given.get(*id).ok_or_else(|| {
            DataError::new(FILE, format!("the {what} sentence `{id}` is missing"))
        })?;
        check_text(id, text, places)?;
    }
    if let Some(unknown) = given
        .keys()
        .find(|id| !declared.iter().any(|(known, _)| known == id))
    {
        return Err(DataError::new(
            FILE,
            format!("the {what} sentence `{unknown}` is not used by the code"),
        ));
    }
    Ok(())
}

/// A revision string: letters, digits, dots and dashes.
pub(crate) fn check_revision(
    file: &'static str,
    schema: u32,
    revision: &str,
) -> Result<(), DataError> {
    if schema != SCHEMA {
        return Err(DataError::new(
            file,
            format!("schema {schema} is not the schema {SCHEMA} this code reads"),
        ));
    }
    let plain = revision
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-');
    if revision.is_empty() || revision.len() > 32 || !plain {
        return Err(DataError::new(
            file,
            "the revision must be 1 to 32 letters, digits, dots or dashes",
        ));
    }
    Ok(())
}

impl Catalogue {
    /// Reads and checks a catalogue.
    pub fn from_toml(text: &str) -> Result<Self, DataError> {
        let file: File =
            toml::from_str(text).map_err(|error| DataError::new(FILE, error.to_string()))?;
        check_revision(FILE, file.schema, &file.revision)?;
        check_table("decision", DECISIONS, &file.decision)?;
        check_table("notice", NOTICES, &file.notice)?;
        check_table("answer", ANSWERS, &file.answer)?;

        let mut lines = BTreeMap::new();
        for (position, row) in file.line.into_iter().enumerate() {
            let declared = LINES.iter().find(|(id, _)| *id == row.id).ok_or_else(|| {
                DataError::new(
                    FILE,
                    format!("the line `{}` is not used by the code", row.id),
                )
            })?;
            check_text(&row.id, &row.text, declared.1)?;
            if !(1..=6).contains(&row.class) {
                return Err(DataError::new(
                    FILE,
                    format!("the line `{}` needs a class from 1 to 6", row.id),
                ));
            }
            if lines
                .insert(row.id.clone(), (row.class, position, row.text))
                .is_some()
            {
                return Err(DataError::new(
                    FILE,
                    format!("the line `{}` is there twice", row.id),
                ));
            }
        }
        if let Some((missing, _)) = LINES.iter().find(|(id, _)| !lines.contains_key(*id)) {
            return Err(DataError::new(
                FILE,
                format!("the line `{missing}` is missing"),
            ));
        }

        let tables: [(&str, &[&str]); 4] = [
            ("plate", PlateType::TOKENS),
            ("intent", Intent::TOKENS),
            ("colour", COLOUR_WORDS),
            ("asker", ASKER_WORDS),
        ];
        for (table, tokens) in tables {
            let words = file.word.get(table).ok_or_else(|| {
                DataError::new(FILE, format!("the word table `{table}` is missing"))
            })?;
            let same = words.len() == tokens.len()
                && tokens.iter().all(|token| words.contains_key(*token));
            let plain = words.values().all(|word| {
                !word.is_empty()
                    && word.len() <= 40
                    && !word.chars().any(|c| c.is_control() || c == '{' || c == '}')
            });
            if !same || !plain {
                return Err(DataError::new(
                    FILE,
                    format!("the word table `{table}` does not match its tokens"),
                ));
            }
        }
        if file.word.len() != tables.len() {
            return Err(DataError::new(
                FILE,
                "there is a word table the code does not use",
            ));
        }

        Ok(Self {
            revision: file.revision,
            words: file.word,
            decisions: file.decision,
            notices: file.notice,
            answers: file.answer,
            lines,
        })
    }

    /// The revision of the file.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Every text of the catalogue: sentences, lines and words. For tests that look for
    /// something that must never be said.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.decisions
            .values()
            .chain(self.notices.values())
            .chain(self.answers.values())
            .map(String::as_str)
            .chain(self.lines.values().map(|(_, _, text)| text.as_str()))
            .chain(
                self.words
                    .values()
                    .flat_map(|table| table.values().map(String::as_str)),
            )
    }

    /// The class of a line of the list.
    pub fn line_class(&self, id: &str) -> Option<u8> {
        self.lines.get(id).map(|(class, _, _)| *class)
    }

    /// The word for a token of a word table.
    pub(crate) fn word(&self, table: &str, token: &str) -> Result<&str, PlanError> {
        self.words
            .get(table)
            .and_then(|words| words.get(token))
            .map(String::as_str)
            .ok_or_else(|| PlanError::Sentence(format!("word.{table}")))
    }

    /// The word for a colour: the nearest of the catalogue's colour words.
    pub(crate) fn colour_word(&self, colour: Colour) -> Result<&str, PlanError> {
        self.word("colour", colour_token(colour))
    }

    /// The sentence of a decision, with its id as declared in [`DECISIONS`].
    pub(crate) fn decision(
        &self,
        id: &str,
        filled: Places<'_>,
    ) -> Result<(&'static str, String), PlanError> {
        let (id, _) = declared(DECISIONS, id)?;
        Ok((id, render(id, self.decisions.get(id), filled)?))
    }

    /// The sentence of a refusal, question, warning or note.
    pub(crate) fn notice(&self, code: &str, filled: Places<'_>) -> Result<Notice, PlanError> {
        let (code, _) = declared(NOTICES, code)?;
        Ok(Notice {
            code,
            sentence: render(code, self.notices.get(code), filled)?,
        })
    }

    /// The sentence of an answer to a proposal, with its id as declared in [`ANSWERS`].
    pub(crate) fn answer(
        &self,
        id: &str,
        filled: Places<'_>,
    ) -> Result<(&'static str, String), PlanError> {
        let (id, _) = declared(ANSWERS, id)?;
        Ok((id, render(id, self.answers.get(id), filled)?))
    }

    /// A line of the list.
    pub(crate) fn line(
        &self,
        id: &str,
        filled: Places<'_>,
        condition: Option<Condition>,
    ) -> Result<(usize, ChecklistItem), PlanError> {
        let (id, _) = declared(LINES, id)?;
        let (class, position, text) = self
            .lines
            .get(id)
            .ok_or_else(|| PlanError::Sentence(id.to_owned()))?;
        Ok((
            *position,
            ChecklistItem {
                id,
                class: *class,
                text: render(id, Some(text), filled)?,
                condition,
            },
        ))
    }
}

fn declared(
    table: &'static [(&'static str, &'static [&'static str])],
    id: &str,
) -> Result<(&'static str, &'static [&'static str]), PlanError> {
    table
        .iter()
        .copied()
        .find(|(known, _)| *known == id)
        .ok_or_else(|| PlanError::Sentence(id.to_owned()))
}

/// A number with at most two decimal places and no trailing zeros.
fn number(value: f64) -> String {
    let text = format!("{value:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

/// Fills the places of a sentence. A place nothing was given for is an error, not a gap.
fn render(id: &str, template: Option<&String>, filled: Places<'_>) -> Result<String, PlanError> {
    let missing = || PlanError::Sentence(id.to_owned());
    let mut rest = template.ok_or_else(missing)?.as_str();
    let mut out = String::with_capacity(rest.len() + 16);
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after.find('}').ok_or_else(missing)?;
        let (_, value) = filled
            .iter()
            .find(|(name, _)| *name == &after[..close])
            .ok_or_else(missing)?;
        match value {
            P::Whole(whole) => out.push_str(&whole.to_string()),
            P::Number(decimal) => out.push_str(&number(*decimal)),
            P::Word(word) => out.push_str(word),
            P::Date(day) => out.push_str(&day.to_string()),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The colour word nearest to a colour. A rough naming for "your grey PLA"; it decides
/// nothing.
fn colour_token(colour: Colour) -> &'static str {
    let [r, g, b] = colour.rgb().map(i32::from);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    if max - min < 36 {
        return match max {
            0..=69 => "black",
            70..=199 => "grey",
            _ => "white",
        };
    }
    // Hue in degrees.
    let span = f64::from(max - min);
    let hue = if max == r {
        60.0 * (f64::from(g - b) / span).rem_euclid(6.0)
    } else if max == g {
        60.0 * (f64::from(b - r) / span + 2.0)
    } else {
        60.0 * (f64::from(r - g) / span + 4.0)
    };
    match hue as i32 {
        0..=14 | 345..=360 => "red",
        15..=44 if max < 150 => "brown",
        15..=44 => "orange",
        45..=69 => "yellow",
        70..=169 => "green",
        170..=259 => "blue",
        260..=299 => "purple",
        _ => "pink",
    }
}
