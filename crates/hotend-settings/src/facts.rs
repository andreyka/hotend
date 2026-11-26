//! The plate facts the engine sends: what was measured on each piece as it lies on the bed.
//!
//! The engine is not trusted. Guarantees:
//! - a [`PlateFacts`] exists only after every number was found present, finite, not negative
//!   and inside the bound of its fact, every list inside its length, and every enum one of
//!   its tokens. The rules read nothing else;
//! - unknown keys, missing keys and values of the wrong type are refused, not skipped;
//! - a [`FactsError`] names the fact and the kind of problem in Hotend's own words. No text
//!   of the engine's answer is copied into it;
//! - the only text a fact may carry is the name of a fit pair, held to a fixed pattern. It
//!   is kept in the record of the facts and is never put into a sentence, a file or a profile.
//!
//! The facts of a piece are held against each other. What no shape can have is refused as
//! [`Problem::Inconsistent`]: an outline shorter than the circle of the contact's area, a
//! strip wider than that circle's radius, a contact larger than the square on its longest
//! side, a part of the overhang larger than the whole, a piece on the bed without a height.
//! Inside those limits the rules use the careful side of two facts that can be cross-checked,
//! [`PieceFacts::thin_strip_mm`] and [`PieceFacts::tipping`]. So one wrong number can ask for
//! more adhesion than needed, and two wrong numbers cannot excuse each other.
//!
//! The JSON text is read straight into the strict types: a key stated twice is refused, a
//! text longer than [`MAX_FACTS_BYTES`] is refused before it is read, and a list stops being
//! read at its bound.

use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::model::Family;

/// The most pieces on one plate.
pub const MAX_PIECES: usize = 256;
/// The most overhang regions, inner loops or fit entries of one piece.
pub const MAX_ENTRIES: usize = 64;
/// Bound of an area, in mm2.
pub const MAX_AREA_MM2: f64 = 1e6;
/// Bound of the contact perimeter, in mm.
pub const MAX_PERIMETER_MM: f64 = 1e5;
/// Bound of a length, in mm.
pub const MAX_LENGTH_MM: f64 = 1000.0;
/// Bound of the slenderness.
pub const MAX_SLENDERNESS: f64 = 1e4;
/// The longest JSON text of plate facts, in bytes. A plate with every list at its bound is
/// about half of this.
pub const MAX_FACTS_BYTES: usize = 8 * 1024 * 1024;
/// Below this contact a piece counts as standing on nothing, in mm2.
const NO_CONTACT_MM2: f64 = 1e-6;
/// How far the region areas may be from `overhang_mm2`, and the stated totals from the sums.
/// The same slack holds for the cross-checks of the facts of one piece: the engine rounds
/// each number on its own.
const SUM_TOLERANCE: f64 = 0.01;
/// Absolute slack for sums of small areas, in mm2.
const SUM_SLACK_MM2: f64 = 0.01;

/// What is wrong with a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    /// The answer is not the expected shape: a key is missing or unknown, or a value has
    /// the wrong type.
    Malformed,
    /// The number is not finite.
    NotFinite,
    /// The number is negative.
    Negative,
    /// The number is above its bound.
    OutOfBounds,
    /// The list is longer than its bound, or empty where it must not be.
    WrongCount,
    /// Two facts contradict each other.
    Inconsistent,
    /// The text does not match its fixed pattern.
    BadName,
}

impl Problem {
    fn words(self) -> &'static str {
        match self {
            Problem::Malformed => "missing, unknown or of the wrong type",
            Problem::NotFinite => "not a finite number",
            Problem::Negative => "negative",
            Problem::OutOfBounds => "above its bound",
            Problem::WrongCount => "a list of the wrong length",
            Problem::Inconsistent => "in contradiction with another fact",
            Problem::BadName => "not a name Hotend accepts",
        }
    }
}

/// Why plate facts were refused. The plan is then blocked with `facts_invalid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FactsError {
    /// The piece, counted from 0, when the problem is in one piece.
    pub piece: Option<usize>,
    /// The name of the fact, one of the names in docs/PRINT_SETTINGS.md.
    pub fact: &'static str,
    /// What is wrong.
    pub problem: Problem,
}

impl std::fmt::Display for FactsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the plate facts cannot be used: `{}`", self.fact)?;
        if let Some(index) = self.piece {
            write!(f, " of piece {}", index + 1)?;
        }
        write!(f, " is {}; prepare the plate again", self.problem.words())
    }
}

impl std::error::Error for FactsError {}

impl FactsError {
    fn plate(fact: &'static str, problem: Problem) -> Self {
        Self {
            piece: None,
            fact,
            problem,
        }
    }

    fn at(self, piece: usize) -> Self {
        Self {
            piece: Some(piece),
            ..self
        }
    }
}

/// A key that must be present and may be `null`. (A plain `Option` would also accept a
/// missing key.)
fn required_nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

/// What the reader says when a list passes its bound. The text is Hotend's own, so it can
/// be recognised in the parser's message without copying anything of the engine's.
const TOO_MANY_PIECES: &str = "hotend: more pieces than a plate holds";
const TOO_MANY_ENTRIES: &str = "hotend: more entries than a piece holds";

/// A list that is read up to `MAX` elements. The element after the bound ends the reading
/// with an error; nothing behind it is parsed or stored.
struct Bounded<T, const MAX: usize>(Vec<T>);

impl<'de, T: Deserialize<'de>, const MAX: usize> Deserialize<'de> for Bounded<T, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Reader<T, const MAX: usize>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>, const MAX: usize> Visitor<'de> for Reader<T, MAX> {
            type Value = Vec<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a list inside its bound")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    if items.len() == MAX {
                        return Err(de::Error::custom(if MAX == MAX_PIECES {
                            TOO_MANY_PIECES
                        } else {
                            TOO_MANY_ENTRIES
                        }));
                    }
                    items.push(item);
                }
                Ok(items)
            }
        }

        deserializer
            .deserialize_seq(Reader::<T, MAX>(PhantomData))
            .map(Bounded)
    }
}

fn entries<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Vec<T>, D::Error> {
    Bounded::<T, MAX_ENTRIES>::deserialize(deserializer).map(|list| list.0)
}

/// [`entries`] for a key that must be present and may be `null`.
fn nullable_entries<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<Vec<T>>, D::Error> {
    Option::<Bounded<T, MAX_ENTRIES>>::deserialize(deserializer).map(|list| list.map(|list| list.0))
}

fn pieces<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<PieceFacts>, D::Error> {
    Bounded::<PieceFacts, MAX_PIECES>::deserialize(deserializer).map(|list| list.0)
}

macro_rules! fact_tokens {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $token:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vmeta])* #[serde(rename = $token)] $variant),+
        }
    };
}

fact_tokens! {
    /// What kind of overhang a region is.
    RegionKind {
        /// Flat and carried on two opposite sides.
        Bridge => "bridge",
        /// The ceiling of a round hole whose axis lies flat.
        HoleCeiling => "hole_ceiling",
        /// Anything else: it reaches out from a carried edge.
        Free => "free",
    }
}

fact_tokens! {
    /// On which side of a fit the mating face lies.
    FitSide {
        /// The face looks inward: a bore.
        Inside => "inside",
        /// The face looks outward: a peg.
        Outside => "outside",
    }
}

/// One connected overhang region, as the engine states it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverhangRegion {
    /// The area of the region.
    pub area_mm2: f64,
    /// What kind of overhang it is.
    pub kind: RegionKind,
    /// For a bridge: the distance between the two carrying sides.
    #[serde(default)]
    pub span_mm: Option<f64>,
    /// For a hole ceiling: the diameter of the hole.
    #[serde(default)]
    pub diameter_mm: Option<f64>,
    /// For a free overhang: the farthest distance from the nearest carried edge.
    #[serde(default)]
    pub reach_mm: Option<f64>,
    /// Whether the region hangs over the same piece.
    pub over_part: bool,
}

impl OverhangRegion {
    /// A bridge of this span.
    pub fn bridge(area_mm2: f64, span_mm: f64, over_part: bool) -> Self {
        Self {
            area_mm2,
            kind: RegionKind::Bridge,
            span_mm: Some(span_mm),
            diameter_mm: None,
            reach_mm: None,
            over_part,
        }
    }

    /// The ceiling of a round hole of this diameter.
    pub fn hole_ceiling(area_mm2: f64, diameter_mm: f64, over_part: bool) -> Self {
        Self {
            area_mm2,
            kind: RegionKind::HoleCeiling,
            span_mm: None,
            diameter_mm: Some(diameter_mm),
            reach_mm: None,
            over_part,
        }
    }

    /// A free overhang with this reach.
    pub fn free(area_mm2: f64, reach_mm: f64, over_part: bool) -> Self {
        Self {
            area_mm2,
            kind: RegionKind::Free,
            span_mm: None,
            diameter_mm: None,
            reach_mm: Some(reach_mm),
            over_part,
        }
    }

    /// The length that belongs to the kind: span, diameter or reach.
    pub fn length_mm(&self) -> Option<f64> {
        match self.kind {
            RegionKind::Bridge => self.span_mm,
            RegionKind::HoleCeiling => self.diameter_mm,
            RegionKind::Free => self.reach_mm,
        }
    }
}

/// One inner loop of a contact region.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InnerLoop {
    /// The smallest width of the hole the loop encloses.
    pub min_width_mm: f64,
    /// Whether a declared mating face borders the loop.
    pub fit: bool,
}

/// A declared mating face with an edge on the bed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FitAtBed {
    /// The name of the fit pair: a lower-case letter, then letters, digits, `_`, `.` or
    /// `-`, 64 characters at most.
    pub pair: String,
    /// The side of the fit the face is on.
    pub side: FitSide,
}

/// The facts of one piece as it lies on the bed. Every key of the engine's object is
/// required; `overhang_regions` and `min_wall_mm` may be `null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PieceFacts {
    /// Planar area on the bed.
    pub contact_mm2: f64,
    /// Total edge length of the contact regions, inner loops included.
    pub contact_perimeter_mm: f64,
    /// The smallest `2 x area / perimeter` over the contact regions.
    pub strip_width_mm: f64,
    /// Longest side of the contact's bounding box.
    pub contact_span_mm: f64,
    /// Height of the piece.
    pub height_mm: f64,
    /// `height / sqrt(contact)`.
    pub slenderness: f64,
    /// Area that hangs steeper than the material prints without support.
    pub overhang_mm2: f64,
    /// The part of `overhang_mm2` on mating faces.
    pub fit_overhang_mm2: f64,
    /// The part of `overhang_mm2` that hangs over the same piece.
    pub overhang_over_part_mm2: f64,
    /// The connected regions of `overhang_mm2`, largest first; `None` when the engine could
    /// not classify the piece.
    #[serde(deserialize_with = "nullable_entries")]
    pub overhang_regions: Option<Vec<OverhangRegion>>,
    /// The inner loops of the contact regions.
    #[serde(deserialize_with = "entries")]
    pub inner_loops: Vec<InnerLoop>,
    /// Declared mating faces with an edge on the bed.
    #[serde(deserialize_with = "entries")]
    pub fit_at_bed: Vec<FitAtBed>,
    /// The thinnest wall, when the wall check ran.
    #[serde(deserialize_with = "required_nullable")]
    pub min_wall_mm: Option<f64>,
}

impl PieceFacts {
    /// The strip width the rules use: the stated one, or `2 x contact / perimeter` of the
    /// whole piece when that is smaller. The smallest region can never be wider than the
    /// piece's own ratio, so a stated width above it is an error of the engine, and the
    /// thinner reading asks for more adhesion.
    pub fn thin_strip_mm(&self) -> f64 {
        if self.contact_perimeter_mm > 0.0 {
            self.strip_width_mm
                .min(2.0 * self.contact_mm2 / self.contact_perimeter_mm)
        } else {
            self.strip_width_mm
        }
    }

    /// The slenderness the rules use: the stated one, or `height / sqrt(contact)` when that
    /// is larger, [`MAX_SLENDERNESS`] at most. However small the contact is stated, the
    /// height over it counts: a piece that stands on next to nothing is as slender as a
    /// piece can be.
    pub fn tipping(&self) -> f64 {
        let footing = self.contact_mm2.max(NO_CONTACT_MM2).sqrt();
        self.slenderness
            .max(self.height_mm / footing)
            .min(MAX_SLENDERNESS)
    }

    /// The facts of one piece against each other (module doc). Each bound is one no shape
    /// can pass; the slack covers the engine's rounding.
    fn consistent(&self) -> Result<(), FactsError> {
        let wrong = |fact: &'static str| Err(FactsError::plate(fact, Problem::Inconsistent));
        let more = |value: f64| value * (1.0 + SUM_TOLERANCE) + SUM_SLACK_MM2;
        let less = |value: f64| (value * (1.0 - SUM_TOLERANCE) - SUM_SLACK_MM2).max(0.0);
        let contact = self.contact_mm2;
        // The shortest outline of an area is a circle's.
        if more(self.contact_perimeter_mm) < 2.0 * (std::f64::consts::PI * less(contact)).sqrt() {
            return wrong("contact_perimeter_mm");
        }
        // `2 x area / perimeter` of a region is largest for a circle: its radius.
        if less(self.strip_width_mm) > (more(contact) / std::f64::consts::PI).sqrt() {
            return wrong("strip_width_mm");
        }
        // A contact fits into the square on the longest side of its bounding box.
        if more(self.contact_span_mm) < less(contact).sqrt() {
            return wrong("contact_span_mm");
        }
        if contact > NO_CONTACT_MM2 && self.height_mm <= 0.0 {
            return wrong("height_mm");
        }
        if less(self.fit_overhang_mm2) > self.overhang_mm2 {
            return wrong("fit_overhang_mm2");
        }
        if less(self.overhang_over_part_mm2) > self.overhang_mm2 {
            return wrong("overhang_over_part_mm2");
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), FactsError> {
        bounded("contact_mm2", self.contact_mm2, MAX_AREA_MM2)?;
        bounded(
            "contact_perimeter_mm",
            self.contact_perimeter_mm,
            MAX_PERIMETER_MM,
        )?;
        bounded("strip_width_mm", self.strip_width_mm, MAX_LENGTH_MM)?;
        bounded("contact_span_mm", self.contact_span_mm, MAX_LENGTH_MM)?;
        bounded("height_mm", self.height_mm, MAX_LENGTH_MM)?;
        bounded("slenderness", self.slenderness, MAX_SLENDERNESS)?;
        bounded("overhang_mm2", self.overhang_mm2, MAX_AREA_MM2)?;
        bounded("fit_overhang_mm2", self.fit_overhang_mm2, MAX_AREA_MM2)?;
        bounded(
            "overhang_over_part_mm2",
            self.overhang_over_part_mm2,
            MAX_AREA_MM2,
        )?;
        if let Some(min_wall) = self.min_wall_mm {
            bounded("min_wall_mm", min_wall, MAX_LENGTH_MM)?;
        }
        if let Some(regions) = &self.overhang_regions {
            if regions.len() > MAX_ENTRIES {
                return Err(FactsError::plate("overhang_regions", Problem::WrongCount));
            }
            let mut sum = 0.0;
            for region in regions {
                bounded("overhang_regions.area_mm2", region.area_mm2, MAX_AREA_MM2)?;
                // The length of the region's own kind must be there; the other two may be
                // absent or null, and are held to the same bound when they are stated.
                let Some(length) = region.length_mm() else {
                    return Err(FactsError::plate("overhang_regions", Problem::Malformed));
                };
                bounded("overhang_regions.length", length, MAX_LENGTH_MM)?;
                for other in [region.span_mm, region.diameter_mm, region.reach_mm]
                    .into_iter()
                    .flatten()
                {
                    bounded("overhang_regions.length", other, MAX_LENGTH_MM)?;
                }
                sum += region.area_mm2;
            }
            if (sum - self.overhang_mm2).abs() > SUM_TOLERANCE * self.overhang_mm2 + SUM_SLACK_MM2 {
                return Err(FactsError::plate("overhang_regions", Problem::Inconsistent));
            }
        }
        if self.inner_loops.len() > MAX_ENTRIES {
            return Err(FactsError::plate("inner_loops", Problem::WrongCount));
        }
        for inner in &self.inner_loops {
            bounded(
                "inner_loops.min_width_mm",
                inner.min_width_mm,
                MAX_LENGTH_MM,
            )?;
        }
        if self.fit_at_bed.len() > MAX_ENTRIES {
            return Err(FactsError::plate("fit_at_bed", Problem::WrongCount));
        }
        for fit in &self.fit_at_bed {
            if !valid_pair_name(&fit.pair) {
                return Err(FactsError::plate("fit_at_bed.pair", Problem::BadName));
            }
        }
        self.consistent()
    }
}

/// A number that must be finite, not negative and at most `max`.
fn bounded(fact: &'static str, value: f64, max: f64) -> Result<(), FactsError> {
    let problem = if !value.is_finite() {
        Problem::NotFinite
    } else if value < 0.0 {
        Problem::Negative
    } else if value > max {
        Problem::OutOfBounds
    } else {
        return Ok(());
    };
    Err(FactsError::plate(fact, problem))
}

/// `^[a-z][a-z0-9_.-]{0,63}$`
fn valid_pair_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let head = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase());
    let tail = bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
    });
    head && tail && name.len() <= 64
}

/// The object the engine sends for one plate.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlateWire {
    family: String,
    piece_count: u64,
    total_contact_mm2: f64,
    #[serde(deserialize_with = "pieces")]
    pieces: Vec<PieceFacts>,
}

/// Turns the parser's refusal into Hotend's own words. The parser's message can repeat text
/// of the engine, so only the two messages this module wrote itself are told apart.
fn refusal(error: &dyn fmt::Display) -> FactsError {
    let message = error.to_string();
    if message.starts_with(TOO_MANY_PIECES) {
        FactsError::plate("pieces", Problem::WrongCount)
    } else if message.starts_with(TOO_MANY_ENTRIES) {
        FactsError::plate("entries", Problem::WrongCount)
    } else {
        FactsError::plate("facts", Problem::Malformed)
    }
}

/// The checked facts of one plate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlateFacts {
    family: Family,
    piece_count: usize,
    total_contact_mm2: f64,
    pieces: Vec<PieceFacts>,
}

impl PlateFacts {
    /// Checks the facts of the pieces of one plate. The piece count and the total contact
    /// are taken from the pieces.
    pub fn new(family: Family, pieces: Vec<PieceFacts>) -> Result<Self, FactsError> {
        if pieces.is_empty() || pieces.len() > MAX_PIECES {
            return Err(FactsError::plate("pieces", Problem::WrongCount));
        }
        for (index, piece) in pieces.iter().enumerate() {
            piece.validate().map_err(|error| error.at(index))?;
        }
        let total_contact_mm2: f64 = pieces.iter().map(|piece| piece.contact_mm2).sum();
        bounded(
            "total_contact_mm2",
            total_contact_mm2,
            MAX_AREA_MM2 * MAX_PIECES as f64,
        )?;
        Ok(Self {
            family,
            piece_count: pieces.len(),
            total_contact_mm2,
            pieces,
        })
    }

    /// Reads and checks the engine's object: `{family, piece_count, total_contact_mm2,
    /// pieces: [...]}`. The stated count and total must agree with the pieces.
    ///
    /// A value that was parsed elsewhere has lost what only the text shows (a key stated
    /// twice, the size). Facts that arrive as text go through [`PlateFacts::from_json`].
    pub fn from_value(value: serde_json::Value) -> Result<Self, FactsError> {
        let wire: PlateWire = serde_json::from_value(value).map_err(|error| refusal(&error))?;
        Self::from_wire(wire)
    }

    fn from_wire(wire: PlateWire) -> Result<Self, FactsError> {
        let family = Family::from_token(&wire.family)
            .ok_or(FactsError::plate("family", Problem::Malformed))?;
        bounded(
            "total_contact_mm2",
            wire.total_contact_mm2,
            MAX_AREA_MM2 * MAX_PIECES as f64,
        )?;
        let facts = Self::new(family, wire.pieces)?;
        if wire.piece_count != facts.piece_count as u64 {
            return Err(FactsError::plate("piece_count", Problem::Inconsistent));
        }
        let off = (wire.total_contact_mm2 - facts.total_contact_mm2).abs();
        if off > SUM_TOLERANCE * facts.total_contact_mm2 + SUM_SLACK_MM2 {
            return Err(FactsError::plate(
                "total_contact_mm2",
                Problem::Inconsistent,
            ));
        }
        Ok(facts)
    }

    /// Reads and checks the engine's object from JSON text. The text is read straight into
    /// the strict types: a key stated twice is refused, as is a text longer than
    /// [`MAX_FACTS_BYTES`], and a list is read up to its bound only.
    pub fn from_json(text: &str) -> Result<Self, FactsError> {
        if text.len() > MAX_FACTS_BYTES {
            return Err(FactsError::plate("facts", Problem::OutOfBounds));
        }
        let wire: PlateWire = serde_json::from_str(text).map_err(|error| refusal(&error))?;
        Self::from_wire(wire)
    }

    /// The material family of the plate.
    pub fn family(&self) -> Family {
        self.family
    }

    /// The pieces, in the engine's order.
    pub fn pieces(&self) -> &[PieceFacts] {
        &self.pieces
    }

    /// The sum of the pieces' contact areas.
    pub fn total_contact_mm2(&self) -> f64 {
        self.total_contact_mm2
    }

    /// Whether any piece has a declared mating face with an edge on the bed.
    pub fn fit_at_bed(&self) -> bool {
        self.pieces.iter().any(|piece| !piece.fit_at_bed.is_empty())
    }
}
