//! Proposals: typed changes `{field, value, reason}` from the agent or the person, and how
//! each is answered.
//!
//! Guarantees:
//! - a change is a field of the closed list and a typed value inside the hard range of that
//!   field ([`Changes`]). A field name that is not known, a text where a number belongs, a
//!   number that is not finite and plain, a duplicate field or too many changes refuse the
//!   whole proposal, and nothing of what was sent is said back: no value, no reason and no
//!   summary;
//! - the reason of a change, and the summary of a proposal, are kept as a [`Note`]: one
//!   printable line for display. A note is part of the answer only. It is not part of
//!   [`Changes`], of [`PlanInputs`] or of the plan, so it cannot reach a value, a file name
//!   or the list;
//! - the plan stays a pure function: [`RuleBook::propose`] returns the new inputs, and the
//!   plan it returns is exactly `RuleBook::plan` of those inputs. The accepted changes are a
//!   set, the newest per field; the order in which changes were written never matters;
//! - a change that is refused changes nothing, and an earlier accepted change for the same
//!   field stays as it was;
//! - what the person chose to make the print hold better stays until the person changes it.
//!   A change by the agent that would hold the print less well than a value of the person
//!   that is in effect (the level, a field of the sticking group, a cap) is refused with
//!   `kept_by_person`; the agent may still ask for the same or for more. For every other
//!   field the newest change wins, whoever asked.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::ser::SerializeSeq;
use serde::{Serialize, Serializer};
use serde_json::Value as Json;

use crate::bounds::{self, Kind, RangeError};
use crate::envelope::{self, Side};
use crate::explain::{P, PlanError};
use crate::inputs::{Asked, Asker, PlanInputs};
use crate::model::{Answer, Colour, Decision, Field, Intent, PrintPlan, RuleId, SpoolSlot, Value};
use crate::rules::RuleBook;
use crate::schema::{MAX_CHANGES, MAX_PLATE, MAX_REASON_CHARS, MAX_SUMMARY_CHARS};

/// A value somebody asked for, with who asked.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Change {
    /// The value, of the type of its field and inside the field's hard range.
    pub value: Value,
    /// Who asked.
    pub by: Asker,
}

/// Why a change cannot be kept.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum ChangeError {
    /// Neither the agent nor the person may move the field.
    #[error("`{0}` cannot be changed; only the fields marked adjustable can")]
    NotAdjustable(Field),
    /// The level and the intent are inputs of their own.
    #[error("`{0}` is set through the plan inputs `level` and `intent`, not through a change")]
    OwnInput(Field),
    /// A change always names a value.
    #[error("a change to `{0}` must name a value")]
    Unset(Field),
    /// The value is not one the field can hold.
    #[error(transparent)]
    Range(#[from] RangeError),
    /// A stored list of changes does not have the shape this crate writes.
    #[error("the stored changes are not a list of `{{field, value, by}}` records")]
    Shape,
}

/// The accepted changes of one plate: at most one per field, the newest. Together with the
/// asked level and the asked intent of [`PlanInputs`] this is everything a plan takes from
/// the agent and the person.
///
/// A change is kept as it was asked for. Every time a plan is computed, each change is
/// judged against the envelope its field has in that plan, so a set built by hand or read
/// back from a file can never bring a value outside an envelope.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Changes {
    items: BTreeMap<Field, Change>,
}

impl Changes {
    /// No changes: the rule plan.
    pub fn none() -> Self {
        Self::default()
    }

    /// Adds a change, replacing an earlier one for the same field.
    pub fn with(mut self, field: Field, value: Value, by: Asker) -> Result<Self, ChangeError> {
        self.set(field, value, by)?;
        Ok(self)
    }

    fn set(&mut self, field: Field, value: Value, by: Asker) -> Result<(), ChangeError> {
        if matches!(field, Field::Level | Field::Intent) {
            return Err(ChangeError::OwnInput(field));
        }
        if !bounds::spec(field).who.adjustable {
            return Err(ChangeError::NotAdjustable(field));
        }
        let value = match value {
            Value::Unset => return Err(ChangeError::Unset(field)),
            // Two places, and one zero: the form every decimal of a plan has.
            Value::Decimal(decimal) => Value::Decimal((decimal * 100.0).round() / 100.0 + 0.0),
            other => other,
        };
        bounds::check(field, &value)?;
        self.items.insert(field, Change { value, by });
        Ok(())
    }

    /// The change for a field.
    pub fn get(&self, field: Field) -> Option<&Change> {
        self.items.get(&field)
    }

    /// Every change, in the order of the field table.
    pub fn iter(&self) -> impl Iterator<Item = (Field, &Change)> + '_ {
        self.items.iter().map(|(field, change)| (*field, change))
    }

    /// How many fields have a change.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether there is no change.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn remove(&mut self, field: Field) {
        self.items.remove(&field);
    }

    /// Reads back what this type serializes to: a list of `{field, value, by}` records. The
    /// values pass the same checks as a new proposal; anything else is refused whole.
    pub fn restore(stored: &Json) -> Result<Self, ChangeError> {
        let mut changes = Self::none();
        for record in stored.as_array().ok_or(ChangeError::Shape)? {
            let record = record.as_object().ok_or(ChangeError::Shape)?;
            if record.len() != 3 {
                return Err(ChangeError::Shape);
            }
            let field = record
                .get("field")
                .and_then(Json::as_str)
                .and_then(Field::from_token)
                .ok_or(ChangeError::Shape)?;
            let by = match record.get("by").and_then(Json::as_str) {
                Some("agent") => Asker::Agent,
                Some("person") => Asker::Person,
                _ => return Err(ChangeError::Shape),
            };
            let value = record.get("value").ok_or(ChangeError::Shape)?;
            let value =
                typed(field, &ProposedValue::from(value)).map_err(|_| ChangeError::Shape)?;
            if changes.get(field).is_some() {
                return Err(ChangeError::Shape);
            }
            changes.set(field, value, by)?;
        }
        Ok(changes)
    }
}

impl Serialize for Changes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Record<'a> {
            field: Field,
            value: &'a Value,
            by: Asker,
        }
        let mut list = serializer.serialize_seq(Some(self.items.len()))?;
        for (field, change) in &self.items {
            list.serialize_element(&Record {
                field: *field,
                value: &change.value,
                by: change.by,
            })?;
        }
        list.end()
    }
}

/// A reason, a summary or a note: one printable line, kept for display only.
///
/// There is no way from a note to a [`Value`], and no plan, input or setting holds one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note(String);

/// What a character of a note is.
enum Shows {
    /// A letter, digit, sign or symbol that Unicode has assigned and that can be seen.
    Itself,
    /// A mark that sits on the character before it.
    Mark,
    /// Nothing a reader can see or trust: a format or tag character, a variation selector,
    /// a filler that looks like a blank, a private-use or unassigned code point, a
    /// separator. It is shown as a space.
    Nothing,
}

fn shows(c: char) -> Shows {
    use unicode_general_category::GeneralCategory as Category;
    // Characters that are assigned as letters, symbols or marks and show nothing.
    if matches!(
        c,
        '\u{034F}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{2800}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FFA0}'
            | '\u{E0100}'..='\u{E01EF}'
    ) {
        return Shows::Nothing;
    }
    match unicode_general_category::get_general_category(c) {
        Category::Control
        | Category::Format
        | Category::Surrogate
        | Category::PrivateUse
        | Category::Unassigned
        | Category::LineSeparator
        | Category::ParagraphSeparator
        | Category::SpaceSeparator => Shows::Nothing,
        Category::NonspacingMark | Category::SpacingMark | Category::EnclosingMark => Shows::Mark,
        _ => Shows::Itself,
    }
}

/// The most marks a note keeps on one character. Text in any language needs fewer; a pile
/// of marks is a way to draw over the lines around a note.
const MAX_MARKS: usize = 4;

impl Note {
    /// Reads a text of `min_chars` to `max_chars` characters. A text that is too short or
    /// too long, or holds a control character (a line break among them), is `None`.
    /// A character that cannot be seen, that reorders what is shown or that Unicode has
    /// given to nobody becomes a space, and marks beyond [`MAX_MARKS`] on one character are
    /// left out. A text that must not be empty and shows nothing is `None`.
    pub fn new(text: &str, min_chars: usize, max_chars: usize) -> Option<Self> {
        // Counting stops one past the limit, so a very long text costs no more than that.
        let count = text.chars().take(max_chars + 1).count();
        if count < min_chars || count > max_chars || text.chars().any(char::is_control) {
            return None;
        }
        let mut shown = String::with_capacity(text.len());
        let mut marks = 0;
        for c in text.chars() {
            match shows(c) {
                Shows::Itself => {
                    marks = 0;
                    shown.push(c);
                }
                Shows::Mark => {
                    if marks < MAX_MARKS {
                        shown.push(c);
                    }
                    marks += 1;
                }
                Shows::Nothing => {
                    marks = 0;
                    shown.push(' ');
                }
            }
        }
        if min_chars > 0 && shown.trim().is_empty() {
            return None;
        }
        Some(Self(shown))
    }

    /// The text, for display.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A value as it arrives, before it is known to fit any field.
#[derive(Debug, Clone, PartialEq)]
pub enum ProposedValue {
    /// A number, finite or not.
    Number(f64),
    /// A text: a token of an enum field, or something that does not belong.
    Text(String),
    /// On or off.
    Flag(bool),
    /// Anything else: nothing, a list, an object.
    Other,
}

impl From<&Json> for ProposedValue {
    fn from(value: &Json) -> Self {
        match value {
            // A number JSON can write and a float cannot hold comes out as not finite.
            Json::Number(number) => ProposedValue::Number(number.as_f64().unwrap_or(f64::NAN)),
            Json::String(text) => ProposedValue::Text(text.clone()),
            Json::Bool(flag) => ProposedValue::Flag(*flag),
            Json::Null | Json::Array(_) | Json::Object(_) => ProposedValue::Other,
        }
    }
}

/// One change of a proposal, as it arrives.
#[derive(Debug, Clone, PartialEq)]
pub struct ProposedChange {
    /// The name of the field, such as `adhesion.brim_width_mm`.
    pub field: String,
    /// The value asked for.
    pub value: ProposedValue,
    /// Why, in the asker's words. Shown as a note and used for nothing else.
    pub reason: String,
}

/// A proposal as it arrives from the agent's tool call, the window or the command line.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    /// Who asks.
    pub by: Asker,
    /// The plate the caller named, if any. The caller picks the inputs of that plate; this
    /// crate does not read it.
    pub plate: Option<u32>,
    /// The changes, 1 to 16.
    pub changes: Vec<ProposedChange>,
    /// A summary of the proposal. Shown as a note and used for nothing else.
    pub summary: String,
}

/// Why a proposal is refused whole, before any rule sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Invalid {
    /// Not the shape `{plate?, changes: [{field, value, reason}], summary}`.
    Shape,
    /// No change at all.
    Empty,
    /// More changes than one proposal may carry.
    TooMany,
    /// A field name that is not in the field list.
    UnknownField,
    /// A value of another kind than the field takes.
    WrongType,
    /// A number that is not finite, or a negative zero.
    Number,
    /// A fraction where a whole number belongs.
    NotWhole,
    /// A number outside the hard range of the field.
    OutOfRange {
        /// The lowest value of the field.
        min: f64,
        /// The highest value of the field.
        max: f64,
    },
    /// A token that is not in the list of the field.
    UnknownValue,
    /// The same field twice.
    DuplicateField,
    /// A reason that is empty, too long or holds a control character.
    Reason,
    /// A summary that is too long or holds a control character.
    Summary,
    /// Another change of the same proposal is not valid.
    OtherItem,
}

impl Invalid {
    /// The id of the catalogue sentence.
    pub const fn detail(self) -> &'static str {
        match self {
            Invalid::Shape => "invalid.shape",
            Invalid::Empty => "invalid.empty",
            Invalid::TooMany => "invalid.too_many",
            Invalid::UnknownField => "invalid.unknown_field",
            Invalid::WrongType => "invalid.wrong_type",
            Invalid::Number => "invalid.number",
            Invalid::NotWhole => "invalid.not_whole",
            Invalid::OutOfRange { .. } => "invalid.out_of_range",
            Invalid::UnknownValue => "invalid.unknown_value",
            Invalid::DuplicateField => "invalid.duplicate_field",
            Invalid::Reason => "invalid.reason",
            Invalid::Summary => "invalid.summary",
            Invalid::OtherItem => "invalid.other_item",
        }
    }
}

impl Proposal {
    /// Reads the arguments of a proposal: `{plate?, changes: [{field, value, reason}],
    /// summary}`. Unknown properties are refused at every level, and so is a list that is
    /// too long, before any of its items is looked at.
    pub fn from_json(by: Asker, arguments: &Json) -> Result<Self, Invalid> {
        let object = arguments.as_object().ok_or(Invalid::Shape)?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "plate" | "changes" | "summary"))
        {
            return Err(Invalid::Shape);
        }
        let plate = match object.get("plate") {
            None => None,
            Some(plate) => Some(
                plate
                    .as_u64()
                    .filter(|plate| (1..=u64::from(MAX_PLATE)).contains(plate))
                    .and_then(|plate| u32::try_from(plate).ok())
                    .ok_or(Invalid::Shape)?,
            ),
        };
        let summary = object
            .get("summary")
            .and_then(Json::as_str)
            .ok_or(Invalid::Shape)?;
        let list = object
            .get("changes")
            .and_then(Json::as_array)
            .ok_or(Invalid::Shape)?;
        if list.len() > MAX_CHANGES {
            return Err(Invalid::TooMany);
        }
        let mut changes = Vec::with_capacity(list.len());
        for item in list {
            let item = item.as_object().ok_or(Invalid::Shape)?;
            let (Some(field), Some(value), Some(reason), 3) = (
                item.get("field").and_then(Json::as_str),
                item.get("value"),
                item.get("reason").and_then(Json::as_str),
                item.len(),
            ) else {
                return Err(Invalid::Shape);
            };
            changes.push(ProposedChange {
                field: field.to_owned(),
                value: ProposedValue::from(value),
                reason: reason.to_owned(),
            });
        }
        Ok(Self {
            by,
            plate,
            changes,
            summary: summary.to_owned(),
        })
    }
}

/// The value of `field` that `value` states, or why it states none.
fn typed(field: Field, value: &ProposedValue) -> Result<Value, Invalid> {
    // Finite, and not the negative zero, which no honest caller writes.
    let plain = |number: f64| {
        if number.is_finite() && !(number == 0.0 && number.is_sign_negative()) {
            Ok(number)
        } else {
            Err(Invalid::Number)
        }
    };
    match (bounds::spec(field).kind, value) {
        (Kind::Whole { min, max }, ProposedValue::Number(number)) => {
            let number = plain(*number)?;
            if number.fract() != 0.0 {
                return Err(Invalid::NotWhole);
            }
            let (low, high) = (min as f64, max as f64);
            // Checked as a float first, so that a huge number never reaches a conversion.
            if number < low || number > high {
                return Err(Invalid::OutOfRange {
                    min: low,
                    max: high,
                });
            }
            Ok(Value::Whole(number as i64))
        }
        (Kind::Decimal { min, max }, ProposedValue::Number(number)) => {
            let number = plain(*number)?;
            if number < min || number > max {
                return Err(Invalid::OutOfRange { min, max });
            }
            Ok(Value::decimal(number))
        }
        (Kind::Tokens(tokens), ProposedValue::Text(text)) => tokens
            .iter()
            .find(|token| **token == text.as_str())
            .map(|token| Value::Token(token))
            .ok_or(Invalid::UnknownValue),
        (Kind::Flag, ProposedValue::Flag(flag)) => Ok(Value::Flag(*flag)),
        (Kind::Colour, ProposedValue::Text(text)) => Colour::parse(text)
            .map(Value::Colour)
            .ok_or(Invalid::UnknownValue),
        (Kind::Slot, ProposedValue::Text(text)) => SpoolSlot::parse(text)
            .map(Value::Slot)
            .ok_or(Invalid::UnknownValue),
        _ => Err(Invalid::WrongType),
    }
}

/// What became of one change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Used as asked.
    Accepted,
    /// Held to the nearest bound of the envelope.
    Clamped,
    /// Not used; nothing changed for this field.
    Refused,
}

/// The answer to one change.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ItemOutcome {
    /// The field; `None` when the name was not one of Hotend's fields.
    pub field: Option<Field>,
    /// Accepted, clamped or refused.
    pub outcome: Outcome,
    /// `accepted`, `clamped`, or the code of the refusal: `invalid_arguments`,
    /// `plan_blocked`, `not_settable`, `locked_by_calibration`, `locked`,
    /// `not_allowed_here`, `conflict` or `kept_by_person`.
    pub code: &'static str,
    /// The id of the catalogue sentence in `message`.
    pub detail: &'static str,
    /// The value asked for, once it is known to be a value of the field.
    pub asked: Option<Value>,
    /// The value the plan now has for the field, when the change was applied.
    pub applied: Option<Value>,
    /// The rule that owns the bound or the set that decided the answer.
    pub rule: Option<RuleId>,
    /// The answer in words, from the catalogue.
    pub message: String,
    /// The reason that came with the change: the asker's note, for display only. `None`
    /// when the reason itself was not valid.
    pub note: Option<Note>,
}

impl fmt::Display for ItemOutcome {
    /// One line per item, such as `clamped 12 -> 10 by ENV-1`. It holds values and ids
    /// only, never the note.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let field = self.field.map_or("(unknown field)", Field::as_str);
        let rule = self.rule.map_or("", RuleId::as_str);
        match (self.outcome, self.asked, self.applied) {
            (Outcome::Accepted, _, _) => write!(f, "{field}: accepted"),
            (Outcome::Clamped, Some(asked), Some(applied)) => write!(
                f,
                "{field}: clamped {} -> {} by {rule}",
                Shown(asked),
                Shown(applied)
            ),
            (Outcome::Refused, _, _) if !rule.is_empty() => {
                write!(f, "{field}: refused: {} ({rule})", self.code)
            }
            _ => write!(f, "{field}: {}: {}", outcome_word(self.outcome), self.code),
        }
    }
}

fn outcome_word(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Accepted => "accepted",
        Outcome::Clamped => "clamped",
        Outcome::Refused => "refused",
    }
}

/// A value as it is written in a line of an answer.
struct Shown(Value);

impl fmt::Display for Shown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Value::Unset => f.write_str("-"),
            Value::Whole(whole) => write!(f, "{whole}"),
            Value::Decimal(decimal) => write!(f, "{decimal}"),
            Value::Token(token) => f.write_str(token),
            Value::Flag(true) => f.write_str("on"),
            Value::Flag(false) => f.write_str("off"),
            Value::Colour(colour) => write!(f, "{colour}"),
            Value::Slot(slot) => write!(f, "{slot}"),
        }
    }
}

/// Why a whole proposal changed nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct CallRefusal {
    /// `invalid_arguments` or `plan_blocked`.
    pub code: &'static str,
    /// The id of the catalogue sentence in `message`.
    pub detail: &'static str,
    /// The refusal in words, from the catalogue.
    pub message: String,
}

/// The answer to a proposal.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ProposalOutcome {
    /// Who asked.
    pub by: Asker,
    /// Set when the whole proposal was refused. Nothing changed then.
    pub refused: Option<CallRefusal>,
    /// One answer per change, in the order written. Empty when the list itself was not
    /// readable or too long.
    pub items: Vec<ItemOutcome>,
    /// Changes accepted earlier that the new plan no longer allows, and that were taken out
    /// of the set.
    pub dropped: Vec<ItemOutcome>,
    /// The summary of the proposal: the asker's note, for display only.
    pub summary: Option<Note>,
    /// What the plan is now a function of: the inputs with the accepted changes.
    pub inputs: PlanInputs,
    /// The plan of `inputs`.
    pub plan: PrintPlan,
}

impl ProposalOutcome {
    /// Whether the proposal changed the accepted set.
    pub fn changed(&self, before: &PlanInputs) -> bool {
        self.inputs != *before
    }
}

/// The accepted change for a field, wherever the inputs keep it.
fn stored(inputs: &PlanInputs, field: Field) -> Option<Change> {
    match field {
        Field::Level => inputs.level.map(|asked| Change {
            value: Value::Whole(i64::from(asked.value)),
            by: asked.by,
        }),
        Field::Intent => inputs.intent.map(|asked| Change {
            value: Value::Token(asked.value.as_str()),
            by: asked.by,
        }),
        other => inputs.changes.get(other).copied(),
    }
}

/// Puts a change into the inputs, or takes the change of the field out.
fn store(inputs: &mut PlanInputs, field: Field, change: Option<Change>) {
    match (field, change) {
        (Field::Level, change) => {
            inputs.level = change.and_then(|change| {
                let level = change
                    .value
                    .whole()
                    .and_then(|level| u8::try_from(level).ok())?;
                Some(Asked {
                    value: level,
                    by: change.by,
                })
            });
        }
        (Field::Intent, change) => {
            inputs.intent = change.and_then(|change| {
                let intent = change.value.token().and_then(Intent::from_token)?;
                Some(Asked {
                    value: intent,
                    by: change.by,
                })
            });
        }
        (other, Some(change)) => {
            // A value that the set does not take is not kept; the field stays as the rules
            // have it.
            if inputs.changes.set(other, change.value, change.by).is_err() {
                inputs.changes.remove(other);
            }
        }
        (other, None) => inputs.changes.remove(other),
    }
}

/// A change of a proposal that passed the schema.
struct Read {
    field: Field,
    /// `None` for a field nobody may move: its value is not looked at.
    value: Option<Value>,
    note: Note,
}

fn read(change: &ProposedChange) -> Result<Read, (Option<Field>, Invalid)> {
    // A name of the closed list or nothing: the text is not kept.
    let field = Field::from_token(&change.field).ok_or((None, Invalid::UnknownField))?;
    let note =
        Note::new(&change.reason, 1, MAX_REASON_CHARS).ok_or((Some(field), Invalid::Reason))?;
    let value = if bounds::spec(field).who.adjustable {
        Some(typed(field, &change.value).map_err(|invalid| (Some(field), invalid))?)
    } else {
        None
    };
    Ok(Read { field, value, note })
}

/// The code and the sentence of a change the agent asked for against the person's choice.
const KEPT_BY_PERSON: &str = "kept_by_person";

/// Whether a field holds the print or the fit: the level, the sticking group and the caps.
/// The person's choice for such a field is kept against the agent.
fn guarded(field: Field) -> bool {
    !matches!(envelope::side(field), Side::Either)
}

impl RuleBook {
    /// Whether the agent asks for a value that holds the print less well than a value the
    /// person chose and that is in effect in `current`.
    fn kept_by_person(
        &self,
        inputs: &PlanInputs,
        current: &PrintPlan,
        by: Asker,
        field: Field,
        value: &Value,
    ) -> bool {
        if by != Asker::Agent || !guarded(field) {
            return false;
        }
        let persons = stored(inputs, field).is_some_and(|change| change.by == Asker::Person);
        let in_effect = current.decision(field).filter(|decision| {
            decision
                .answer
                .is_some_and(|answered| answered.by == Asker::Person && answered.answer.applied())
        });
        match in_effect {
            Some(decision) if persons => !envelope::not_weaker(field, value, &decision.value),
            _ => false,
        }
    }

    /// The sentence of an answer with its numbers filled in.
    fn answer_text(
        &self,
        detail: &str,
        asked: Option<Value>,
        applied: Option<Value>,
        rule: Option<RuleId>,
        range: Option<(f64, f64)>,
    ) -> Result<(&'static str, String), PlanError> {
        let number =
            |value: Option<Value>| P::Number(value.and_then(|v| v.number()).unwrap_or(0.0));
        let (min, max) = range.unwrap_or((0.0, 0.0));
        let limit = match detail {
            "invalid.reason" => MAX_REASON_CHARS,
            "invalid.summary" => MAX_SUMMARY_CHARS,
            _ => MAX_CHANGES,
        } as i64;
        // Every place any answer has; a sentence takes the ones it names.
        let filled = [
            ("asked", number(asked)),
            ("value", number(applied)),
            ("rule", P::Word(rule.map_or("", RuleId::as_str))),
            ("min", P::Number(min)),
            (
                "max",
                if range.is_some() {
                    P::Number(max)
                } else {
                    P::Whole(limit)
                },
            ),
        ];
        self.catalogue().answer(detail, &filled)
    }

    fn invalid_item(
        &self,
        field: Option<Field>,
        invalid: Invalid,
        note: Option<Note>,
    ) -> Result<ItemOutcome, PlanError> {
        let range = match invalid {
            Invalid::OutOfRange { min, max } => Some((min, max)),
            _ => None,
        };
        let (detail, message) = self.answer_text(invalid.detail(), None, None, None, range)?;
        Ok(ItemOutcome {
            field,
            outcome: Outcome::Refused,
            code: "invalid_arguments",
            detail,
            asked: None,
            applied: None,
            rule: None,
            message,
            note,
        })
    }

    fn refused_item(
        &self,
        item: &Read,
        code: &'static str,
        detail: &str,
        rule: Option<RuleId>,
    ) -> Result<ItemOutcome, PlanError> {
        let (detail, message) = self.answer_text(detail, item.value, None, rule, None)?;
        Ok(ItemOutcome {
            field: Some(item.field),
            outcome: Outcome::Refused,
            code,
            detail,
            asked: item.value,
            applied: None,
            rule,
            message,
            note: Some(item.note.clone()),
        })
    }

    /// The answer the plan recorded for a field, as the answer to an item.
    fn recorded_item(
        &self,
        decision: &Decision,
        asked: Value,
        note: Option<Note>,
    ) -> Result<ItemOutcome, PlanError> {
        let Some(answered) = decision.answer else {
            // The plan did not see the change: treated as a change that could not be kept.
            let rule = Some(RuleId::Xf1);
            let (detail, message) = self.answer_text("conflict", Some(asked), None, rule, None)?;
            return Ok(ItemOutcome {
                field: Some(decision.field),
                outcome: Outcome::Refused,
                code: Answer::Conflict.as_str(),
                detail,
                asked: Some(asked),
                applied: None,
                rule,
                message,
                note,
            });
        };
        let applied = answered.answer.applied().then_some(decision.value);
        let (detail, message) = self.answer_text(
            answered.detail,
            Some(asked),
            applied,
            Some(answered.rule),
            None,
        )?;
        Ok(ItemOutcome {
            field: Some(decision.field),
            outcome: match answered.answer {
                Answer::Accepted => Outcome::Accepted,
                Answer::Clamped => Outcome::Clamped,
                Answer::NotAllowed | Answer::Locked | Answer::Conflict => Outcome::Refused,
            },
            code: answered.answer.as_str(),
            detail,
            asked: Some(asked),
            applied,
            // An accepted value was held by no rule.
            rule: (answered.answer != Answer::Accepted).then_some(answered.rule),
            message,
            note,
        })
    }

    /// Refuses a proposal whose arguments could not be read at all. Nothing changes.
    pub fn refuse(
        &self,
        inputs: &PlanInputs,
        by: Asker,
        invalid: Invalid,
    ) -> Result<ProposalOutcome, PlanError> {
        let (detail, message) = self.answer_text(invalid.detail(), None, None, None, None)?;
        Ok(ProposalOutcome {
            by,
            refused: Some(CallRefusal {
                code: "invalid_arguments",
                detail,
                message,
            }),
            items: Vec::new(),
            dropped: Vec::new(),
            summary: None,
            inputs: inputs.clone(),
            plan: self.plan(inputs)?,
        })
    }

    /// Answers a proposal that arrives as JSON arguments: [`Proposal::from_json`], then
    /// [`RuleBook::propose`].
    pub fn propose_json(
        &self,
        inputs: &PlanInputs,
        by: Asker,
        arguments: &Json,
    ) -> Result<ProposalOutcome, PlanError> {
        match Proposal::from_json(by, arguments) {
            Ok(proposal) => self.propose(inputs, &proposal),
            Err(invalid) => self.refuse(inputs, by, invalid),
        }
    }

    /// Answers a proposal against the plate of `inputs`, which hold the changes accepted so
    /// far.
    ///
    /// 1. Schema: a change that is not well formed refuses the whole proposal.
    /// 2. Authority: a field nobody may move is refused; the other changes go on.
    /// 3. Envelope: the plan is computed with the new changes beside the accepted ones. A
    ///    number outside its envelope is held to the nearest bound; a value outside its
    ///    allowed set, or one that breaks a cross-field rule, is refused and the field keeps
    ///    what it had.
    ///
    /// The returned inputs hold the accepted set, and the returned plan is the plan of
    /// those inputs. An `Err` means that a data file and the code disagree.
    pub fn propose(
        &self,
        inputs: &PlanInputs,
        proposal: &Proposal,
    ) -> Result<ProposalOutcome, PlanError> {
        let by = proposal.by;
        let unchanged = |refused: Option<CallRefusal>,
                         items: Vec<ItemOutcome>,
                         summary: Option<Note>,
                         plan: PrintPlan| ProposalOutcome {
            by,
            refused,
            items,
            dropped: Vec::new(),
            summary,
            inputs: inputs.clone(),
            plan,
        };
        let call_refusal = |code: &'static str, detail: &str| -> Result<CallRefusal, PlanError> {
            let (detail, message) = self.answer_text(detail, None, None, None, None)?;
            Ok(CallRefusal {
                code,
                detail,
                message,
            })
        };
        let current = self.plan(inputs)?;

        // ---- 1. Schema. Nothing below looks at more than `MAX_CHANGES` items. ----
        let summary = Note::new(&proposal.summary, 0, MAX_SUMMARY_CHARS);
        let whole_call = if proposal.changes.is_empty() {
            Some(Invalid::Empty)
        } else if proposal.changes.len() > MAX_CHANGES {
            Some(Invalid::TooMany)
        } else if summary.is_none() {
            Some(Invalid::Summary)
        } else {
            None
        };
        if let Some(invalid @ (Invalid::Empty | Invalid::TooMany)) = whole_call {
            let refusal = call_refusal("invalid_arguments", invalid.detail())?;
            return Ok(unchanged(Some(refusal), Vec::new(), None, current));
        }
        let mut items: Vec<Result<Read, (Option<Field>, Invalid)>> =
            proposal.changes.iter().map(read).collect();
        let mut seen = BTreeSet::new();
        let mut twice = BTreeSet::new();
        for item in items.iter().flatten() {
            if !seen.insert(item.field) {
                twice.insert(item.field);
            }
        }
        for item in &mut items {
            if let Ok(read) = item
                && twice.contains(&read.field)
            {
                *item = Err((Some(read.field), Invalid::DuplicateField));
            }
        }
        let first_fault = whole_call.or_else(|| {
            items
                .iter()
                .find_map(|item| item.as_ref().err().map(|(_, invalid)| *invalid))
        });
        if let Some(fault) = first_fault {
            let answers = items
                .into_iter()
                .map(|item| match item {
                    Ok(read) => self.invalid_item(Some(read.field), Invalid::OtherItem, None),
                    Err((field, invalid)) => self.invalid_item(field, invalid, None),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let refusal = call_refusal("invalid_arguments", fault.detail())?;
            // Nothing of a call that is refused whole is said back, a valid reason and
            // the summary included.
            return Ok(unchanged(Some(refusal), answers, None, current));
        }
        let items: Vec<Read> = items.into_iter().flatten().collect();

        // ---- A plan that is blocked has no envelopes to judge a change against. ----
        let Some(derived) = &current.derived else {
            let answers = items
                .iter()
                .map(|item| self.refused_item(item, "plan_blocked", "plan_blocked", None))
                .collect::<Result<Vec<_>, _>>()?;
            let refusal = call_refusal("plan_blocked", "plan_blocked")?;
            return Ok(unchanged(Some(refusal), answers, summary, current));
        };

        // ---- 2. Authority. ----
        let mut answers: BTreeMap<Field, ItemOutcome> = BTreeMap::new();
        let mut candidate = inputs.clone();
        // The fields whose change in `candidate` is the one of this proposal.
        let mut fresh: BTreeSet<Field> = BTreeSet::new();
        for item in &items {
            let who = bounds::spec(item.field).who;
            match item.value {
                Some(value) if self.kept_by_person(inputs, &current, by, item.field, &value) => {
                    let answer = self.refused_item(item, KEPT_BY_PERSON, KEPT_BY_PERSON, None)?;
                    answers.insert(item.field, answer);
                }
                Some(value) => {
                    store(&mut candidate, item.field, Some(Change { value, by }));
                    fresh.insert(item.field);
                }
                None if who.calibration && derived.calibrated => {
                    let answer = self.refused_item(
                        item,
                        "locked_by_calibration",
                        "locked_by_calibration",
                        Some(RuleId::Sig1),
                    )?;
                    answers.insert(item.field, answer);
                }
                None => {
                    let answer = self.refused_item(item, "not_settable", "not_settable", None)?;
                    answers.insert(item.field, answer);
                }
            }
        }
        let note_of = |field: Field| {
            items
                .iter()
                .find(|item| item.field == field)
                .map(|item| item.note.clone())
        };

        // ---- 3 to 5. Envelopes, the fixed order and the cross-field rules are the plan's
        // own: it judges every change of the set. What it refuses is taken out again, and
        // the plan is computed once more, until the set holds only what the plan applies.
        let mut dropped: Vec<ItemOutcome> = Vec::new();
        let mut rounds = 0;
        let plan = loop {
            let plan = match self.plan(&candidate) {
                Ok(plan) if plan.settings.is_some() && rounds <= 2 * Field::ALL.len() => plan,
                Ok(_) | Err(PlanError::Cross(_)) => {
                    // The changes of this proposal leave no deliverable plan: all of them
                    // are taken back.
                    let answers = items
                        .iter()
                        .map(|item| match answers.remove(&item.field) {
                            Some(answer) => Ok(answer),
                            None => self.refused_item(
                                item,
                                Answer::Conflict.as_str(),
                                "conflict",
                                Some(RuleId::Xf1),
                            ),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    return Ok(unchanged(None, answers, summary, current));
                }
                Err(error) => return Err(error),
            };
            rounds += 1;
            let refused: Vec<&Decision> = plan
                .decisions
                .iter()
                .filter(|decision| {
                    decision
                        .answer
                        .is_some_and(|answered| !answered.answer.applied())
                })
                .collect();
            if refused.is_empty() {
                break plan;
            }
            for decision in refused {
                let field = decision.field;
                let Some(change) = stored(&candidate, field) else {
                    continue;
                };
                if fresh.remove(&field) {
                    // A change of this proposal: answered, and the field gets back what it
                    // had before.
                    let answer = self.recorded_item(decision, change.value, note_of(field))?;
                    answers.insert(field, answer);
                    let before = stored(inputs, field).filter(|before| *before != change);
                    store(&mut candidate, field, before);
                } else if by == Asker::Agent && change.by == Asker::Person && guarded(field) {
                    // The agent's changes would push out what the person chose to make the
                    // print hold: they are taken back instead, all of them.
                    let answers = items
                        .iter()
                        .map(|item| match answers.remove(&item.field) {
                            Some(answer) if answer.outcome == Outcome::Refused => Ok(answer),
                            _ => self.refused_item(item, KEPT_BY_PERSON, KEPT_BY_PERSON, None),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    return Ok(unchanged(None, answers, summary, current));
                } else {
                    // An earlier change that the new plan no longer allows.
                    dropped.push(self.recorded_item(decision, change.value, None)?);
                    store(&mut candidate, field, None);
                }
            }
        };

        // ---- The answers of the changes that stayed, read from the plan itself. ----
        let mut outcomes = Vec::with_capacity(items.len());
        for item in &items {
            let answer = match (answers.remove(&item.field), item.value) {
                (Some(answer), _) => answer,
                (None, Some(value)) => match plan.decision(item.field) {
                    Some(decision) => {
                        self.recorded_item(decision, value, Some(item.note.clone()))?
                    }
                    None => self.refused_item(
                        item,
                        Answer::Conflict.as_str(),
                        "conflict",
                        Some(RuleId::Xf1),
                    )?,
                },
                (None, None) => self.refused_item(item, "not_settable", "not_settable", None)?,
            };
            outcomes.push(answer);
        }
        Ok(ProposalOutcome {
            by,
            refused: None,
            items: outcomes,
            dropped,
            summary,
            inputs: candidate,
            plan,
        })
    }
}
