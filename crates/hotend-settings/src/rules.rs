//! The rule engine: plate facts, material card, setup, history and calibration in, a
//! [`PrintPlan`] out (docs/PRINT_SETTINGS.md).
//!
//! Guarantees:
//! - the plan is a pure function of its inputs and the four data files: no clock, no
//!   randomness, no I/O. Equal inputs give equal bytes;
//! - the rules fill every field, so the plan is printable without anybody choosing anything;
//! - every value passes the constant hard range of its field before a plan exists;
//! - the sticking group depends only on the facts, the card, the plate, the level, the
//!   history and the calibration. The intent is read after it and cannot reach it: with a
//!   preset other than the standard one, STK-1 writes the four baseline values explicitly;
//! - what a refusal says comes from the catalogue. No input text reaches a sentence;
//! - a proven baseline ([`crate::inputs::Baseline`]) is a floor: every field it holds is at
//!   least as careful as the baseline (held to what the plate allows), and the rules may go
//!   further. Without one the rules decide alone.
//!
//! What somebody asked for is an input like any other: the asked intent, the asked level
//! and the accepted changes ([`crate::propose::Changes`]). The level and the intent come
//! first, because they change the defaults of other fields. Every other field is decided by
//! the rules, gets its envelope ([`crate::envelope`]), and only then takes a change, judged
//! against that envelope; every [`Decision`] records the value the rules alone gave. A
//! change that breaks a cross-field rule is taken back and the plan computed again.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use crate::bounds;
use crate::cards::{Bed, Card, Cards, Feed, Glue, PlateStatus};
use crate::checklist::{self, ListBuilder};
use crate::envelope::{self, Bounds, Denied, Limits, proven_kept};
use crate::explain::{Catalogue, DataError, LINES, P, Places, PlanError};
use crate::facts::{OverhangRegion, PieceFacts, RegionKind};
use crate::inputs::{Baseline, PlanInputs, Spool};
use crate::level;
use crate::model::{
    Answer, Answered, Basis, BrimKind, Checklist, Condition, Decision, Derived, Envelope, Family,
    Field, FilamentSetting, InfillPattern, Intent, Notice, PrintPlan, PrintSettings, Revisions,
    RuleId, Seam, Source, SpoolSlot, SupportKind, SupportWhere, Value, WarpClass, Why,
};
use crate::propose::Changes;
use crate::ruledata::{Intents, RuleData};
use crate::signature::{FitSignature, SignatureParts};

/// Rounding slack when two decimals with two places are compared.
const EPS: f64 = 1e-9;

/// The rules with their data: thresholds, material cards, intents and sentences.
#[derive(Debug, Clone)]
pub struct RuleBook {
    data: RuleData,
    cards: Cards,
    intents: Intents,
    catalogue: Catalogue,
}

/// A value on its way into the plan.
struct Entry<'a> {
    value: Value,
    why: Why,
    sentence: &'a str,
    filled: Places<'a>,
    /// The envelope, where the rules define it themselves; otherwise it follows from the
    /// field and the plate's limits.
    bounds: Option<Bounds>,
}

/// The changes that were taken back because they broke a cross-field rule, with the rule
/// and the sentence of the answer.
type Reverted = BTreeMap<Field, (RuleId, &'static str)>;

/// Collects one decision per field.
struct Draft<'a> {
    catalogue: &'a Catalogue,
    limits: Limits,
    changes: &'a Changes,
    reverted: &'a Reverted,
    decisions: Vec<Option<Decision>>,
    /// The fields whose value the proven baseline set.
    proven_fields: Vec<Field>,
    /// A value of the proven baseline was held to what the plate allows.
    proven_limited: bool,
}

impl<'a> Draft<'a> {
    fn new(
        catalogue: &'a Catalogue,
        limits: Limits,
        changes: &'a Changes,
        reverted: &'a Reverted,
    ) -> Self {
        Self {
            catalogue,
            limits,
            changes,
            reverted,
            decisions: vec![None; Field::ALL.len()],
            proven_fields: Vec::new(),
            proven_limited: false,
        }
    }

    /// Records the decision of a field whose value the proven baseline gives (see
    /// [`Floor`]). The sentence names the day of the proven print; a value held to what the
    /// plate allows says so instead.
    fn proven(
        &mut self,
        field: Field,
        floor: &Floor,
        sentence: &str,
        date: P<'_>,
    ) -> Result<Value, PlanError> {
        let shown = floor.value.whole().unwrap_or(0);
        let (sentence, asked) = match floor.asked {
            Some(asked) => ("proven.limited", asked),
            None => (sentence, shown),
        };
        self.proven_fields.push(field);
        self.proven_limited |= floor.asked.is_some();
        self.plain(
            field,
            floor.value,
            PROVEN,
            sentence,
            &[
                ("value", P::Whole(shown)),
                ("asked", P::Whole(asked)),
                ("date", date),
            ],
        )
    }

    /// Records the rules' decision for a field, then applies the change somebody asked for,
    /// if there is one and the envelope of the field allows it. Returns the value the plan
    /// has for the field.
    fn set(&mut self, field: Field, entry: Entry<'_>) -> Result<Value, PlanError> {
        // Checked here as well as when the settings are built, so that the error names the
        // rule's field while the cause is still at hand.
        bounds::check(field, &entry.value)?;
        let (id, sentence) = self.catalogue.decision(entry.sentence, entry.filled)?;
        let bounds = entry
            .bounds
            .unwrap_or_else(|| self.limits.bounds(field, &entry.value, entry.why.rule));
        let mut decision = Decision::of_rules(
            field,
            entry.value,
            entry.why,
            id,
            sentence,
            bounds.envelope.clone(),
        );
        if let Some(change) = self.changes.get(field) {
            match self.reverted.get(&field) {
                Some((rule, detail)) => {
                    envelope::refuse(&mut decision, change, Answer::Conflict, *rule, detail);
                }
                None => envelope::apply(&mut decision, &bounds, change, self.catalogue)?,
            }
        }
        let value = decision.value;
        if let Some(slot) = self.decisions.get_mut(field.index()) {
            *slot = Some(decision);
        }
        Ok(value)
    }

    fn plain(
        &mut self,
        field: Field,
        value: Value,
        why: Why,
        sentence: &str,
        filled: Places<'_>,
    ) -> Result<Value, PlanError> {
        self.set(
            field,
            Entry {
                value,
                why,
                sentence,
                filled,
                bounds: None,
            },
        )
    }

    fn decision_mut(&mut self, field: Field) -> Option<&mut Decision> {
        self.decisions
            .get_mut(field.index())
            .and_then(Option::as_mut)
    }

    fn finish(self) -> Result<Vec<Decision>, PlanError> {
        self.decisions
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or(PlanError::Range(bounds::RangeError::Incomplete))
    }
}

fn rule(rule: RuleId, basis: Basis) -> Why {
    Why {
        source: Source::Rule,
        rule,
        basis,
    }
}

/// Why a value is what the proven baseline gives.
const PROVEN: Why = Why {
    source: Source::Proven,
    rule: RuleId::Prv1,
    basis: Basis::Measured,
};

/// A value the proven baseline gives a field.
struct Floor {
    /// The value of the plan.
    value: Value,
    /// The baseline's own value, when the plate allows less and `value` is held there.
    asked: Option<i64>,
}

/// The proven baseline of a plate, with the plan the rules give alone (without it), which
/// says whether a value is the baseline's doing.
struct Proven<'a> {
    baseline: Option<&'a Baseline>,
    alone: Option<&'a PrintPlan>,
}

impl Proven<'_> {
    /// The baseline's value for `field` when it is what the plan must take: the rules'
    /// value `rules` is less careful than the baseline, or equal to it while the rules alone
    /// (with the level they give without the baseline) are less careful. `top` is what the
    /// plate allows at most; a baseline value above it is held there. `None`: the rules
    /// decide the field.
    fn floor(&self, field: Field, rules: &Value, top: Option<i64>) -> Option<Floor> {
        let wanted = self.baseline?.get(field)?;
        let (value, asked) = match (wanted.whole(), top) {
            (Some(number), Some(top)) if number > top => (Value::Whole(top), Some(number)),
            _ => (wanted, None),
        };
        if !proven_kept(field, rules, &value) {
            return Some(Floor { value, asked });
        }
        let alone = self
            .alone
            .and_then(|plan| plan.decision(field))
            .map(|decision| decision.rule_value)?;
        (*rules == value && !proven_kept(field, &alone, &value)).then_some(Floor { value, asked })
    }
}

/// Why a value is not offered: the rule that keeps it out, and the sentence that says so.
type Out = (RuleId, &'static str);

/// A value the slicer side has not verified cannot be written.
const UNVERIFIED: Out = (RuleId::Xf1, "not_allowed.unverified");

/// The allowed set of an enum field: every token `judge` lets in. A token it keeps out is
/// remembered with the reason, for the answer to a change that asks for it.
fn choice<T: Copy>(
    all: &[T],
    judge: impl Fn(T) -> Result<(), Out>,
    token: impl Fn(T) -> &'static str,
) -> Bounds {
    let mut allowed = Vec::new();
    let mut denied = Vec::new();
    for candidate in all.iter().copied() {
        let value = Value::Token(token(candidate));
        match judge(candidate) {
            Ok(()) => allowed.push(value),
            Err((rule, detail)) => denied.push(Denied {
                value,
                rule,
                detail,
            }),
        }
    }
    Bounds::set(allowed, denied)
}

/// `Ok` when the condition holds, else the reason.
fn only_if(condition: bool, out: Out) -> Result<(), Out> {
    if condition { Ok(()) } else { Err(out) }
}

/// What the brim rules ask for on a plate.
struct BrimAsk {
    /// AD-1 to AD-5, as far as they fired.
    fired: BTreeSet<RuleId>,
    /// The widest width asked for, with the rule that asked and the number that made it.
    width: i64,
    rule: RuleId,
    detail: f64,
    /// A piece is slender enough for level 2 and gentle motion.
    very_slender: bool,
    /// A piece is slender enough for the warning to turn or split it.
    extreme: bool,
}

/// The overhang that counts towards supports (SUP-1, SUP-3).
struct Overhang {
    /// Every overhang, counted or not.
    total_mm2: f64,
    support_mm2: f64,
    regions: usize,
    largest_mm2: f64,
    /// Every piece came with its regions.
    classified: bool,
    /// The counted area that can force supports on: the bridges and free overhangs of the
    /// classified pieces. The ceiling of a round hole switches supports on and never forces
    /// them, because support inside a hole cannot be dug out; a piece without regions
    /// forces nothing, because nobody knows what its overhang is.
    forcing_mm2: f64,
    /// Counted overhang lies above its own piece: supports stand on the part by default.
    over_part: bool,
    /// And not only as the ceiling of a hole: supports on the bed alone would then leave a
    /// ledge unsupported, so that choice is not offered.
    over_part_forced: bool,
    fit_overhang: bool,
}

impl RuleBook {
    /// The rules with the data files compiled into the crate.
    pub fn builtin() -> Result<&'static RuleBook, DataError> {
        static BOOK: OnceLock<Result<RuleBook, DataError>> = OnceLock::new();
        BOOK.get_or_init(|| {
            RuleBook::from_toml(
                include_str!("../data/print-rules.toml"),
                include_str!("../data/print-cards.toml"),
                include_str!("../data/intents.toml"),
                include_str!("../data/sentences.toml"),
            )
        })
        .as_ref()
        .map_err(Clone::clone)
    }

    /// The rules with the given data files. Each file is read strictly and checked against
    /// the hard ranges, and the files are checked against each other.
    pub fn from_toml(
        rules: &str,
        cards: &str,
        intents: &str,
        sentences: &str,
    ) -> Result<Self, DataError> {
        let book = Self {
            data: RuleData::from_toml(rules)?,
            cards: Cards::from_toml(cards)?,
            intents: Intents::from_toml(intents)?,
            catalogue: Catalogue::from_toml(sentences)?,
        };
        for card in book.cards.all() {
            if let Some(unknown) = card
                .list
                .iter()
                .chain(card.grade.iter().flat_map(|grade| &grade.list))
                .find(|id| !LINES.iter().any(|(known, _)| known == id))
            {
                return Err(DataError::new(
                    "print-cards.toml",
                    format!(
                        "the {} card names the list line `{unknown}`, which the catalogue does not have",
                        card.family
                    ),
                ));
            }
        }
        Ok(book)
    }

    /// The thresholds of the rules.
    pub fn data(&self) -> &RuleData {
        &self.data
    }

    /// The material cards.
    pub fn cards(&self) -> &Cards {
        &self.cards
    }

    /// The intent table.
    pub fn intents(&self) -> &Intents {
        &self.intents
    }

    /// The sentence catalogue.
    pub fn catalogue(&self) -> &Catalogue {
        &self.catalogue
    }

    /// The revisions of the four data files.
    pub fn revisions(&self) -> Revisions {
        Revisions {
            print_rules: self.data.revision.clone(),
            print_cards: self.cards.revision().to_owned(),
            intents: self.intents.revision().to_owned(),
            sentences: self.catalogue.revision().to_owned(),
        }
    }

    /// The plan for plate facts the engine sent but that were refused (`facts_invalid`).
    pub fn refused_facts(&self) -> Result<PrintPlan, PlanError> {
        self.blocked(None, "facts_invalid", &[], None)
    }

    fn blocked(
        &self,
        inputs: Option<&PlanInputs>,
        code: &str,
        filled: Places<'_>,
        question: Option<&str>,
    ) -> Result<PrintPlan, PlanError> {
        let questions = match question {
            Some(code) => vec![self.catalogue.notice(code, &[])?],
            None => Vec::new(),
        };
        Ok(PrintPlan {
            settings: None,
            decisions: Vec::new(),
            facts: inputs.map(|inputs| inputs.facts.clone()),
            derived: None,
            questions,
            warnings: Vec::new(),
            notes: Vec::new(),
            blocked: Some(self.catalogue.notice(code, filled)?),
            checklist: Checklist::empty(),
            revisions: self.revisions(),
        })
    }

    /// AD-1 to AD-5 over the pieces of the plate; the plate takes the widest width.
    fn brim_ask(&self, pieces: &[PieceFacts], card: &Card) -> BrimAsk {
        let brim = &self.data.brim;
        let mut ask = BrimAsk {
            fired: BTreeSet::new(),
            width: 0,
            rule: RuleId::Ad0,
            detail: 0.0,
            very_slender: false,
            extreme: false,
        };
        let mut candidates: Vec<(RuleId, i64, f64)> = Vec::new();
        for piece in pieces {
            let strip = piece.thin_strip_mm();
            if strip < brim.thin_strip.below_mm {
                candidates.push((RuleId::Ad1, brim.thin_strip.width_mm, strip));
            }
            if piece.contact_mm2 < brim.small_foot.tiny_below_mm2 {
                candidates.push((
                    RuleId::Ad2,
                    brim.small_foot.tiny_width_mm,
                    piece.contact_mm2,
                ));
            } else if piece.contact_mm2 < brim.small_foot.below_mm2 {
                candidates.push((RuleId::Ad2, brim.small_foot.width_mm, piece.contact_mm2));
            }
            let tipping = piece.tipping();
            if tipping > brim.slender.very_above {
                candidates.push((RuleId::Ad3, brim.slender.very_width_mm, tipping));
                ask.very_slender = true;
            } else if tipping > brim.slender.above {
                candidates.push((RuleId::Ad3, brim.slender.width_mm, tipping));
            }
            ask.extreme |= tipping > brim.slender.warn_above;
            match card.warp_class {
                WarpClass::Low if piece.contact_span_mm > brim.long_part.low_above_mm => {
                    candidates.push((
                        RuleId::Ad4,
                        brim.long_part.low_width_mm,
                        piece.contact_span_mm,
                    ));
                }
                WarpClass::Medium if piece.contact_span_mm > brim.long_part.medium_above_mm => {
                    candidates.push((
                        RuleId::Ad4,
                        brim.long_part.medium_width_mm,
                        piece.contact_span_mm,
                    ));
                }
                _ => {}
            }
        }
        if card.warp_class == WarpClass::High {
            candidates.push((RuleId::Ad5, brim.warping.width_mm, 0.0));
        }
        for (rule, width, detail) in candidates {
            ask.fired.insert(rule);
            // The widest wins; among equal widths the lower rule number, so that the reason
            // shown does not depend on the order of the pieces.
            if width > ask.width
                || (width == ask.width && rule < ask.rule)
                || ask.rule == RuleId::Ad0
            {
                (ask.width, ask.rule, ask.detail) = (width, rule, detail);
            }
        }
        ask
    }

    /// Whether a region counts towards supports: not a short bridge, not the ceiling of a
    /// small round hole, not a free overhang with a short reach.
    fn counts(&self, region: &OverhangRegion) -> bool {
        let supports = &self.data.supports;
        let limit = match region.kind {
            RegionKind::Bridge => supports.bridge_max_mm,
            RegionKind::HoleCeiling => supports.hole_max_mm,
            RegionKind::Free => supports.reach_max_mm,
        };
        // A region without its length cannot be shown to be harmless.
        region.length_mm().is_none_or(|length| length > limit)
    }

    fn overhang(&self, pieces: &[PieceFacts]) -> Overhang {
        let supports = &self.data.supports;
        let mut found = Overhang {
            total_mm2: pieces.iter().map(|piece| piece.overhang_mm2).sum(),
            support_mm2: 0.0,
            regions: 0,
            largest_mm2: 0.0,
            classified: true,
            forcing_mm2: 0.0,
            over_part: false,
            over_part_forced: false,
            fit_overhang: false,
        };
        for piece in pieces {
            found.fit_overhang |= piece.fit_overhang_mm2 >= supports.fit_overhang_from_mm2;
            match &piece.overhang_regions {
                Some(regions) => {
                    // Area above the piece itself, by the regions' flags: in every counted
                    // region, and in the counted regions that are not a hole ceiling.
                    let (mut over, mut over_solid) = (0.0, 0.0);
                    // The area of the regions that are not counted, and of the counted hole
                    // ceilings.
                    let (mut ignored, mut holes) = (0.0, 0.0);
                    for region in regions {
                        if !self.counts(region) {
                            ignored += region.area_mm2;
                            continue;
                        }
                        found.support_mm2 += region.area_mm2;
                        found.regions += 1;
                        found.largest_mm2 = found.largest_mm2.max(region.area_mm2);
                        let hole = region.kind == RegionKind::HoleCeiling;
                        if hole {
                            holes += region.area_mm2;
                        } else {
                            found.forcing_mm2 += region.area_mm2;
                        }
                        if region.over_part {
                            over += region.area_mm2;
                            if !hole {
                                over_solid += region.area_mm2;
                            }
                        }
                    }
                    // A flag is set per region, so several regions that each hang a little
                    // over the part carry none. The piece's own total is the second witness:
                    // what the other regions cannot hold of it lies in the regions asked about.
                    let stated = piece.overhang_over_part_mm2;
                    over = over.max(stated - ignored);
                    over_solid = over_solid.max(stated - ignored - holes);
                    found.over_part |= over >= supports.over_part_from_mm2;
                    found.over_part_forced |= over_solid >= supports.over_part_from_mm2;
                }
                // Not classified: the whole overhang counts, and forces nothing.
                None => {
                    found.classified = false;
                    found.support_mm2 += piece.overhang_mm2;
                    found.largest_mm2 = found.largest_mm2.max(piece.overhang_mm2);
                    let over = piece.overhang_over_part_mm2 >= supports.over_part_from_mm2;
                    found.over_part |= over;
                    found.over_part_forced |= over;
                }
            }
        }
        found
    }

    /// The spool of the plate: the only declared spool with the plate's material, or the
    /// person's choice among several.
    fn spool<'a>(
        &self,
        inputs: &'a PlanInputs,
        family: Family,
    ) -> Result<(&'a Spool, Source), &'static str> {
        let matching: Vec<&Spool> = inputs
            .setup
            .spools()
            .iter()
            .filter(|spool| spool.material_id == inputs.material && spool.family == family)
            .collect();
        match (matching.as_slice(), inputs.spool_choice) {
            ([], _) => Err("spools_not_declared"),
            ([only], _) => Ok((*only, Source::Rule)),
            (several, Some(choice)) => several
                .iter()
                .find(|spool| spool.slot == choice)
                .map(|spool| (*spool, Source::Person))
                .ok_or("spool_not_chosen"),
            (_, None) => Err("spool_not_chosen"),
        }
    }

    /// Computes the plan of one plate. A plan that cannot be printed as asked comes back
    /// blocked, with the code and the sentence of the reason; an `Err` means that a data
    /// file and the code disagree.
    ///
    /// The plan is a function of `inputs` alone, the accepted changes among them. Every
    /// plan that is returned has passed [`RuleBook::verify`]. A change that breaks a
    /// cross-field rule is taken back, with the answer `conflict` in its decision, and the
    /// plan is computed again without it.
    pub fn plan(&self, inputs: &PlanInputs) -> Result<PrintPlan, PlanError> {
        let mut reverted = Reverted::new();
        loop {
            let plan = self.compute(inputs, &reverted)?;
            let Err(violation) = self.verify(inputs, &plan) else {
                return Ok(plan);
            };
            // The change of the field that breaks the rule goes first; failing that, the
            // last change that is in effect. Each round takes one more change back, so the
            // loop ends, at the latest with the rule plan.
            let in_effect = |field: Field| {
                !reverted.contains_key(&field)
                    && inputs.changes.get(field).is_some()
                    && plan
                        .decision(field)
                        .and_then(|decision| decision.answer)
                        .is_some_and(|answered| answered.answer.applied())
            };
            let culprit = Some(violation.field)
                .filter(|field| in_effect(*field))
                .or_else(|| {
                    Field::ALL
                        .iter()
                        .rev()
                        .copied()
                        .find(|field| in_effect(*field))
                });
            match culprit {
                Some(field) => reverted.insert(field, (violation.rule, violation.detail)),
                None => return Err(PlanError::Cross(violation)),
            };
        }
    }

    /// One pass of the rules over the inputs, without the changes in `reverted`.
    fn compute(&self, inputs: &PlanInputs, reverted: &Reverted) -> Result<PrintPlan, PlanError> {
        let cat = &self.catalogue;
        let data = &self.data;
        let facts = &inputs.facts;
        let pieces = facts.pieces();
        let family = facts.family();
        let family_word = P::Word(family.as_str());
        let card = self
            .cards
            .card(family)
            .ok_or_else(|| PlanError::Sentence("card".to_owned()))?;

        // ---- What must be answered or changed before there is a plan. ----
        let Some(plate) = inputs.setup.plate_type() else {
            return self.blocked(
                Some(inputs),
                "bed_type_not_chosen",
                &[],
                Some("question.plate_type"),
            );
        };
        let plate_word = P::Word(cat.word("plate", plate.as_str())?);
        let Some(on) = card.on(plate) else {
            return self.blocked(
                Some(inputs),
                "plate_not_supported",
                &[],
                Some("question.plate_type"),
            );
        };
        if inputs.setup.spools().is_empty() {
            let filled = [("family", family_word)];
            return self.blocked(
                Some(inputs),
                "spools_not_declared",
                &filled,
                Some("question.spools"),
            );
        }
        let (spool, spool_source) = match self.spool(inputs, family) {
            Ok(found) => found,
            Err(code) => {
                let question = if code == "spool_not_chosen" {
                    "question.spool_choice"
                } else {
                    "question.spools"
                };
                return self.blocked(
                    Some(inputs),
                    code,
                    &[("family", family_word)],
                    Some(question),
                );
            }
        };
        // PLT-1.
        let bed = match (on.status, on.bed_values()) {
            (PlateStatus::Ok | PlateStatus::Warning, Some(bed)) => bed,
            _ => {
                let filled = [("family", family_word), ("plate", plate_word)];
                return self.blocked(Some(inputs), "plate_material_conflict", &filled, None);
            }
        };
        // PLT-2: a bed temperature lies inside the card's window and at or below
        // the printer's and the filament's limit. Below the window there is none, and a bed
        // colder than the window is not a plan.
        let bed_limit_c = inputs.printer.bed_limit_c();
        if bed_limit_c < bed.window[0] {
            let filled = [
                ("family", family_word),
                ("plate", plate_word),
                ("low", P::Whole(bed.window[0])),
                ("limit", P::Whole(bed_limit_c)),
            ];
            return self.blocked(Some(inputs), "bed_limit_below_window", &filled, None);
        }
        // MAT-2.
        let shore = inputs.material.shore_a();
        let nozzle_mm = inputs.printer.nozzle_diameter_mm();
        if card.flexible
            && shore.is_some_and(|shore| shore <= data.flexible.refused_shore_a)
            && nozzle_mm <= data.flexible.refused_nozzle_mm + EPS
        {
            let filled = [
                ("shore", P::Whole(i64::from(shore.unwrap_or(0)))),
                ("nozzle", P::Number(nozzle_mm)),
            ];
            return self.blocked(Some(inputs), "material_not_printable", &filled, None);
        }
        if card.feed == Feed::ExternalMain && spool.slot != SpoolSlot::ExternalMain {
            return self.blocked(
                Some(inputs),
                "material_not_allowed",
                &[("family", family_word)],
                None,
            );
        }
        // NOZ-1: only the main nozzle, and only a slot the printer data know.
        let feed = match inputs.printer.feed(spool.slot) {
            Some(feed) if feed.main => feed,
            _ => return self.blocked(Some(inputs), "nozzle_choice_unproven", &[], None),
        };
        // For the baseline: the first layer the standard preset brings, and that STK-1
        // writes, must be one this nozzle can print. Otherwise no value of this plan is
        // proven for the nozzle, and nothing is planned.
        let height_limit = (data.cross.first_layer_height_max_factor * nozzle_mm)
            .min(inputs.printer.max_layer_height_mm());
        let baseline_width = data.baseline.first_layer_line_width_mm;
        if data.baseline.first_layer_height_mm > height_limit + EPS
            || baseline_width < data.cross.line_width_min_factor * nozzle_mm - EPS
            || baseline_width > data.cross.line_width_max_factor * nozzle_mm + EPS
        {
            let filled = [("nozzle", P::Number(nozzle_mm))];
            return self.blocked(Some(inputs), "nozzle_size_unproven", &filled, None);
        }

        // ---- The proven baseline: a floor, and the plan the rules give without it. ----
        let baseline = inputs.applicable_baseline();
        let alone = match baseline {
            Some(_) => {
                let mut without = inputs.clone();
                without.baseline = None;
                Some(self.compute(&without, reverted)?)
            }
            None => None,
        };
        let proven = Proven {
            baseline,
            alone: alone.as_ref(),
        };
        // The day of the proven print, for the sentences that name it.
        let date = baseline.map_or(P::Whole(0), |baseline| P::Date(baseline.printed_on()));

        // ---- Brim rules and the level. ----
        let ask = self.brim_ask(pieces, card);
        let fit_plate = facts.fit_at_bed();
        let (asked_width, asked_rule) = if card.flexible {
            // AD-6 replaces the widths of the rules above.
            let thin_or_tall = ask.fired.contains(&RuleId::Ad1) || ask.fired.contains(&RuleId::Ad3);
            let width = if thin_or_tall {
                data.brim.flexible.thin_or_tall_width_mm
            } else {
                data.brim.flexible.width_mm
            };
            (width, RuleId::Ad6)
        } else {
            (ask.width, ask.rule)
        };
        let geometry_level = match (ask.very_slender, ask.fired.is_empty()) {
            (true, _) => 2,
            (false, false) => 1,
            (false, true) => 0,
        };
        let outcome = level::decide(
            geometry_level,
            spool.silk,
            &inputs.history,
            baseline.map(|baseline| baseline.values().level),
            inputs.level.as_ref(),
            &data.level,
        );
        let level = outcome.level;
        let step = level.clamp(0, 2) as usize;
        let level_word = P::Whole(level);

        // ---- Calibration. A gauge defines a signature and takes none. ----
        let calibration = inputs
            .calibration
            .as_ref()
            .map(FitSignature::parts)
            .filter(|parts| {
                !inputs.gauge
                    && parts.material_id == inputs.material
                    && parts.nozzle_id == feed.nozzle_id
            });
        let mut signature_differs = false;
        // Calibrated first layer values that this nozzle cannot print are not used.
        let calibrated = calibration.filter(|parts| {
            let fits = parts.first_layer_height_mm <= height_limit + EPS
                && parts.first_layer_line_width_mm
                    >= data.cross.line_width_min_factor * nozzle_mm - EPS
                && parts.first_layer_line_width_mm
                    <= data.cross.line_width_max_factor * nozzle_mm + EPS;
            signature_differs |= !fits;
            fits
        });
        let foot_mm = calibrated.map_or(data.baseline.elephant_foot_mm, |parts| {
            parts.elephant_foot_mm
        });
        let line_width_mm = calibrated.map_or(data.baseline.first_layer_line_width_mm, |parts| {
            parts.first_layer_line_width_mm
        });

        // ---- The limits the envelopes of this plate end at. ----
        // MAT-1: thin walls print as walls only.
        let thin_walls = pieces.iter().any(|piece| {
            piece.thin_strip_mm() < data.walls.strip_below_mm
                || piece
                    .min_wall_mm
                    .is_some_and(|wall| wall <= data.walls.min_wall_at_most_mm + EPS)
        });
        let wall_floor = if thin_walls {
            data.walls.loops
        } else {
            bounds::WALL_LOOPS.0
        };
        // The envelope of the wall loops starts at the proven baseline's, where that is more.
        let proven_walls = baseline.map_or(0, |baseline| baseline.values().wall_loops);
        let (plan_wall_floor, wall_floor_owner) = if proven_walls > wall_floor {
            (proven_walls, RuleId::Prv1)
        } else {
            (wall_floor, RuleId::Mat1)
        };
        let brim_width_max = if card.warp_class == WarpClass::High {
            data.brim.limit.max_warp_high_mm
        } else {
            data.brim.limit.max_mm
        };
        let limits = Limits {
            level_max: data.level.ask_max,
            brim_width_max,
            // PLT-2: the window ends where the printer or the filament does.
            bed_top: bed.window[1].min(bed_limit_c),
            fan_off_max: data.level.fan_off_layers,
            first_layer_accel_top: data.baseline.first_layer_accel_mm_s2,
            wall_floor: plan_wall_floor,
            wall_floor_owner,
            threshold_max: 90 - card.max_overhang_deg,
        };

        let mut draft = Draft::new(cat, limits, &inputs.changes, reverted);
        let mut warnings: Vec<Notice> = Vec::new();
        let mut notes: Vec<Notice> = Vec::new();

        // ---- Setup and filament. ----
        let person = Why {
            source: Source::Person,
            rule: RuleId::Set1,
            basis: Basis::Measured,
        };
        draft.plain(
            Field::PlateType,
            Value::Token(plate.as_str()),
            person,
            "setup.plate",
            &[("plate", plate_word)],
        )?;
        let spool_why = Why {
            source: spool_source,
            rule: RuleId::Set1,
            basis: Basis::Measured,
        };
        match spool.slot {
            SpoolSlot::Ams(number) => draft.plain(
                Field::Spool,
                Value::Slot(spool.slot),
                spool_why,
                "setup.spool.ams",
                &[
                    ("family", family_word),
                    ("slot", P::Whole(i64::from(number))),
                ],
            )?,
            _ => draft.plain(
                Field::Spool,
                Value::Slot(spool.slot),
                spool_why,
                "setup.spool.external",
                &[("family", family_word)],
            )?,
        };
        draft.plain(
            Field::NozzleId,
            Value::Whole(i64::from(feed.nozzle_id)),
            rule(RuleId::Noz1, Basis::Measured),
            "setup.nozzle",
            &[],
        )?;
        draft.plain(
            Field::Colour,
            Value::Colour(spool.colour),
            person,
            "setup.colour",
            &[],
        )?;

        // ---- Intent. It is read here and used only below the sticking group. ----
        let (intent, intent_why, intent_sentence, intent_bounds) = if inputs.gauge {
            (
                Intent::FitCritical,
                rule(RuleId::Sig1, Basis::Judgment),
                "intent.gauge",
                Bounds::locked(RuleId::Sig1),
            )
        } else {
            let all = choice(Intent::ALL, |_| Ok(()), Intent::as_str);
            match inputs.intent {
                Some(asked) => (
                    asked.value,
                    Why {
                        source: asked.by.source(),
                        rule: RuleId::Int1,
                        basis: Basis::Judgment,
                    },
                    "intent.chosen",
                    all,
                ),
                None => (
                    Intent::Balanced,
                    rule(RuleId::Int1, Basis::Stock),
                    "intent.default",
                    all,
                ),
            }
        };
        let intent_word = P::Word(cat.word("intent", intent.as_str())?);
        draft.set(
            Field::Intent,
            Entry {
                value: Value::Token(intent.as_str()),
                why: intent_why,
                sentence: intent_sentence,
                filled: &[("intent", intent_word)],
                bounds: Some(intent_bounds),
            },
        )?;
        if let Some(decision) = draft.decision_mut(Field::Intent) {
            if !inputs.gauge && inputs.intent.is_some() {
                decision.rule_value = Value::Token(Intent::Balanced.as_str());
                decision.rule_source = Source::Rule;
            }
            if inputs.gauge {
                // A gauge keeps its intent whatever was asked for.
                decision.asked = inputs
                    .intent
                    .filter(|asked| asked.value != Intent::FitCritical)
                    .map(|asked| Value::Token(asked.value.as_str()));
            }
            decision.answer = inputs.intent.map(|asked| {
                let locked = inputs.gauge && asked.value != Intent::FitCritical;
                Answered {
                    by: asked.by,
                    answer: if locked {
                        Answer::Locked
                    } else {
                        Answer::Accepted
                    },
                    rule: decision.rule,
                    detail: if locked { "locked" } else { "accepted" },
                }
            });
        }

        // ---- Sticking group. ----
        let first_fired = ask.fired.iter().next().map_or("", |rule| rule.as_str());
        // The envelope starts at the level the rules, the spool and the history give.
        let level_bounds = limits.bounds(
            Field::Level,
            &Value::Whole(outcome.rule_level),
            outcome.rule_why.rule,
        );
        // The sentence of level 1 names what the level changes for this card on this plate.
        let top = bed.window[1].min(bed_limit_c);
        let slower = card.first_layer.walls_mm_s[1] < card.first_layer.walls_mm_s[0]
            || card.first_layer.infill_mm_s[1] < card.first_layer.infill_mm_s[0];
        let hotter = bed.raised[0].min(top) > bed.normal[0].min(top)
            || (!fit_plate && bed.raised[1].min(top) > bed.normal[1].min(top));
        let level_sentence = match (outcome.sentence, slower, hotter) {
            ("level.1", true, false) => "level.1.slower",
            ("level.1", false, true) => "level.1.hotter",
            ("level.1", false, false) => "level.1.same",
            (sentence, _, _) => sentence,
        };
        draft.set(
            Field::Level,
            Entry {
                value: Value::Whole(level),
                why: outcome.why,
                sentence: level_sentence,
                filled: &[
                    ("rule", P::Word(first_fired)),
                    ("level", level_word),
                    ("date", date),
                ],
                bounds: Some(level_bounds),
            },
        )?;
        if outcome.rule_why.source == Source::Proven {
            draft.proven_fields.push(Field::Level);
        }
        if let Some(decision) = draft.decision_mut(Field::Level) {
            decision.rule_value = Value::Whole(outcome.rule_level);
            decision.rule_source = outcome.rule_why.source;
            decision.rule_rule = outcome.rule_why.rule;
            decision.asked = outcome.not_taken.map(Value::Whole);
            decision.answer = inputs.level.map(|asked| {
                // Held to the rules' level from below, or to the highest level a request
                // can reach from above.
                let held = outcome.not_taken.map(|wanted| {
                    if wanted < level {
                        outcome.rule_why.rule
                    } else {
                        RuleId::LvlAsk
                    }
                });
                Answered {
                    by: asked.by,
                    answer: if held.is_some() {
                        Answer::Clamped
                    } else {
                        Answer::Accepted
                    },
                    rule: held.unwrap_or(decision.rule),
                    detail: if held.is_some() {
                        "clamped"
                    } else {
                        "accepted"
                    },
                }
            });
        }

        // Brim width: the rules' width, widened at level 2.
        let mut width = asked_width;
        let mut widened = false;
        if level >= 2 {
            let wider = (asked_width + data.level.brim_extra_mm)
                .max(data.level.brim_min_mm)
                .min(data.level.brim_max_mm)
                .max(asked_width);
            widened = wider > width;
            width = wider;
        }
        // The proven baseline's brim, where the rules ask for a narrower one.
        let rules_width = width;
        let proven_width =
            proven.floor(Field::BrimWidth, &Value::Whole(width), Some(brim_width_max));
        if let Some(floor) = &proven_width {
            width = floor.value.whole().unwrap_or(width);
        }
        // A brim the rules or the proven baseline draw is there to hold the part.
        let brim_required = width > 0;
        // The rule that requires the brim: a brim rule, level 2 where none fired, or the
        // proven baseline where the rules draw none.
        let kind_rule = if proven_width.is_some() && rules_width == 0 {
            RuleId::Prv1
        } else if brim_required && widened && asked_width == 0 {
            RuleId::Lvl2
        } else {
            asked_rule
        };
        let width_word = P::Whole(width);
        let (width_why, width_sentence, width_detail): (Why, &str, (&'static str, P<'_>)) =
            if widened {
                (
                    rule(RuleId::Lvl2, data.level.mark),
                    "brim.width.level2",
                    ("width", width_word),
                )
            } else {
                match asked_rule {
                    RuleId::Ad1 => (
                        rule(RuleId::Ad1, data.brim.thin_strip.mark),
                        "brim.width.thin_strip",
                        ("strip", P::Number(ask.detail)),
                    ),
                    RuleId::Ad2 => (
                        rule(RuleId::Ad2, data.brim.small_foot.mark),
                        "brim.width.small_foot",
                        ("contact", P::Whole(ask.detail.round() as i64)),
                    ),
                    RuleId::Ad3 => (
                        rule(RuleId::Ad3, data.brim.slender.mark),
                        "brim.width.slender",
                        ("ratio", P::Number((ask.detail * 10.0).round() / 10.0)),
                    ),
                    RuleId::Ad4 => (
                        rule(RuleId::Ad4, data.brim.long_part.mark),
                        "brim.width.long",
                        ("span", P::Whole(ask.detail.round() as i64)),
                    ),
                    RuleId::Ad5 => (
                        rule(RuleId::Ad5, data.brim.warping.mark),
                        "brim.width.warping",
                        ("family", family_word),
                    ),
                    RuleId::Ad6 => (
                        rule(RuleId::Ad6, data.brim.flexible.mark),
                        "brim.width.flexible",
                        ("family", family_word),
                    ),
                    _ => (
                        rule(RuleId::Ad0, Basis::Judgment),
                        "brim.width.none",
                        ("width", width_word),
                    ),
                }
            };
        // The width takes a change before the kind is decided: the kind follows the width.
        let drawn_width = match &proven_width {
            Some(floor) => draft.proven(Field::BrimWidth, floor, "proven.brim_width", date)?,
            None => draft.plain(
                Field::BrimWidth,
                Value::Whole(width),
                width_why,
                width_sentence,
                &[("width", width_word), width_detail],
            )?,
        }
        .whole()
        .unwrap_or(width);
        let drawn = drawn_width > 0;
        let kind = if drawn {
            BrimKind::Outer
        } else {
            BrimKind::None
        };

        // FIT-1: an inner brim needs a verified value, an inner loop, room in
        // every inner loop of the plate (the kind is one value for all pieces), and no bore
        // of a declared fit on the plate. `inner` alone would take the outer brim
        // away, so it is never offered where a brim is required. `none` is offered only
        // while no brim is drawn: a required brim cannot be switched off, and a brim
        // somebody asked for is removed by asking for width 0.
        let inner_room = 2.0 * drawn_width as f64 + data.brim.inner.extra_mm;
        let loops = || pieces.iter().flat_map(|piece| piece.inner_loops.iter());
        let no_fit_bore = !loops().any(|inner| inner.fit);
        let wide_hole =
            loops().next().is_some() && loops().all(|inner| inner.min_width_mm >= inner_room - EPS);
        let inside = |candidate: BrimKind| {
            only_if(no_fit_bore, (RuleId::Fit1, "not_allowed.brim_inner_fit"))?;
            if candidate == BrimKind::Inner {
                only_if(!brim_required, (kind_rule, "not_allowed.brim_inner_alone"))?;
            }
            only_if(wide_hole, (RuleId::Xf1, "not_allowed.brim_inner_room"))?;
            only_if(
                inputs
                    .capabilities
                    .verified(Field::BrimKind, candidate.as_str()),
                UNVERIFIED,
            )
        };
        let kinds = choice(
            BrimKind::ALL,
            |candidate| match candidate {
                BrimKind::None if !drawn => Ok(()),
                BrimKind::None if brim_required => Err((kind_rule, "not_allowed.brim_required")),
                BrimKind::None => Err((RuleId::Xf1, "not_allowed.brim_asked")),
                BrimKind::Outer => Ok(()),
                BrimKind::Inner | BrimKind::OuterAndInner => inside(candidate),
            },
            BrimKind::as_str,
        );
        // A brim that only a change asked for: the kind follows that change.
        let followed = drawn && !brim_required;
        let (kind_why, kind_sentence) = if followed {
            let asker = inputs
                .changes
                .get(Field::BrimWidth)
                .map_or(Source::Rule, |change| change.by.source());
            let why = Why {
                source: asker,
                rule: RuleId::Xf1,
                basis: Basis::Judgment,
            };
            (why, "brim.kind.asked")
        } else if brim_required && proven_width.is_some() {
            draft.proven_fields.push(Field::BrimKind);
            (PROVEN, "proven.brim_kind")
        } else if brim_required {
            (rule(kind_rule, Basis::Measured), "brim.kind.outer")
        } else {
            (rule(kind_rule, Basis::Measured), "brim.kind.none")
        };
        draft.set(
            Field::BrimKind,
            Entry {
                value: Value::Token(kind.as_str()),
                why: kind_why,
                sentence: kind_sentence,
                filled: &[("date", date)],
                bounds: Some(kinds),
            },
        )?;
        if followed && let Some(decision) = draft.decision_mut(Field::BrimKind) {
            // The rules alone drew no brim here.
            decision.rule_value = Value::Token(BrimKind::None.as_str());
            decision.rule_source = Source::Rule;
            decision.rule_rule = kind_rule;
        }

        // FIT-2: a brim that a rule or the proven baseline requires touches the part.
        let (gap, gap_why, gap_sentence) = if card.flexible {
            (
                card.brim_gap_mm,
                rule(RuleId::Card, card.brim_gap_mark),
                "brim.gap.flexible",
            )
        } else if brim_required && foot_mm >= data.brim.gap.min_elephant_foot_mm - EPS {
            (
                data.brim.gap.required_mm,
                rule(RuleId::Fit2, data.brim.gap.mark),
                "brim.gap.attached",
            )
        } else {
            (
                card.brim_gap_mm,
                rule(RuleId::Card, card.brim_gap_mark),
                "brim.gap.loose",
            )
        };
        draft.plain(
            Field::BrimGap,
            Value::decimal(gap),
            gap_why,
            gap_sentence,
            &[("gap", P::Number(gap))],
        )?;
        draft.plain(
            Field::SkirtLoops,
            Value::Whole(0),
            rule(RuleId::Fix1, Basis::Stock),
            "fixed.skirt",
            &[],
        )?;

        // The intent's preset decides whether STK-1 writes the baseline.
        let balanced = self
            .intents
            .row(Intent::Balanced)
            .ok_or_else(|| PlanError::Sentence("intents".to_owned()))?;
        let row = self
            .intents
            .row(intent)
            .ok_or_else(|| PlanError::Sentence("intents".to_owned()))?;
        let mut unavailable = false;
        let preset_installed =
            inputs.capabilities.has_preset(row.process) || row.process == balanced.process;
        let process = if preset_installed {
            row.process
        } else {
            balanced.process
        };
        unavailable |= !preset_installed;
        let stock_preset = process == data.baseline.preset;

        // The three calibrated fields and the first layer acceleration.
        let thick_first_layer = level >= 2
            && plate == data.level.first_layer_height_plate
            && !fit_plate
            && data.level.first_layer_height_mm <= height_limit + EPS;
        let baseline_fields = [
            (
                Field::ElephantFoot,
                data.baseline.elephant_foot_mm,
                calibrated.map(|parts| parts.elephant_foot_mm),
            ),
            (
                Field::FirstLayerHeight,
                data.baseline.first_layer_height_mm,
                calibrated.map(|parts| parts.first_layer_height_mm),
            ),
            (
                Field::FirstLayerLineWidth,
                data.baseline.first_layer_line_width_mm,
                calibrated.map(|parts| parts.first_layer_line_width_mm),
            ),
        ];
        for (field, stock, recorded) in baseline_fields {
            let (value, why, sentence, shown) = if let Some(recorded) = recorded {
                let why = Why {
                    source: Source::Calibration,
                    rule: RuleId::Sig1,
                    basis: Basis::Measured,
                };
                (
                    Value::decimal(recorded),
                    why,
                    "calibration.locked",
                    recorded,
                )
            } else if field == Field::FirstLayerHeight && thick_first_layer {
                let height = data.level.first_layer_height_mm;
                (
                    Value::decimal(height),
                    rule(RuleId::Lvl2, Basis::Sourced),
                    "level2.first_layer_height",
                    height,
                )
            } else if !stock_preset {
                (
                    Value::decimal(stock),
                    rule(RuleId::Stk1, data.baseline.mark),
                    "stock.written",
                    stock,
                )
            } else {
                let why = Why {
                    source: Source::Profile,
                    rule: RuleId::Stk1,
                    basis: Basis::Stock,
                };
                (Value::Unset, why, "stock.kept", stock)
            };
            draft.plain(field, value, why, sentence, &[("value", P::Number(shown))])?;
            // FIT-4: with mating faces on the bed nobody tunes these three, whatever set
            // them. The lock names that rule, so that the envelope step cannot open it.
            if fit_plate && let Some(decision) = draft.decision_mut(field) {
                decision.envelope = Envelope::Locked { rule: RuleId::Fit4 };
            }
        }

        let speeds = &card.first_layer;
        let card_level = [("family", family_word), ("level", level_word)];
        let wall_speed = Value::Whole(speeds.walls_mm_s[step]);
        match proven.floor(Field::FirstLayerWallSpeed, &wall_speed, None) {
            Some(floor) => draft.proven(
                Field::FirstLayerWallSpeed,
                &floor,
                "proven.wall_speed",
                date,
            )?,
            None => draft.plain(
                Field::FirstLayerWallSpeed,
                wall_speed,
                rule(RuleId::Card, speeds.marks[step]),
                "first_layer.wall_speed",
                &[
                    ("value", P::Whole(speeds.walls_mm_s[step])),
                    card_level[0],
                    card_level[1],
                ],
            )?,
        };
        let infill_speed = Value::Whole(speeds.infill_mm_s[step]);
        match proven.floor(Field::FirstLayerInfillSpeed, &infill_speed, None) {
            Some(floor) => draft.proven(
                Field::FirstLayerInfillSpeed,
                &floor,
                "proven.infill_speed",
                date,
            )?,
            None => draft.plain(
                Field::FirstLayerInfillSpeed,
                infill_speed,
                rule(RuleId::Card, speeds.marks[step]),
                "first_layer.infill_speed",
                &[
                    ("value", P::Whole(speeds.infill_mm_s[step])),
                    card_level[0],
                    card_level[1],
                ],
            )?,
        };
        let accel = data.baseline.first_layer_accel_mm_s2;
        if stock_preset {
            let why = Why {
                source: Source::Profile,
                rule: RuleId::Stk1,
                basis: Basis::Stock,
            };
            draft.plain(
                Field::FirstLayerAccel,
                Value::Unset,
                why,
                "stock.kept",
                &[("value", P::Whole(accel))],
            )?;
        } else {
            let why = rule(RuleId::Stk1, data.baseline.mark);
            draft.plain(
                Field::FirstLayerAccel,
                Value::Whole(accel),
                why,
                "stock.written",
                &[("value", P::Whole(accel))],
            )?;
        }

        // Bed temperatures (FIT-5, PLT-2).
        let beds = self.bed(
            &bed,
            BedAsk {
                step,
                fit_plate,
                calibration,
                level_by: outcome.why.source,
                limit_c: bed_limit_c,
            },
        );
        signature_differs |= beds.differs;
        if beds.limited {
            warnings.push(cat.notice("bed_limited", &[])?);
        }
        // The proven baseline heats the bed where the rules give a cooler one, never above
        // the window (cut at the printer's limit); the other layers never above the first.
        let proven_first = proven.floor(
            Field::FirstLayerBed,
            &Value::Whole(beds.pair[0]),
            Some(limits.bed_top),
        );
        let rule_first = proven_first
            .as_ref()
            .and_then(|floor| floor.value.whole())
            .unwrap_or(beds.pair[0]);
        let proven_other = proven.floor(
            Field::LayersBed,
            &Value::Whole(beds.pair[1]),
            Some(rule_first),
        );
        let rule_other = proven_other
            .as_ref()
            .and_then(|floor| floor.value.whole())
            .unwrap_or(beds.pair[1]);
        // A change can only heat the first layer further, so the other layers stay at or
        // below it. What follows reads the pair as the plan has it.
        let first_bed = match &proven_first {
            Some(floor) => draft.proven(Field::FirstLayerBed, floor, "proven.bed_first", date)?,
            None => draft.plain(
                Field::FirstLayerBed,
                Value::Whole(beds.pair[0]),
                beds.first.0,
                beds.first.1,
                &[
                    ("value", P::Whole(beds.pair[0])),
                    card_level[0],
                    card_level[1],
                ],
            )?,
        }
        .whole()
        .unwrap_or(rule_first);
        let bed_pair = [first_bed, rule_other];
        match &proven_other {
            Some(floor) => draft.proven(Field::LayersBed, floor, "proven.bed_other", date)?,
            None => draft.plain(
                Field::LayersBed,
                Value::Whole(beds.pair[1]),
                beds.other.0,
                beds.other.1,
                &[("value", P::Whole(beds.pair[1])), card_level[1]],
            )?,
        };

        let fixed = Why {
            source: Source::Profile,
            rule: RuleId::Fix1,
            basis: Basis::Stock,
        };
        draft.plain(
            Field::FirstLayerNozzle,
            Value::Unset,
            fixed,
            "stock.nozzle",
            &[],
        )?;
        draft.plain(
            Field::LayersNozzle,
            Value::Unset,
            fixed,
            "stock.nozzle",
            &[],
        )?;

        // Cooling.
        let (fan_off, fan_why, fan_sentence) =
            if level >= 2 && data.level.fan_off_layers > card.fan_off_layers {
                (
                    data.level.fan_off_layers,
                    rule(RuleId::Lvl2, data.level.mark),
                    "fan.off_layers.level2",
                )
            } else {
                (
                    card.fan_off_layers,
                    rule(RuleId::Card, card.fan_off_layers_mark),
                    "fan.off_layers",
                )
            };
        match proven.floor(
            Field::PartFanOffLayers,
            &Value::Whole(fan_off),
            Some(limits.fan_off_max),
        ) {
            Some(floor) => draft.proven(Field::PartFanOffLayers, &floor, "proven.fan_off", date)?,
            None => draft.plain(
                Field::PartFanOffLayers,
                Value::Whole(fan_off),
                fan_why,
                fan_sentence,
                &[("value", P::Whole(fan_off))],
            )?,
        };
        let aux_first = Value::Whole(card.aux_fan_first_layers_pct);
        match proven.floor(Field::AuxFanFirstLayers, &aux_first, None) {
            Some(floor) => draft.proven(Field::AuxFanFirstLayers, &floor, "proven.aux", date)?,
            None => draft.plain(
                Field::AuxFanFirstLayers,
                aux_first,
                rule(RuleId::Card, card.aux_fan_first_layers_mark),
                "fan.aux_first",
                &[("value", P::Whole(card.aux_fan_first_layers_pct))],
            )?,
        };
        let aux_max = card
            .aux_fan_max
            .as_ref()
            .map_or(Value::Unset, |aux| Value::Whole(aux.pct[step]));
        match (
            proven.floor(Field::AuxFanMax, &aux_max, None),
            &card.aux_fan_max,
        ) {
            (Some(floor), _) => draft.proven(Field::AuxFanMax, &floor, "proven.aux", date)?,
            (None, Some(aux)) => draft.plain(
                Field::AuxFanMax,
                Value::Whole(aux.pct[step]),
                rule(RuleId::Card, aux.marks[step]),
                "fan.aux_max",
                &[
                    ("value", P::Whole(aux.pct[step])),
                    card_level[0],
                    card_level[1],
                ],
            )?,
            (None, None) => {
                let why = Why {
                    source: Source::Profile,
                    rule: RuleId::Card,
                    basis: Basis::Stock,
                };
                draft.plain(Field::AuxFanMax, Value::Unset, why, "fan.aux_max.none", &[])?
            }
        };

        // ---- Shell: the intent's table, raised by MAT-1. ----
        let by_intent = Why {
            source: Source::Intent,
            rule: RuleId::Int1,
            basis: row.mark,
        };
        let fallback = Why {
            source: Source::Intent,
            rule: RuleId::Int2,
            basis: balanced.mark,
        };
        let layer = P::Number(process.layer_height_hundredths() as f64 / 100.0);
        draft.plain(
            Field::Process,
            Value::Token(process.as_str()),
            if preset_installed {
                by_intent
            } else {
                fallback
            },
            if preset_installed {
                "shell.process"
            } else {
                "shell.process.fallback"
            },
            &[("intent", intent_word), ("layer", layer)],
        )?;
        let wall_loops = row.wall_loops.max(wall_floor);
        if let Some(floor) = proven.floor(Field::WallLoops, &Value::Whole(wall_loops), None) {
            draft.proven(Field::WallLoops, &floor, "proven.walls", date)?;
        } else if row.wall_loops < wall_floor {
            draft.plain(
                Field::WallLoops,
                Value::Whole(wall_loops),
                rule(RuleId::Mat1, data.walls.mark),
                "shell.walls.thin",
                &[("value", P::Whole(wall_loops))],
            )?;
        } else {
            draft.plain(
                Field::WallLoops,
                Value::Whole(wall_loops),
                by_intent,
                "shell.walls",
                &[("value", P::Whole(wall_loops)), ("intent", intent_word)],
            )?;
        }
        draft.plain(
            Field::InfillPct,
            Value::Whole(row.infill_pct),
            by_intent,
            "shell.infill",
            &[("value", P::Whole(row.infill_pct)), ("intent", intent_word)],
        )?;

        let verified = |field: Field, token: &str| inputs.capabilities.verified(field, token);
        let pattern_ok = row.infill_pattern == balanced.infill_pattern
            || verified(Field::InfillPattern, row.infill_pattern.as_str());
        let pattern = if pattern_ok {
            row.infill_pattern
        } else {
            balanced.infill_pattern
        };
        unavailable |= !pattern_ok;
        draft.set(
            Field::InfillPattern,
            Entry {
                value: Value::Token(pattern.as_str()),
                why: if pattern_ok { by_intent } else { fallback },
                sentence: if pattern_ok {
                    "shell.pattern"
                } else {
                    "shell.fallback"
                },
                filled: &[("intent", intent_word)],
                bounds: Some(choice(
                    InfillPattern::ALL,
                    |candidate| {
                        only_if(
                            candidate == pattern
                                || candidate == balanced.infill_pattern
                                || verified(Field::InfillPattern, candidate.as_str()),
                            UNVERIFIED,
                        )
                    },
                    InfillPattern::as_str,
                )),
            },
        )?;
        let seam_ok = row.seam == balanced.seam || verified(Field::Seam, row.seam.as_str());
        let seam = if seam_ok { row.seam } else { balanced.seam };
        unavailable |= !seam_ok;
        draft.set(
            Field::Seam,
            Entry {
                value: Value::Token(seam.as_str()),
                why: if seam_ok { by_intent } else { fallback },
                sentence: if seam_ok {
                    "shell.seam"
                } else {
                    "shell.fallback"
                },
                filled: &[("intent", intent_word)],
                bounds: Some(choice(
                    Seam::ALL,
                    |candidate| {
                        only_if(
                            candidate == seam
                                || candidate == balanced.seam
                                || verified(Field::Seam, candidate.as_str()),
                            UNVERIFIED,
                        )
                    },
                    Seam::as_str,
                )),
            },
        )?;
        if unavailable {
            warnings.push(cat.notice("intent_value_unavailable", &[("intent", intent_word)])?);
        }

        // ---- Supports (SUP-1 to SUP-4). ----
        let supports = &data.supports;
        let hang = self.overhang(pieces);
        let (on_from, forced_above) = if card.flexible {
            (
                supports.flexible_on_from_mm2,
                supports.flexible_forced_above_mm2,
            )
        } else {
            (supports.on_from_mm2, supports.forced_above_mm2)
        };
        let supports_on = hang.support_mm2 >= on_from;
        // Supports are forced by what is known to need them: the bridges and free overhangs
        // of the classified pieces. A hole ceiling and an unclassified piece switch supports
        // on and leave `off` allowed at every area.
        let forced = supports_on && hang.forcing_mm2 > forced_above;
        // Supports may be switched on wherever something overhangs at all, also when no
        // region counts: a short bridge or a small hole can still print badly.
        let offered = hang.total_mm2 >= supports.offer_from_mm2;
        let (allowed, denied) = match (supports_on, forced, offered) {
            (true, true, _) => (
                vec![Value::Flag(true)],
                vec![Denied {
                    value: Value::Flag(false),
                    rule: RuleId::Sup1,
                    detail: "not_allowed.supports_required",
                }],
            ),
            (true, false, _) => (vec![Value::Flag(true), Value::Flag(false)], Vec::new()),
            (false, _, true) => (vec![Value::Flag(false), Value::Flag(true)], Vec::new()),
            (false, _, false) => (
                vec![Value::Flag(false)],
                vec![Denied {
                    value: Value::Flag(true),
                    rule: RuleId::Sup1,
                    detail: "not_allowed.supports_nothing",
                }],
            ),
        };
        let area = P::Whole(
            if supports_on {
                hang.support_mm2
            } else {
                hang.total_mm2
            }
            .round() as i64,
        );
        let (support_sentence, support_filled): (&str, [(&'static str, P<'_>); 2]) =
            match (supports_on, hang.classified, offered) {
                (true, true, _) => (
                    if hang.regions == 1 {
                        "supports.on.one"
                    } else {
                        "supports.on"
                    },
                    [
                        ("count", P::Whole(hang.regions as i64)),
                        ("largest", P::Whole(hang.largest_mm2.round() as i64)),
                    ],
                ),
                (true, false, _) => (
                    "supports.on.unclassified",
                    [("area", area), ("count", P::Whole(0))],
                ),
                (false, _, true) => (
                    "supports.off.small",
                    [("area", area), ("count", P::Whole(0))],
                ),
                (false, _, false) => (
                    "supports.off.none",
                    [("area", area), ("count", P::Whole(0))],
                ),
            };
        draft.set(
            Field::SupportsEnabled,
            Entry {
                value: Value::Flag(supports_on),
                why: rule(RuleId::Sup1, supports.mark),
                sentence: support_sentence,
                filled: &support_filled,
                bounds: Some(Bounds::set(allowed, denied)),
            },
        )?;
        let normal_ok = verified(Field::SupportsKind, SupportKind::Normal.as_str());
        let support_kind = if hang.fit_overhang && normal_ok {
            SupportKind::Normal
        } else {
            SupportKind::Tree
        };
        draft.set(
            Field::SupportsKind,
            Entry {
                value: Value::Token(support_kind.as_str()),
                why: rule(
                    RuleId::Sup4,
                    if support_kind == SupportKind::Tree {
                        Basis::Stock
                    } else {
                        Basis::Judgment
                    },
                ),
                sentence: if support_kind == SupportKind::Tree {
                    "supports.kind.tree"
                } else {
                    "supports.kind.normal"
                },
                filled: &[],
                bounds: Some(choice(
                    SupportKind::ALL,
                    |candidate| only_if(candidate == SupportKind::Tree || normal_ok, UNVERIFIED),
                    SupportKind::as_str,
                )),
            },
        )?;
        draft.plain(
            Field::SupportsThreshold,
            Value::Whole(supports.threshold_deg),
            rule(RuleId::Sup2, Basis::Stock),
            "supports.threshold",
            &[("value", P::Whole(supports.threshold_deg))],
        )?;
        let place = if hang.over_part {
            SupportWhere::Everywhere
        } else {
            SupportWhere::BedOnly
        };
        draft.set(
            Field::SupportsWhere,
            Entry {
                value: Value::Token(place.as_str()),
                why: rule(RuleId::Sup3, supports.mark),
                sentence: if hang.over_part {
                    "supports.where.everywhere"
                } else {
                    "supports.where.bed_only"
                },
                filled: &[],
                bounds: Some(choice(
                    SupportWhere::ALL,
                    |candidate| {
                        only_if(
                            candidate == SupportWhere::Everywhere || !hang.over_part_forced,
                            (RuleId::Sup3, "not_allowed.supports_over_part"),
                        )
                    },
                    SupportWhere::as_str,
                )),
            },
        )?;

        // ---- Motion caps. SPD-1: a cap can only lower a speed, and the lowest cap wins. ----
        let gentle = ask.very_slender;
        let none = Why {
            source: Source::Profile,
            rule: RuleId::Spd1,
            basis: Basis::Stock,
        };
        let gentle_why = rule(RuleId::Spd2, data.gentle.mark);
        let outer_caps = [
            card.outer_wall_cap_mm_s
                .zip(card.outer_wall_cap_mark)
                .map(|(cap, mark)| (cap, rule(RuleId::Card, mark), "motion.outer_wall.card")),
            gentle.then_some((
                data.gentle.outer_wall_speed_cap_mm_s,
                gentle_why,
                "motion.gentle",
            )),
            row.outer_wall_speed_cap_mm_s
                .map(|cap| (cap, by_intent, "motion.outer_wall.intent")),
        ];
        let outer_cap = outer_caps
            .into_iter()
            .flatten()
            .min_by_key(|(cap, _, _)| *cap);
        // The cap as the plan has it: a change may have lowered it.
        let outer_cap = match outer_cap {
            Some((cap, why, sentence)) => draft.plain(
                Field::OuterWallSpeedCap,
                Value::Whole(cap),
                why,
                sentence,
                &[
                    ("value", P::Whole(cap)),
                    ("intent", intent_word),
                    ("family", family_word),
                ],
            )?,
            None => draft.plain(
                Field::OuterWallSpeedCap,
                Value::Unset,
                none,
                "motion.none",
                &[],
            )?,
        }
        .whole();
        draft.plain(Field::SpeedCap, Value::Unset, none, "motion.none", &[])?;
        if gentle {
            let cap = data.gentle.accel_cap_mm_s2;
            draft.plain(
                Field::AccelCap,
                Value::Whole(cap),
                gentle_why,
                "motion.gentle",
                &[("value", P::Whole(cap))],
            )?;
        } else {
            draft.plain(Field::AccelCap, Value::Unset, none, "motion.none", &[])?;
        }
        let outer_accels = [
            gentle.then_some((
                data.gentle.outer_wall_accel_cap_mm_s2,
                gentle_why,
                "motion.gentle",
            )),
            row.outer_wall_accel_cap_mm_s2
                .map(|cap| (cap, by_intent, "motion.outer_accel.intent")),
        ];
        match outer_accels
            .into_iter()
            .flatten()
            .min_by_key(|(cap, _, _)| *cap)
        {
            Some((cap, why, sentence)) => draft.plain(
                Field::OuterWallAccelCap,
                Value::Whole(cap),
                why,
                sentence,
                &[("value", P::Whole(cap)), ("intent", intent_word)],
            )?,
            None => draft.plain(
                Field::OuterWallAccelCap,
                Value::Unset,
                none,
                "motion.none",
                &[],
            )?,
        };
        match card.grade(shore) {
            Some(grade) => draft.plain(
                Field::MaxFlowCap,
                Value::decimal(grade.max_flow_cap_mm3_s),
                rule(RuleId::Card, grade.mark),
                "motion.flow.card",
                &[("value", P::Number(grade.max_flow_cap_mm3_s))],
            )?,
            None => draft.plain(Field::MaxFlowCap, Value::Unset, none, "motion.none", &[])?,
        };
        draft.plain(Field::JerkCap, Value::Unset, fixed, "motion.jerk", &[])?;
        draft.plain(
            Field::PrimeTower,
            Value::Flag(false),
            rule(RuleId::Pt1, Basis::Measured),
            "prime_tower.off",
            &[],
        )?;

        // ---- Warnings and notes. ----
        if on.status == PlateStatus::Warning {
            warnings.push(cat.notice(
                "plate_not_recommended",
                &[("family", family_word), ("plate", plate_word)],
            )?);
        }
        if ask.extreme {
            warnings.push(cat.notice(
                "turn_or_split",
                &[("limit", P::Number(data.brim.slender.warn_above))],
            )?);
        }
        let crowded = card.flexible && pieces.len() > data.flexible.max_pieces;
        if crowded {
            let filled = [
                ("count", P::Whole(pieces.len() as i64)),
                ("max", P::Whole(data.flexible.max_pieces as i64)),
            ];
            warnings.push(cat.notice("too_many_flexible_pieces", &filled)?);
        }
        if !card.proven {
            warnings.push(cat.notice("settings_not_proven", &[("family", family_word)])?);
        }
        // The product is not printed the way its gauge was.
        if let Some(parts) = calibration {
            let tolerance = data.signature.bed_tolerance_c;
            signature_differs |= parts.process != process
                || (parts.layer_height_mm * 100.0).round() as i64
                    != process.layer_height_hundredths()
                || parts.outer_wall_cap_mm_s != outer_cap
                || (parts.first_layer_bed_c - bed_pair[0]).abs() > tolerance
                || (parts.layers_bed_c - bed_pair[1]).abs() > tolerance;
        }
        if signature_differs {
            warnings.push(cat.notice("fit_signature_differs", &[])?);
        }
        notes.push(cat.notice("chamber_fan", &[])?);
        if level >= 3 {
            notes.push(cat.notice("level_3", &[])?);
        }
        if card.flexible {
            notes.push(cat.notice("dedicated_hotend", &[])?);
        }
        if baseline.is_some() {
            notes.push(cat.notice("proven_baseline", &[("date", date)])?);
        }
        if draft.proven_limited {
            warnings.push(cat.notice("proven_limited", &[("date", date)])?);
        }

        // ---- The list. ----
        let mut list = ListBuilder::new(cat);
        list.add(&format!("plate.{plate}"), &[], None)?;
        if level >= 3 {
            list.add("plate.check", &[], None)?;
            list.add("run_calibration", &[], None)?;
        }
        if on.status == PlateStatus::Warning {
            list.add("plate.not_recommended", &[("family", family_word)], None)?;
        }
        match bed.glue {
            Glue::Required => list.add("glue.required", &[], None)?,
            // Said only where a person would otherwise reach for glue.
            Glue::No if card.flexible => list.add("glue.none", &[], None)?,
            // Level 3 names every aid at the printer: glue is allowed for a rigid filament
            // on every plate (the vendor's material table), needed or not.
            Glue::No | Glue::Unstated if level >= 3 && !card.flexible => {
                list.add("glue.allowed", &[], None)?;
            }
            Glue::No | Glue::Unstated => {}
        }
        let colour_word = P::Word(cat.colour_word(spool.colour)?);
        match spool.slot {
            SpoolSlot::Ams(number) => list.add(
                "spool.ams",
                &[
                    ("colour", colour_word),
                    ("family", family_word),
                    ("slot", P::Whole(i64::from(number))),
                ],
                None,
            )?,
            _ => list.add(
                "spool.external",
                &[("colour", colour_word), ("family", family_word)],
                None,
            )?,
        }
        // The lines of the card and of the hardness grade. A plan carries one line about
        // what to do afterwards: where a brim is drawn, the brim line below replaces the
        // card's, and says to let the bed cool as well.
        let grade_list = card.grade(shore).map_or(&[][..], |grade| &grade.list[..]);
        for id in card.list.iter().chain(grade_list) {
            if !(drawn && id.starts_with("after.")) {
                list.add(id, &[], None)?;
            }
        }
        list.add("open_sliced", &[], None)?;
        list.add("project", &[], Some(Condition::ProjectDelivered))?;
        list.add("no_sync", &[], None)?;
        list.add("bed_levelling", &[], None)?;
        if gentle || intent == Intent::BestSurface {
            list.add("calibration", &[], None)?;
        }
        // VNT-1: the line is carried with its condition; the export knows the print time.
        if card
            .vent_mark_c
            .is_some_and(|mark| bed_pair[0] > mark || bed_pair[1] > mark)
        {
            list.add(
                "vent",
                &[],
                Some(Condition::PrintLongerThanMinutes(
                    data.venting.longer_than_minutes,
                )),
            )?;
        }
        if drawn {
            let lines = (drawn_width as f64 / line_width_mm).round() as i64;
            list.add("watch_brim", &[("lines", P::Whole(lines))], None)?;
            // FIT-3. A flexible brim does not snap off; its line says how it comes off.
            list.add(
                if card.flexible {
                    "after.flexible_brim"
                } else if fit_plate {
                    "after.brim_fit"
                } else {
                    "after.brim"
                },
                &[],
                None,
            )?;
        }
        if crowded {
            list.add("few_pieces", &[], None)?;
        }
        list.add("outcome", &[], None)?;
        let base_limit = if card.flexible {
            checklist::LIMIT_FLEXIBLE
        } else {
            checklist::LIMIT
        };
        let checklist = list.finish(if level >= 3 {
            base_limit + checklist::LEVEL_3_EXTRA
        } else {
            base_limit
        });

        // ---- The settings. Every value passes its hard range once more here. ----
        let mut proven_fields = std::mem::take(&mut draft.proven_fields);
        proven_fields.sort_by_key(|field| field.index());
        proven_fields.dedup();
        let decisions = draft.finish()?;
        let filament = FilamentSetting {
            spool: spool.slot,
            nozzle_id: feed.nozzle_id,
            colour: spool.colour,
            material_id: inputs.material.clone(),
            family,
        };
        let settings = PrintSettings::from_values(
            decisions.iter().map(|decision| decision.value).collect(),
            filament,
        )?;

        let mut fired: Vec<RuleId> = ask.fired.iter().copied().collect();
        if card.flexible {
            fired.push(RuleId::Ad6);
        }
        let derived = Derived {
            fired,
            rule_level: outcome.rule_level,
            asked_brim_width_mm: asked_width,
            brim_required,
            brim_width_max_mm: brim_width_max,
            warp_class: card.warp_class,
            fit_plate,
            bed_window_c: beds.window,
            nozzle_range_c: card.nozzle_range_c,
            wall_loops_floor: plan_wall_floor,
            support_mm2: hang.support_mm2,
            support_regions: hang.classified.then_some(hang.regions),
            support_largest_mm2: hang.largest_mm2,
            gentle_motion: gentle,
            calibrated: calibrated.is_some(),
            elephant_foot_mm: foot_mm,
            proven_on: baseline.map(Baseline::printed_on),
            proven_fields,
        };

        Ok(PrintPlan {
            settings: Some(settings),
            decisions,
            facts: Some(facts.clone()),
            derived: Some(derived),
            questions: Vec::new(),
            warnings,
            notes,
            blocked: None,
            checklist,
            revisions: self.revisions(),
        })
    }

    /// The bed pair `[first layer, other layers]` with the reason for each value. The caller
    /// has refused a limit below the card's window.
    fn bed(&self, bed: &Bed, ask: BedAsk<'_>) -> Beds {
        let BedAsk {
            step,
            fit_plate,
            calibration,
            level_by,
            limit_c,
        } = ask;
        // PLT-2: never above what the printer and the filament allow, and never below the
        // card's window.
        let window = [bed.window[0], bed.window[1].min(limit_c).max(bed.window[0])];
        let into_window = |value: i64| value.clamp(window[0], window[1]);
        // FIT-5: with mating faces on the bed, level 1 heats the first layer only.
        let (by_level, mark, other_fit) = match step {
            0 => (bed.normal, bed.normal_mark, false),
            1 if fit_plate => ([bed.raised[0], bed.normal[1]], bed.raised_mark, true),
            _ => (bed.raised, bed.raised_mark, false),
        };
        let card_first = (rule(RuleId::Card, mark), "bed.first.card");
        let card_other = if other_fit {
            (rule(RuleId::Fit5, Basis::Judgment), "bed.other.fit")
        } else {
            (rule(RuleId::Card, mark), "bed.other.card")
        };

        let mut pair = by_level;
        let mut first = card_first;
        let mut other = card_other;
        let mut differs = false;
        if let (Some(parts), true) = (calibration, fit_plate) {
            // The product takes the bed pair of its gauge, and never a colder one than the
            // card gives the plate's own level, whatever raised that level: a gauge printed
            // at a lower level or on another plate must not cool the bed under a piece that
            // needs it hot. The fit is then not printed as calibrated, and the plan says so.
            let recorded = [
                into_window(parts.first_layer_bed_c),
                into_window(parts.layers_bed_c),
            ];
            differs |= recorded != [parts.first_layer_bed_c, parts.layers_bed_c];
            let gauge = Why {
                source: Source::Calibration,
                rule: RuleId::Fit5,
                basis: Basis::Measured,
            };
            let raised = Why {
                source: level_by,
                rule: RuleId::Fit5,
                basis: Basis::Judgment,
            };
            for index in 0..2 {
                let reason = if by_level[index] > recorded[index] {
                    differs = true;
                    pair[index] = by_level[index];
                    (raised, "bed.raised_over_calibration")
                } else {
                    pair[index] = recorded[index];
                    (gauge, "bed.calibration")
                };
                if index == 0 {
                    first = reason;
                } else {
                    other = reason;
                }
            }
        }
        let wanted = pair;
        pair = [into_window(pair[0]), into_window(pair[1])];
        pair[1] = pair[1].min(pair[0]);
        let limited = bed.window[1] > limit_c && (wanted[0] > pair[0] || wanted[1] > pair[1]);
        if limited {
            let held = (rule(RuleId::Plt2, Basis::Judgment), "bed.limited");
            if wanted[0] > pair[0] {
                first = held;
            }
            if wanted[1] > pair[1] {
                other = held;
            }
        }
        Beds {
            pair,
            window,
            first,
            other,
            differs,
            limited,
        }
    }

    /// The fit signature the sliced file of this plan must state: the plan's values, with
    /// the baseline for the fields the plan leaves to the installed profile. `None` for a
    /// blocked plan.
    pub fn expected_signature(&self, plan: &PrintPlan) -> Option<FitSignature> {
        let settings = plan.settings.as_ref()?;
        let filament = settings.filaments().first()?;
        let process = settings.process()?;
        let baseline = &self.data.baseline;
        let decimal = |field: Field, stock: f64| settings.get(field).number().unwrap_or(stock);
        FitSignature::new(SignatureParts {
            process,
            layer_height_mm: process.layer_height_hundredths() as f64 / 100.0,
            first_layer_height_mm: decimal(Field::FirstLayerHeight, baseline.first_layer_height_mm),
            first_layer_line_width_mm: decimal(
                Field::FirstLayerLineWidth,
                baseline.first_layer_line_width_mm,
            ),
            elephant_foot_mm: decimal(Field::ElephantFoot, baseline.elephant_foot_mm),
            outer_wall_cap_mm_s: settings.get(Field::OuterWallSpeedCap).whole(),
            first_layer_bed_c: settings.get(Field::FirstLayerBed).whole()?,
            layers_bed_c: settings.get(Field::LayersBed).whole()?,
            brim_gap_mm: settings.get(Field::BrimGap).number()?,
            material_id: filament.material_id.clone(),
            nozzle_id: filament.nozzle_id,
        })
        .ok()
    }
}

/// What the bed temperatures depend on.
struct BedAsk<'a> {
    /// The level, 0 to 2.
    step: usize,
    /// Whether mating faces stand on the bed.
    fit_plate: bool,
    /// The gauge's signature, when one is recorded.
    calibration: Option<&'a SignatureParts>,
    /// Who set the level: the rules, the history or a request.
    level_by: Source,
    /// The highest bed temperature the printer and the filament allow.
    limit_c: i64,
}

/// The bed temperatures of a plan.
struct Beds {
    pair: [i64; 2],
    window: [i64; 2],
    first: (Why, &'static str),
    other: (Why, &'static str),
    /// The pair is not the gauge's although a gauge is recorded.
    differs: bool,
    /// The printer's or the filament's limit lowered a value.
    limited: bool,
}
