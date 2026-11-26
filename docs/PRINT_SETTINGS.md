# Print settings

How Hotend chooses the print settings of a plate, for people who build on the crate
`crates/hotend-settings`. The crate is pure (no I/O beyond its own embedded data files, no
async, no dependency on another Hotend crate) and knows no slicer: a *field* is a setting of
Hotend, and only `hotend-slicer` knows which profile key carries it.

## Model

Source: `src/model.rs`, `src/bounds.rs`, `src/schema.rs`, `src/facts.rs`.

### Fields

The list of fields is closed: `model::Field`, 39 values, in the order below. That order is
also the order in which a plan is computed, serialized and, later, changed.

"Who": **P** the person is asked. **R** a rule decides. **A** the agent or the person may move
the value inside its envelope. **H** print history moves the rule's result. **C** locked to the
fit calibration when one exists. **F** fixed in version 1. "opt" means the value may be unset:
"as the installed profile has it", nothing is written for the field.

| Field | Value | Hard range | Who |
| --- | --- | --- | --- |
| `setup.plate_type` | token | `cool`, `textured-pei`, `high-temp`, `engineering`, `cool-supertack` | P |
| `filament.spool` | spool slot | `1` to `16`, `external_main`, `external_auxiliary` | R, P |
| `filament.nozzle_id` | whole | 0 to 1 (device extruder id) | R |
| `filament.colour` | colour | `#RRGGBB` | P |
| `intent` | token | `balanced`, `strong`, `best_surface`, `fast`, `fit_critical` | A |
| `adhesion.level` | whole, step | 0 to 3 | R, H, A |
| `adhesion.brim_kind` | token | `none`, `outer`, `inner`, `outer_and_inner` | R, A |
| `adhesion.brim_width_mm` | whole, mm | 0 to 15 | R, H, A |
| `adhesion.brim_gap_mm` | decimal, mm | 0 to 0.3 | R |
| `adhesion.skirt_loops` | whole | 0 | F |
| `adhesion.elephant_foot_mm` | decimal, mm, opt | 0 to 0.3 | C |
| `first_layer.height_mm` | decimal, mm, opt | 0.16 to 0.28 | C, R |
| `first_layer.line_width_mm` | decimal, mm, opt | 0.42 to 0.60 | C |
| `first_layer.wall_speed_mm_s` | whole, mm/s | 10 to 50 | R, A |
| `first_layer.infill_speed_mm_s` | whole, mm/s | 15 to 105 | R, A |
| `first_layer.accel_mm_s2` | whole, mm/s2, cap, opt | 250 to 500 | A |
| `first_layer.bed_c` | whole, C | 30 to 120; a plan narrows it to the card's window | R, H, A |
| `layers.bed_c` | whole, C | 30 to 120; never above `first_layer.bed_c` | R |
| `first_layer.nozzle_c`, `layers.nozzle_c` | whole, C, opt | 170 to 300; never written in version 1 | F |
| `cooling.part_fan_off_layers` | whole, layers | 1 to 10 | R, A |
| `cooling.aux_fan_first_layers_pct` | whole, percent | 0 to 40 | R |
| `cooling.aux_fan_max_pct` | whole, percent, cap, opt | 0 to 100 | R, A |
| `shell.process` | token | `standard-0.20mm`, `fine-0.12mm`, `draft-0.24mm` | the intent only |
| `shell.wall_loops` | whole | 2 to 6 | A |
| `shell.infill_pct` | whole, percent | 5 to 50 | A |
| `shell.infill_pattern` | token | `grid`, `gyroid` | A |
| `shell.seam` | token | `aligned`, `back` | A |
| `supports.enabled` | flag | on, off | R, A |
| `supports.kind` | token | `tree`, `normal` | R, A |
| `supports.threshold_deg` | whole, degrees | 20 to 45 | A |
| `supports.where` | token | `everywhere`, `bed_only` | R, A |
| `motion.outer_wall_speed_cap_mm_s` | whole, mm/s, cap, opt | 20 to 200 | A |
| `motion.speed_cap_mm_s` | whole, mm/s, cap, opt | 20 to 300 | A |
| `motion.accel_cap_mm_s2` | whole, mm/s2, cap, opt | 500 to 10000 | A |
| `motion.outer_wall_accel_cap_mm_s2` | whole, mm/s2, cap, opt | 500 to 5000 | A |
| `motion.max_flow_cap_mm3_s` | decimal, mm3/s, cap, opt | 1 to 25 | A |
| `motion.jerk_cap_mm_s` | whole, mm/s, cap, opt | 1 to 9; reserved | F |
| `prime_tower.enabled` | flag | on, off | R |

The fields from `adhesion.level` to `cooling.aux_fan_max_pct` are the *sticking group*
(`bounds::sticking_fields()`): no choice of the agent, the person or an intent may weaken one
of them.

### Values

`model::Value` is `Unset`, `Whole(i64)`, `Decimal(f64)` (two places), `Token(&'static str)`,
`Flag(bool)`, `Colour` or `Slot`. A token is always one written in this crate; no value type
can hold text from a person, a model or a file. In JSON a value is `null`, a number, a string
or `true`/`false`. The only identifier that is not a token of the crate is `MaterialId`
(lower-case letters, digits and dashes, 40 characters at most).

### Hard ranges

Every hard range is a constant in `bounds.rs` and nowhere else. None is read from a profile, a
data file or an input. `bounds::check(field, &value)` is the one test "this value may exist for
this field": the settings are built through it, the proposal schema is generated from the same
table. `bounds::spec(field)` gives unit, kind, group and "who".

**Echoed values.** The slicer side judges every value a sliced file states for a mapped key,
whether Hotend wrote the key or not, with `PrintPlan::check_echo(field, &value)`, not with
`bounds::check` alone. `check_echo` is `bounds::check` plus the two ranges only a plan knows:
both nozzle temperatures must lie in the family's range (`derived.nozzle_range_c`, PLA 190 to
240), and both bed temperatures in the card's window for the plate, cut at the printer's and
the filament's limit (`derived.bed_window_c`). An echoed PLA nozzle of 290 C is inside the
constant 170 to 300 C and is refused by `check_echo`. An echo always states a value, so `Unset`
is refused. `PrintPlan::echo_range(field)` gives the range as numbers.

The bed range 30 to 120 C is the span of all material cards, up to the printer's own limit
(`bed_max_c` of `crates/hotend-printer/profiles/bambu.toml`). Inside it every family has a
constant range of its own, `bounds::bed_range_c(family)` (PLA 35 to 65, PETG 60 to 80, TPU 30
to 45, ABS and ASA 90 to 100: the outermost values of the vendor's material table), and a card
whose window leaves it fails to load. The nozzle range 170 to 300 C likewise contains the
per-family ranges of the cards.

**Floors of the data.** `bounds::floors` holds the constants that the rule data and the cards
cannot go below: a required brim has gap 0 and needs an elephant foot compensation of at least
0.1 mm; a brim rule of a rigid material draws at least 3 mm (a flexible one 1 mm) and level 2
at least 3 mm, widening by at least 1 mm; AD-1 fires at least under 2 mm, AD-2 at least under
200 mm2, AD-3 at the latest from a ratio of 3, AD-4 at the latest from 250 mm (warp class low)
and 180 mm (medium); the part fan waits at least 3 layers; level 2 never lowers the first layer
under 0.20 mm; thin walls get at least 3 loops. A data file that asks for less fails to load,
so what guards adhesion and the fit does not rest on a data file alone.

### `PrintSettings`

One value per field, plus the list of filaments of the plate (one entry in version 1: spool,
nozzle id, colour, material id, family). It has no public constructor and no `Deserialize`: the
only way to obtain one is a plan computed by this crate, and building one checks every value
against its hard range. `get(field)` and `iter()` read it. Serialized form:

```json
{"schema": 1,
 "filaments": [{"spool": "1", "nozzle_id": 1, "colour": "#898989", "material_id": "pla-generic", "family": "PLA"}],
 "fields": {"setup.plate_type": "textured-pei", "...": "...", "prime_tower.enabled": false}}
```

The stored plan is a record. It is never read back: the core recomputes the plan from its
stored inputs and compares the hash.

### `Decision` and `PrintPlan`

`PrintPlan` is `{settings, decisions, facts, derived, questions, warnings, notes, blocked,
checklist, revisions}`. A blocked plan has `settings: null` and no decisions; `blocked` is
`{code, sentence}` and says what the person must do.

`Decision` is `{field, value, unit, source, rule, basis, sentence_id, sentence, envelope,
asked, scope, rule_value, rule_source, rule_rule, answer}`:

- `source`: `rule`, `intent`, `agent`, `person`, `history`, `calibration`, `proven` (the
  proven baseline, see "Proven baselines") or `profile` (the field is unset);
- `rule`: a stable rule id such as `AD-1` (`model::RuleId`);
- `basis`: `measured`, `sourced`, `judgment` or `stock`;
- `sentence`: a sentence of the catalogue with the plan's numbers filled in, never free text;
- `envelope`: `{"range": {min, max, step}}`, `{"set": {allowed}}` or `{"locked": {rule}}`;
- `asked`: what the agent or the person asked for, when that differs from `value`;
- `scope`: `plate`;
- `rule_value`, `rule_source`, `rule_rule`: what the rules alone gave, and which rule set it.
  They equal `value`, `source` and `rule` in a plan without accepted changes. A change is
  judged against `rule_value`, never against an earlier change;
- `answer`: `null`, or `{by, answer, rule, detail}` when somebody asked for a value of this
  field (see "Envelopes and proposals").

### Plate facts

`facts::PlateFacts` is the checked form of what the engine measures per piece
(`contact_mm2`, `contact_perimeter_mm`, `strip_width_mm`, `contact_span_mm`, `height_mm`,
`slenderness`, `overhang_mm2`, `fit_overhang_mm2`, `overhang_over_part_mm2`,
`overhang_regions`, `inner_loops`, `fit_at_bed`, `min_wall_mm`). The engine's object for a plate
is:

```json
{"family": "PLA", "piece_count": 1, "total_contact_mm2": 456.0,
 "pieces": [{"contact_mm2": 456.0, "contact_perimeter_mm": 452.0, "strip_width_mm": 2.0,
             "contact_span_mm": 74.0, "height_mm": 6.0, "slenderness": 0.28,
             "overhang_mm2": 0.0, "fit_overhang_mm2": 0.0, "overhang_over_part_mm2": 0.0,
             "overhang_regions": [], "inner_loops": [{"min_width_mm": 70.0, "fit": true}],
             "fit_at_bed": [{"pair": "ring", "side": "outside"}], "min_wall_mm": null}]}
```

`PlateFacts::from_json` reads the engine's text straight into the strict types, and
`from_value` an already parsed value. Both refuse (`FactsError`, the plan is then blocked with
`facts_invalid`): an unknown or missing key (`overhang_regions` and `min_wall_mm` must be
present and may be `null`), a value of the wrong type, a number that is not finite, negative or
above its bound (areas 1e6, perimeter 1e5, lengths 1000, slenderness 1e4), more than 64 regions,
inner loops or fit entries, more than 256 pieces, region areas that do not sum to
`overhang_mm2` within 1 percent, a stated count or total that disagrees with the pieces, and a
fit pair name outside `^[a-z][a-z0-9_.-]{0,63}$`. An overhang region is
`{area_mm2, kind, span_mm, diameter_mm, reach_mm, over_part}` with `kind` one of `bridge`,
`hole_ceiling`, `free`; the length of the region's own kind (span, diameter, reach) is required
and the other two may be absent or `null`. The error names the fact and the kind of problem in
Hotend's words and repeats no text of the engine.

Only the text shows some faults, so facts that arrive as text go through `from_json`: a key
that is stated twice is refused (two readers of one text must not read two plates), a text
longer than `MAX_FACTS_BYTES` (8 MiB; a plate with every list at its bound is about half of
that) is refused before it is parsed, and a list stops being read at its bound, so what stands
behind the 257th piece or the 65th entry is never parsed or stored.

The facts of a piece are held against each other, and what no shape can have is refused as
`inconsistent` (with 1 percent and 0.01 of slack for the engine's rounding):

| Fact | Refused when |
| --- | --- |
| `contact_perimeter_mm` | shorter than the circle of the contact's area, `2 x sqrt(pi x contact)` |
| `strip_width_mm` | wider than that circle's radius, `sqrt(contact / pi)` |
| `contact_span_mm` | shorter than `sqrt(contact)`: a contact fits into the square on its longest side |
| `height_mm` | 0 on a piece with contact |
| `fit_overhang_mm2`, `overhang_over_part_mm2` | larger than `overhang_mm2` |

Inside those limits the rules read the careful side of two facts that can be cross-checked:
`thin_strip_mm()` is the smaller of `strip_width_mm` and `2 x contact / perimeter` of the piece,
and `tipping()` the larger of `slenderness` and `height / sqrt(contact)`, 1e4 at most, however
small the stated contact is (a piece that stands on next to nothing is as slender as a piece
can be). One wrong number can therefore ask for more adhesion than needed, and two wrong
numbers cannot excuse each other.

### The proposal schema

`schema::proposal_schema()` returns the JSON Schema (draft 2020-12) of
`{plate?, changes: [{field, value, reason}], summary}`: `field` is an enum of the A fields, a
`oneOf` branch per field gives `value` the field's type and hard range or its token list,
`reason` is 1 to 240 characters and `summary` at most 300, both without control characters, at
most 16 changes, and unknown properties are refused at every level. The schema cannot express
"no field twice" or "the reason shows something"; the proposal step checks both. A snapshot
test pins the generated schema (`tests/snapshots/proposal-schema.json`; update with
`HOTEND_UPDATE_SNAPSHOTS=1` and review the diff), and `tests/schema.rs` runs the schema
through a JSON Schema validator (the `jsonschema` crate, a dev-dependency) beside the proposal
step: over the examples and over both ends of every adjustable field's hard range, what the
schema accepts the step reads, and what it refuses the step refuses whole. The one intended
difference: a field without A is not in the schema, and the step answers it by itself with
`not_settable` so that the other changes of the call go on.

## Rules and levels

### Computing a plan

```rust
let book = hotend_settings::RuleBook::builtin()?;   // the four data files, parsed once
let plan = book.plan(&inputs)?;                      // PlanInputs -> PrintPlan
```

`plan` is a pure function: the same `PlanInputs` give the same plan, byte for byte
(`PrintPlan::sha256()` hashes the canonical JSON: sorted keys, no spaces). It reads no file
besides the crate's own embedded data, no clock and no network. An `Err(PlanError)` means that
a data file and the code disagree (a sentence without its text, a value outside its hard
range); it is a defect of Hotend, never an answer to the person.

`PlanInputs` (`inputs.rs`) holds everything a plan depends on:

| Input | What it is |
| --- | --- |
| `facts` | the checked plate facts of the engine (`PlateFacts`, see "Plate facts") |
| `material` | the material id of the plate; its family is the family of the facts |
| `setup` | the person's answers: the plate on the bed and the declared spools (slot, material, colour, silk) |
| `spool_choice` | the person's choice when several spools hold the material |
| `printer` | what the core derived from the printer data: nozzle diameter, the nozzle's highest layer, the lower of the printer's and the filament's highest bed temperature, and for each slot which nozzle it feeds |
| `history` | `min_level` (a floor for the level) and the proven level, for this printer, plate and material |
| `calibration` | the fit signature of the newest gauge for this printer, material and nozzle |
| `gauge` | whether the plate is a fit gauge |
| `intent`, `level` | what the agent or the person asked for, each with who asked |
| `changes` | the accepted changes to the other adjustable fields, the newest per field (see "Envelopes and proposals") |
| `capabilities` | which process presets are installed and which optional tokens the slicer side has verified |
| `baseline` | the proven baseline of this printer, plate and material, when the person recorded one (see "Proven baselines"); ignored when its plate or material is not the plate's own |

Every input type is closed: tokens, bounded numbers, ids with a fixed pattern. A constructor
refuses anything else with an `InputError`.

### A plan that is blocked

When a plate cannot be planned, `plan` returns a plan with `blocked: {code, sentence}`, no
settings, no decisions and an empty list. Where an answer of the person helps, `questions`
holds the question.

| Code | When | Question |
| --- | --- | --- |
| `bed_type_not_chosen` | no plate type is stored | `question.plate_type` |
| `plate_not_supported` | the plate has no card (Cool Plate SuperTack) | `question.plate_type` |
| `spools_not_declared` | no spool, or none with the plate's material | `question.spools` |
| `spool_not_chosen` | several spools hold the material and none is chosen | `question.spool_choice` |
| `plate_material_conflict` | the card refuses the material on this plate (PLT-1) | |
| `material_not_printable` | a flexible filament of 85A or softer with a nozzle of 0.4 mm or less (MAT-2) | |
| `material_not_allowed` | a flexible filament that is not on the outside holder of the main nozzle (MAT-2) | |
| `nozzle_choice_unproven` | the spool feeds the auxiliary nozzle, or its slot is unknown to the printer data (NOZ-1) | |
| `nozzle_size_unproven` | the baseline first layer (0.2 mm high, 0.5 mm wide) is not one the fitted nozzle prints | |
| `bed_limit_below_window` | the printer's or the filament's highest bed temperature lies below the low end of the card's window for this plate: no bed temperature is both inside the window and allowed | |
| `facts_invalid` | the engine's facts were refused; the caller asks `RuleBook::refused_facts()` for this plan | |

**Codes that need their own mapping.** Three codes of this crate need a sentence and a mapping
of their own wherever a plan is shown (protocol, window texts); the sentences are in the
catalogue:

- `nozzle_size_unproven` (blocks a plan): every measurement of the sticking baseline was taken
  with a 0.4 mm nozzle. A plate for any other nozzle is refused until the baseline is measured
  for it. This is stricter than NOZ-1 alone, on purpose: a first layer that the nozzle
  cannot print is not planned.
- `bed_limit_below_window` (blocks a plan): see the table. Planning such a plate at the limit
  with a warning would put ABS on a bed of 65 C.
- `plan_blocked` (answers a proposal): a blocked plan has no envelopes to judge a change
  against.

`kept_by_person` is a fourth: it is not a problem code but an answer to a proposed change, see
"Answering a proposal".

### Order of the rules

1. The refusals above.
2. **Brim rules** over every piece; the plate takes the widest width asked for.
3. **Level** from the brim rules, the spool, the history and a request.
4. **Calibration**: which recorded gauge values apply.
5. **Sticking group**: brim, the first layer, bed temperatures, early cooling. Nothing in this
   group reads the intent. A proven baseline is the floor of each of these fields (and of the
   level in step 3 and the wall loops in step 6).
6. **Shell** from the intent's row, raised by the rules.
7. **Supports**, **motion caps**, prime tower.
8. Warnings, notes and the before-you-print list.
9. Every value is checked against its hard range once more when the settings are built.

Every decision names the rule that set it. The ids are stable:

| Id | Rule |
| --- | --- |
| `AD-0` | no brim rule fired: no brim |
| `AD-1` | thin strip: the narrowest strip on the bed is under 3 mm: outer brim 5 mm |
| `AD-2` | small foot: contact under 300 mm2: 3 mm; under 100 mm2: 5 mm |
| `AD-3` | tall and slender: tipping ratio above 2: 5 mm; above 4: 8 mm, level 2 and gentle motion; above 6: also the warning `turn_or_split` |
| `AD-4` | long part: contact span above 180 mm (warp class low) or 120 mm (medium): 3 mm or 5 mm |
| `AD-5` | warping material (warp class high): 5 mm |
| `AD-6` | flexible material: replaces the widths above with 0 mm, or 3 mm when AD-1 or AD-3 fired; it does not change the level |
| `LVL-0`, `LVL-1`, `LVL-2`, `LVL-3` | the level, see below |
| `LVL-SILK`, `LVL-HIST`, `LVL-ASK` | the level was raised by a silk spool, by the history, by a request |
| `CARD` | the value of the material's print card for this plate and level |
| `PLT-1`, `PLT-2` | plate refused for the material; bed temperatures held at the printer's and the filament's limit, the other layers never hotter than the first |
| `MAT-1` | thin walls (strip under 3 mm or wall up to 2.4 mm): at least 3 wall loops |
| `MAT-2` | flexible material: the refusals above, and a warning above 4 pieces |
| `VNT-1` | the venting line, with its condition |
| `SUP-1` to `SUP-4` | supports on or off, threshold angle, where, kind |
| `SPD-1`, `SPD-2` | every motion field is a cap; gentle motion for very slender parts |
| `PT-1` | the prime tower is off |
| `NOZ-1` | the main nozzle only |
| `INT-1`, `INT-2` | the intent's value; the `balanced` value because the intent's is not available |
| `STK-1` | the sticking baseline, kept or written |
| `FIT-1` to `FIT-5` | no inner brim in a bore of a fit; brim gap 0 for a brim that holds; the after-print line; calibrated fields locked; bed temperature on fit plates |
| `PRV-1` | the proven baseline: a value of a print the person reported as good on this printer, plate and material, where the rules alone give a less careful one |
| `SIG-1` | a value of the fit calibration; a gauge's intent |
| `SET-1`, `FIX-1` | the person's setup; a value Hotend fixes (skirt 0, nozzle temperatures left to the profile) |
| `XF-1` | a rule that ties several fields together: the brim kind follows the width, a token must be verified, a change that breaks such a tie is taken back |
| `ENV-1` | the end of an envelope that no other rule owns: the hard range, or the widest brim |

Some ids never stand in a decision of the rule plan: `PLT-1` and `MAT-2` act as refusals and
warnings, `VNT-1` and `FIT-3` as list lines, `FIT-1` through the allowed set of the brim kind,
and `FIT-4` as the lock (`envelope: {"locked": {"rule": "FIT-4"}}`) of elephant foot, first
layer height and first layer line width on a plate with `fit_at_bed` entries.

The facts are read on their careful side: the strip width is the smaller of the stated width
and `2 x area / perimeter`, the tipping ratio the larger of the stated slenderness and
`height / sqrt(contact)`, and facts that contradict each other are refused before any rule
reads them ("Plate facts"). An engine that states a comfortable number for an uncomfortable
shape cannot talk a brim away.

### Brim

- **Kind.** `outer` when the plan has a brim width, else `none`. While a brim is drawn,
  `none` is not in the allowed set. `outer_and_inner` is offered only when its token is
  verified (`capabilities`), the plate has an inner loop, every inner loop on the plate is at
  least `2 x width + 10` mm wide (the kind is one value for all pieces, so one narrow
  hole anywhere takes the offer away) and no inner loop belongs to a declared fit (FIT-1).
  `inner` alone is offered under the same conditions and only when no brim is required,
  because it would take the outer brim away.
- **Gap.** 0 for every rigid family whenever the brim is there to hold the part (required by
  a brim rule, by level 2 or by a proven baseline) and the elephant foot compensation the plan
  counts on is at least 0.1 mm (FIT-2): such a brim must hold the edge. A brim nobody requires
  (asked for by a change), and every flexible material, keeps the card's gap (0.1 mm).
  A brim of a proven baseline counts as required: a brim with a gap does not touch the part and
  cannot hold a corner down, and the brim of the test print below (measured) had gap 0
  (`brim_object_gap = 0` in its slice) and held. TPU keeps 0.1 mm: its brim is hard to cut off.
- **Limits.** The floor for any later change is the rules' width, `rule_value` of the
  decision `adhesion.brim_width_mm`, and the ceiling `derived.brim_width_max_mm` (10 mm, 15 mm
  for warp class high). `derived.asked_brim_width_mm` is what the brim rules alone ask for,
  before level 2 widens it; it equals the floor only below level 2 (at level 2 a rule width of
  5 mm is planned as 8 mm).
- **Clearance of the layout.** The room the layout keeps round every piece comes from the
  width the plan finally has, never from `asked_brim_width_mm`: the `clearance` passed to the
  engine is the plan's `adhesion.brim_width_mm` (the `value` of that decision, after the level
  and after every accepted change) plus the spacing allowance of the layout, and it is
  computed again after every accepted change and every change of the level. At level 2, or
  after a change, the planned brim is 8 to 10 mm where the rules asked for 3 to 5; a layout
  made with the smaller number lets brims merge or cross a keep-out. When the wider brim does
  not fit, that is `brim_does_not_fit`, not a narrower clearance.
- **Flexible material at level 2.** AD-6 keeps a flexible brim at 0 or 3 mm; level 2 still
  widens it like any other (5 to 6 mm), because the level exists to make the print hold and a
  flexible piece that came loose needs it too. Such a brim does not snap off, so the list then
  says how to cut it away (`after.flexible_brim`).

### Levels

| Level | Chosen when | Effect |
| --- | --- | --- |
| 0 normal | no brim rule fired | the card's values for level 0 |
| 1 at risk | any of AD-1 to AD-5 fired | the card's first layer speeds, bed pair and aux fan cap for level 1; the sentence of the level names what that changes on this plate (`level.1`: slower and hotter; `level.1.slower`, `level.1.hotter`, `level.1.same` where the card has the same speeds or the same bed pair at both levels) |
| 2 stubborn | AD-3 above 4, the history, or a request | the card's values for level 2; brim at least 5 mm and 3 mm wider than the rules' width, 10 mm at most and never narrower than the rules' width; part fan off for 5 layers; first layer 0.25 mm on Textured PEI unless a calibration sets it or mating faces stand on the bed |
| 3 ask | the history only | no further setting; the note `level_3` and the list lines `plate.check`, `run_calibration` and, for a rigid material on a plate whose card does not already require glue, `glue.allowed` (the vendor's material table allows glue for every family; a flexible material keeps `glue.none` where its card says so) |

A silk spool raises the level by one, to 2 at most. `history.min_level` is a floor, and so is
the level of a proven baseline (`level.proven`, rule `PRV-1`). A request
(`PlanInputs.level`) can raise the level to 2 at most; a request below the rules' level
changes nothing and is recorded in the decision's `asked`. Nothing lowers a level: every step
takes a maximum. A test checks that a higher level never narrows the brim, cools the bed,
shortens the fan delay or speeds up the first layer, for every card and plate.

On a plate with `fit_at_bed` entries (FIT-5), level 1 raises only the first layer's bed
temperature; the other layers keep the level 0 value and are raised at level 2 only. A product
plate with such entries and a recorded gauge takes the gauge's bed pair, and never a colder one
than the card gives the plate's own level: each of the two temperatures is the higher of the
recorded one and the card's for that level, whatever raised the level (the geometry, a silk
spool, the history or a request). A gauge that was printed at a lower level or on another
plate therefore cannot cool the bed under a thin or slender piece. Whenever the result is not
the recorded pair, the decision says `bed.raised_over_calibration` and the plan carries the
warning `fit_signature_differs`: the fit is then not printed as it was calibrated.

A printer or filament limit inside the card's window holds the bed at the limit with the
warning `bed_limited`; `derived.bed_window_c` is then `[low end of the card's window, limit]`.
A limit below the low end blocks the plan (`bed_limit_below_window`). `verify` checks both bed
temperatures against the card's own window, not against a window the plan states.

### Print cards

`data/print-cards.toml` has one card per material family (PLA, PETG, TPU, ABS, ASA; a pytest
checks this against `hotend_fit.materials`). A card gives, per plate, the status (`ok`,
`warning`, `refused`), the bed pair at level 0 and from level 1, the window a bed temperature
may be moved in, and whether glue is needed; and per level the first layer speeds and the aux
fan cap. It also gives the warp class, the fan delay, the brim gap, the vent mark, the feed
(any slot, or the outside holder of the main nozzle), flow and outer wall caps, grades by
Shore hardness, the steepest overhang the family prints without support (`max_overhang_deg`;
the ceiling of the support threshold is 90 minus it), and the ids of its list lines. Cards for
ABS and ASA are marked unproven: the plan carries the warning `settings_not_proven`.

A grade row (`[[card.grade]]`: `shore_a`, flow cap, list lines) holds for its hardness and
every softer one down to the next row: `Card::grade` takes the row with the lowest hardness
that is not softer than the material. A material harder than every row has no grade (TPU 95A:
no flow cap, no hand-loading line), and a material whose id names no hardness gets the softest
row, so an unknown filament is printed as carefully as the softest one the card knows. The
hardness is read from the id as `python/hotend_fit/materials.py` reads it: the first two
digits that are followed by `a` and then by the end of the id or a hyphen (`tpu-085a` is 85A on
both sides). `tests/data/shore-table.json` holds ids with the hardness each names; the Rust
test reads it, and `python/tests/fit/test_print_cards.py` holds the fit library to the same
rows.

PLA on the Engineering Plate needs glue (`glue = "required"`): the vendor's page on plates
asks for adhesive on that plate for every filament.

Loading refuses a card whose numbers leave the hard ranges or the constant floors (a bed
window outside the family's range, a fan delay under 3 layers), whose raised bed pair is below
the normal one, or whose speeds or fan cap rise with the level. A data file can therefore make
a plan more careful, and cannot make a level weaker than the one below it.

### Intents and the sticking baseline

`data/intents.toml` gives, per intent, the process preset, wall loops, infill, seam and caps.
Rules can only raise what an intent gives (MAT-1 for walls; caps only lower speeds). A preset
that is not installed or a token that is not verified falls back to the `balanced` value with
the warning `intent_value_unavailable` (`INT-2`). A gauge is always `fit_critical`.

The intent never reaches the sticking group. With the standard preset the three first layer
values of the baseline and the first layer acceleration stay unset, and the proof demands the
baseline in the sliced file. With any other preset the plan writes the four baseline values
(STK-1), so a preset cannot move them. A calibration or level 2 takes precedence for its field.

### Supports and motion

- **SUP-1.** The counted area is the sum over the plate of the overhang regions that are not
  a bridge of at most 20 mm, not the ceiling of a round hole of at most 8 mm and not a free
  overhang reaching at most 2 mm. Under 20 mm2 supports
  are off; `on` is offered when the plate's whole overhang is at least 4 mm2. From 20 mm2
  they are on. For flexible materials the limits are 50 and 200 mm2.
- **What forces supports.** `off` leaves the allowed set only when the counted bridges and
  free overhangs of the classified pieces are above 100 mm2 (200 for a flexible material).
  The ceiling of a round hole wider than 8 mm counts toward "on" and never toward "forced":
  such a hole prints without support, and support inside a through hole or on a fit bore
  cannot be dug out without scarring it, so the person must be able to switch it off.
  A piece the engine did not classify (`overhang_regions: null`) adds its whole overhang to
  the counted area and forces nothing; it does not unlock supports that the classified pieces
  force. One counted area is told in the singular (`supports.on.one`).
- **SUP-2.** Threshold 30 degrees. **SUP-3.** `bed_only`, unless at least 4 mm2 of counted
  overhang lie over the part on some piece: then `everywhere`. The area over the part is the
  larger of the sum of the counted regions the engine flags `over_part` and the piece's
  `overhang_over_part_mm2` less the regions that are not counted, because the engine flags a
  region only when 4 mm2 or half of it hang over the part, and several small ones would
  otherwise never add up. `bed_only` leaves the allowed set only when that area, without the
  hole ceilings, reaches 4 mm2: a hole ceiling alone never locks `everywhere`.
  **SUP-4.** `tree`; `normal` when a piece has at least 4 mm2 of overhang on a mating face and
  the token is verified.
- **SPD-1.** No speed or acceleration is raised: every motion field is a cap, and of several
  caps the lowest wins. **SPD-2.** A tipping ratio above 4 sets the acceleration cap 4000, the
  outer wall acceleration cap 2000 and the outer wall cap 100 mm/s.

### Fit signature

`FitSignature` (`signature.rs`) is `{process, layer_height_mm, first_layer_height_mm,
first_layer_line_width_mm, elephant_foot_mm, outer_wall_cap_mm_s, first_layer_bed_c,
layers_bed_c, brim_gap_mm, material_id, nozzle_id}`; every number is checked against its hard
range when the signature is built, and `sha256()` hashes its canonical JSON.
`RuleBook::expected_signature(&plan)` gives the signature the sliced file of a plan must state
(the plan's values, with the baseline for the fields the plan leaves unset). The export builds
the stored signature from the proof's parsed values, not from the plan.

A product plan with a `calibration` for the same material and nozzle sets elephant foot, first
layer height and first layer line width to the recorded values (`source: calibration`, rule
`SIG-1`), takes the recorded bed pair on a fit plate (FIT-5, never colder than the plate's level
asks for), and carries `fit_signature_differs` when the preset, the layer height or the outer
wall cap differ or a bed temperature differs by more than 5 C. The warning is shown on every
product plate with a recorded gauge: the facts name only the mating faces on the bed and in
overhangs, so the plan cannot know that a plate has no fit at all, and an intent without the
gauge's outer wall cap does differ from it. A signature for another material or nozzle is
ignored. Recorded first layer values that the fitted nozzle cannot print are not used, and the
warning is shown. A gauge plan takes no calibration: it defines one.

### Sentences

Every text of a plan comes from `data/sentences.toml`: `[decision]` (one sentence per reason
of a decision), `[notice]` (refusals, questions, warnings, notes), `[answer]` (the answers to
a proposed change), `[[line]]` (the list, each with its class) and `[word.*]` (the names of
plates, intents, colours and askers). The code declares each id with the places it fills
(`explain::DECISIONS`, `NOTICES`, `ANSWERS`, `LINES`); loading fails
when an id, a place or a word is missing or left over on either side. A place is filled only
with a number, a calendar day (`model::Day`, written `YYYY-MM-DD`: the `{date}` of the
proven baseline's sentences), or a word of the catalogue or of a closed enum. No text of a
person, a model or a file can reach a sentence, and no sentence names a slicer key: the
person's label of a proven print is shown beside the plan by the core, never inside it.

### The before-you-print list

`plan.checklist` is `{items: [{id, class, text, condition}], limit}`, sorted by class:

| Class | Lines |
| --- | --- |
| 1 | the plate to put on, glue (`glue.required`, `glue.none`, at level 3 `glue.allowed`), an unproven material |
| 2 | the spool and its slot; for a flexible material `tpu.dry` (dry the spool, put it higher than the inlet) and, for the grades the card names (90A and softer, or unknown), `tpu.load` (load by hand; the printer cannot unload it) |
| 3 | which file to open; the line for the editable project |
| 4 | printer options (no sync, bed levelling, calibration) |
| 5 | during the print: venting, watching the brim, few pieces |
| 6 | after the print, and reporting the outcome |

`Checklist::lines(print_minutes, project_delivered)` gives the lines to show. A line with the
condition `print_longer_than_minutes` (the venting line of VNT-1, 60 minutes) is kept while
the print time is unknown and dropped when the slicer reports a shorter print; the line with
`project_delivered` is shown only when the editable project is handed over as well; it is a
class 3 line and stays whenever the project is delivered.

The limit is 8 lines, 10 for a flexible material, and 3 more at level 3, which adds three
lines only it has (`checklist::LIMIT`, `LIMIT_FLEXIBLE`, `LEVEL_3_EXTRA`). When there are more
lines, they go from the highest class, the last line of that class first. Never dropped are
the lines of the classes 1 to 3 and the lines of `checklist::ALWAYS_SHOWN`
(`Checklist::never_dropped`):

| Line | Why it stays |
| --- | --- |
| `after.brim_fit` | the brim touches a mating face (gap 0); scraping it flush is what protects the fit |
| `after.flexible`, `after.flexible_brim` | how a flexible part, and its brim, come off the plate |
| `outcome` | "did every piece stay down?" is what the history of the level rests on; it is always the last line |
| `few_pieces` | MAT-2 asks for this line on a crowded flexible plate |
| `run_calibration` | what level 3 asks for |

So the plain brim line `after.brim` and `after.cool` go first (class 6), then `watch_brim` and
`vent` (class 5), then `calibration`, `bed_levelling` and `no_sync` (class 4). With the project
line (class 3, shown whenever the project is delivered) a hard limit and "classes 1 to 3 are
never dropped" cannot both hold; this choice keeps the fit and the history. A list is longer
than its limit only by lines that are never dropped; below level 3 no test plate has more of
them than the limit (8 and 10 lines at most). A plan carries one line about what to do
afterwards: `after.brim_fit` or `after.brim` where a rigid brim is drawn, `after.flexible_brim`
where a flexible one is, else the card's (`after.cool`, `after.flexible`).

The four hand-loading sentences of a flexible material take two lines, so a flexible list has
room for its other lines. The venting line says to take the top glass cover off once the first
layers are down, as the vendor's material table words it.

### What is data

| File | Holds | Revision in the plan |
| --- | --- | --- |
| `data/print-rules.toml` | every threshold and width of the rules, the baseline, the level effects, the cross-field factors | `revisions.print_rules` |
| `data/print-cards.toml` | the material cards | `revisions.print_cards` |
| `data/intents.toml` | the intent table | `revisions.intents` |
| `data/sentences.toml` | every sentence and word | `revisions.sentences` |

Each table of the rule data and each number of a card carries a mark: `measured`, `sourced`,
`judgment` or `stock`, with its source; the mark becomes the `basis` of the decision. The
files are embedded at build time and checked when first used (`RuleBook::builtin()`);
`RuleBook::from_toml` builds a rule book from other texts, for tests. The hard ranges are not
data: they are constants in `bounds.rs`, and a data file whose value leaves one, or asks for
less than a floor of `bounds::floors`, fails to load.

### What a plan says beside its decisions

`plan.derived` holds what the rules worked out and no single decision shows: the rules that
fired, the rules' level, the asked brim width and its ceiling, whether the brim is required,
the bed window cut at the printer's limit, the family's nozzle range, the wall loop floor, the
counted support area,
whether gentle motion is on, whether a calibration applies, and whether mating faces stand on
the bed.

`plan.warnings` and `plan.notes` are `{code, sentence}` records like `blocked`:

| Code | Kind | When |
| --- | --- | --- |
| `plate_not_recommended` | warning | the card marks the plate `warning` for the material (PLA on the Engineering Plate) |
| `turn_or_split` | warning | AD-3: a tipping ratio above 6 |
| `too_many_flexible_pieces` | warning | MAT-2: more than 4 flexible pieces; the list carries `few_pieces` |
| `intent_value_unavailable` | warning | INT-2: a value of the intent is not installed or not verified |
| `fit_signature_differs` | warning | the product is not printed as its gauge was (see "Fit signature") |
| `settings_not_proven` | warning | the card is unproven (ABS, ASA); the list starts with `unproven` |
| `bed_limited` | warning | PLT-2: a bed temperature was held at the printer's or the filament's limit |
| `chamber_fan` | note | always: the chamber fan of the first layer is the printer's own |
| `level_3` | note | level 3 |
| `dedicated_hotend` | note | a flexible material |
| `proven_baseline` | note | a proven baseline applies to the plate; the sentence names the day of the print |
| `proven_limited` | warning | a value of the proven baseline lies beyond what the plate allows (the bed window cut at the printer's limit, the widest brim, the longest fan delay) and is held there |

### Proven baselines

Source: `src/inputs.rs` (`Baseline`, `BaselineValues`), `src/rules.rs`, `src/envelope.rs`;
tests `tests/baseline.rs`. The core keeps the records (docs/PROTOCOL.md, section 8, "Proven
baselines") and the command line seeds them (`hotend baseline set`).

A proven baseline is the set of sticking settings of a print the person reported as good, for
one printer, plate type and material. It is not part of the rules and not a default for
anybody: it exists only in the data folder of the person who recorded it, and without one the
rules decide alone, byte for byte as before (a plan without a baseline has no trace of one;
`derived.proven_on` and `derived.proven_fields` are left out).

| Field | Floor |
| --- | --- |
| `adhesion.level` | 0 to 2, a floor like `history.min_level` (step 3); a higher level brings its own effects (level 2: wider brim, longer fan delay, thicker first layer on Textured PEI) |
| `adhesion.brim_kind`, `adhesion.brim_width_mm` | `none` with 0, or `outer` with a width above 0: the brim is at least that wide, held to the widest brim of the plate (10 mm, 15 mm for warp class high); a brim of the baseline is required, so `none` is not allowed and FIT-2 gives it gap 0 |
| `first_layer.wall_speed_mm_s`, `first_layer.infill_speed_mm_s` | at most the baseline's |
| `first_layer.bed_c`, `layers.bed_c` | at least the baseline's, held to the top of the card's window cut at the printer's and the filament's limit; the other layers never above the first |
| `cooling.part_fan_off_layers` | at least the baseline's, held to 5 layers (the fan delay of level 2) |
| `cooling.aux_fan_first_layers_pct`, `cooling.aux_fan_max_pct` | at most the baseline's (a cap where the card names none) |
| `shell.wall_loops` | at least the baseline's: the envelope's floor (`derived.wall_loops_floor`) is the higher of MAT-1's and the baseline's, owned by `PRV-1` when the baseline's is higher |

`Baseline::new` refuses a value outside its field's hard range, a level above 2, a brim kind
other than `none` or `outer` (no other kind has been printed with), a width that disagrees with
the kind, and a bed after the first layer hotter than during it. It holds the plate, the
material and the day of the print (`model::Day`), and no text.

**How the floor is applied.** The rules decide every field as before, at the level that the
baseline may have raised; then each field the baseline holds takes the baseline's value where
the rules' value is less careful (`envelope::not_weaker`; the wall loops upwards). A baseline
value beyond what the plate allows is held there, the decision says so (`proven.limited`) and
the plan carries the warning `proven_limited`. The value the baseline gives is the rules' value
of the plan (`rule_value`, `rule_source: proven`), so every envelope starts there: no change by
the agent or the person, and no requested level, can take a field below the baseline. The
rules may go further: a slender piece still gets level 2, a wider brim, a slower first layer
and a longer fan delay, and those decisions name their own rule.

**Where a value came from.** A decision says `source: proven`, rule `PRV-1`, basis `measured`,
with a sentence that names the day of the print, exactly when the baseline is the reason for the
value: the rules' value at the plan's level is less careful than the baseline, or equal to it
while the rules alone (computed again without the baseline, at the level they give without it)
are less careful. Where the rules alone give the same value, the rule keeps the decision (for
the test print below: the gap by FIT-2, the three walls by MAT-1, the fan delay and the
auxiliary fan on the first layers by the card). `derived.proven_fields` lists the fields of
`PRV-1` decisions. Level 1 or 2 of a baseline also reaches the bed of a plate with a recorded
gauge, which FIT-5 raises for the level and names the level's source (`proven`) with its own
sentence.

**`verify`.** Every plan with an applicable baseline is checked against it once more: each of
its fields at least as careful as the baseline, held only to the plate's limits above
(`PRV-1` violation otherwise).

**A test print (measured).** A ring-shaped test print (Textured PEI, Generic PLA, X2D) held. Its
slice has bed 65 C on the first layer and 60 C after, first layer 20 mm/s on the walls and 30
mm/s on the infill, an outer brim of 5 mm with gap 0, the part fan off for 3 layers, the
auxiliary fan at 0 on the first layer and at most 30 percent, 3 walls and no prime tower. The
rules alone give that plate level 0, no brim (gap 0.1 mm), first layer 30 and 50 mm/s, bed 60
and 55 C, auxiliary fan cap 50 percent and 3 walls (MAT-1, a thin wall): no strip on the bed is
narrow and the pieces are short, so no brim rule fires. With the print's baseline (`level 1`,
outer brim 5 mm, 20 and 30 mm/s, 65 and 60 C, fan off 3, auxiliary 0 and 30 percent, 3 walls)
the plan is exactly that slice's settings; `tests/baseline.rs` holds the plate by the engine's
facts and checks both plans field by field.

## Envelopes and proposals

Source: `src/envelope.rs`, `src/propose.rs`; tests `tests/properties.rs`.

The agent and the person may ask for other values. They never write a value: they propose
`{field, value, reason}`, and the plan answers each change with *accepted*, *clamped* or
*refused*. What can be asked for on a plate is the field's *envelope*, which the rules compute
from the same facts as the value.

### Envelopes

Every decision carries the envelope of its field on this plate (`Decision.envelope`). The
rules' own value always lies inside it, and no envelope leaves the field's hard range.

| Field | Envelope | Rule that owns the bound |
| --- | --- | --- |
| `intent` | the five intents; locked on a gauge | `SIG-1` |
| `adhesion.level` | the rules' level to 2 | below: the rule that gave the level; above: `LVL-ASK` |
| `adhesion.brim_kind` | a set, see "Brim kind and width" | the brim rule, `FIT-1`, `XF-1` |
| `adhesion.brim_width_mm` | the rules' width to `derived.brim_width_max_mm` (10 mm, 15 mm for warp class high) | below: the brim rule; above: `ENV-1` |
| `first_layer.wall_speed_mm_s`, `first_layer.infill_speed_mm_s` | the hard minimum to the rules' value | above: `CARD` |
| `first_layer.accel_mm_s2` | 250 to the rules' value; to the baseline's value while the field is left to the installed profile | above: `STK-1` |
| `first_layer.bed_c` | the rules' value to the top of the card's window, cut at the printer's and the filament's limit | above: `PLT-2` |
| `cooling.part_fan_off_layers` | the rules' value to the fan delay of level 2 (5 layers) | above: `LVL-2` |
| `cooling.aux_fan_max_pct` | 0 to the rules' cap; the hard range when the card sets no cap | above: `CARD` |
| `shell.wall_loops` | `derived.wall_loops_floor` to 6 | below: `MAT-1` |
| `shell.infill_pct` | the hard range | `ENV-1` |
| `shell.infill_pattern`, `shell.seam` | the value the plan has, the `balanced` value, and every token the slicer side has verified | `XF-1` |
| `supports.enabled`, `supports.kind`, `supports.where` | the sets of SUP-1, SUP-4 and SUP-3 | the support rule |
| `supports.threshold_deg` | 20 to 90 minus the card's `max_overhang_deg` (45 today) | above: `SUP-2` |
| the five `motion.*` caps | the hard minimum to the rules' cap; the hard range when no rule set a cap | above: the rule that set the cap |
| every other field | locked | the rule that decided the field |

`envelope::side(field)` states the safe direction of a field, and
`envelope::not_weaker(field, value, reference)` compares two values of it:

- **Up** (a higher value holds the print better): level, brim width, first layer height, both
  bed temperatures, fan delay.
- **Down** (a lower value is the careful one): brim gap, first layer speeds and acceleration,
  both auxiliary fan fields, and every `motion.*` field. A cap that is left to the profile is
  the weakest cap, so naming a cap where the rules named none is allowed.
- **Fixed**: skirt loops, elephant foot, first layer line width, the nozzle temperatures, and
  the brim kind (a kind is at least as safe as another when it draws everything the other
  draws; `none` is the weakest).
- **Either**: the shell and support fields and the setup.

An envelope of a sticking field starts at the rules' value and reaches only to the safe side.
A field whose rule value is already at the end has a range of one value (`min == max`), which
reads as "adjustable in general, nowhere to go on this plate". A numeric field moves in steps
of 1, a decimal field in steps of 0.01.

`Envelope::contains`, `Envelope::in_hard_range` and `Envelope::corners` (the two ends of a
range, every value of a set) are public for the tests and for the window.

### Accepted changes are inputs

A plan stays a function of its inputs. What somebody asked for is part of them:

| Input | Holds |
| --- | --- |
| `PlanInputs.intent`, `PlanInputs.level` | the asked intent and the asked level, each with who asked |
| `PlanInputs.changes` | `Changes`: at most one `{value, by}` per field, for every other adjustable field |

`Changes::with(field, value, by)` takes a typed value inside the field's hard range for a field
marked A, and nothing else (`ChangeError`). There is no reason, no time and no order in a
`Changes`: it is a set, the newest change per field (with one exception for the agent, see
"Answering a proposal"). It serializes to a list of
`{field, value, by}` in the order of the field table, and `Changes::restore(&json)` reads that
list back through the same checks (an unknown key, a field twice, a value of the wrong type or
outside the hard range refuse the whole list). `PlanInputs::reset()` gives the same plate with
nothing asked for; its plan is the rule plan.

A change is stored **as it was asked for** and judged **every time the plan is computed**:

1. the rules decide the field's value and its envelope, at the plan's level and intent;
2. the asked value is judged against that envelope: inside, it is the value; a number outside
   is held to the nearest bound; a token or flag outside the allowed set leaves the field as
   the rules have it;
3. the fields are decided in the order of the field table, so a field that takes its default
   from an earlier one (the brim kind from the width, the bed after the first layer from the
   first, every level-dependent value from the level) sees the changed value.

So a set that was accepted on one day cannot bring a value outside an envelope on another:
when the level rises, the card changes or a rule gets stricter, the stored change is held to
the new bound in the next plan. The decision shows it: `asked` is the value asked for, and
`answer` is `{by, answer, rule, detail}` with `answer` one of `accepted`, `clamped`,
`not_allowed_here`, `locked` or `conflict`. `source` becomes `agent` or `person` only where the
value differs from `rule_value`; a change that was held to the rules' own value leaves the
source as it was and names the rule that owns the bound. `PrintPlan::changed_by(asker)` counts
the fields whose value differs from the rules' because that asker asked.

After the last field, `RuleBook::verify(&inputs, &plan)` checks the whole plan: every value in
its hard range and in its envelope, no sticking field and no cap on the weak side of its rule
value, and the cross-field rules: a first layer the nozzle can print, bed temperatures inside
the window and under the printer's limit with later layers never hotter than the first, one
filament and no prime tower, a brim kind that draws exactly when the brim has a width,
calibrated fields equal to the gauge's, wall loops at or above the thin-wall floor. `plan`
returns only plans that pass. When a change breaks a cross-field rule, that change is taken back
(`answer: conflict`) and the plan is computed again; this ends at the latest with the rule plan.
`Err(PlanError::Cross)` means that the rule plan itself breaks a rule, which is a defect of a
data file or of the code.

Two checks stay with the caller: whether the brim fits on the bed needs the layout,
and the JSON Schema of the proposal tool is applied at the tool gateway before the arguments
reach this crate (the crate checks the same things again).

### Brim kind and width

The brim is one thing with two fields. The width leads and the kind follows:

- The width is decided first and takes its change first. The kind's default is `outer` when the
  plan has a width and `none` when it has none.
- While a brim is drawn, `none` is not in the allowed set: a brim a rule requires cannot be
  switched off (`not_allowed.brim_required`, with the brim rule), and a brim somebody asked for
  is removed by asking for width 0 (`not_allowed.brim_asked`).
- `inner` and `outer_and_inner` need a verified token, no hole of a declared fit (`FIT-1`), a
  hole on the plate, and every hole of the plate at least `2 x width + 10` mm wide for the
  width the plan has now. `inner` alone is never offered where a brim is required.
- A kind other than `none` on a plate without a width draws nothing. It is the one change that
  an envelope cannot judge alone, and it is answered `conflict`
  (`conflict.brim_needs_width`, `XF-1`): ask for a width, or for both in one proposal.
- A brim that only a change asked for keeps the card's gap (0.1 mm); gap 0 belongs to brims a
  rule requires (`FIT-2`). The decision of the kind then says `brim.kind.asked` and names the
  asker of the width.

### Answering a proposal

```rust
let outcome = book.propose_json(&inputs, Asker::Agent, &arguments)?;
// or: book.propose(&inputs, &proposal)?
// outcome.inputs is what to store; outcome.plan is book.plan(&outcome.inputs)
```

`Proposal::from_json` reads `{plate?, changes: [{field, value, reason}], summary}` and refuses
any other shape. `propose` then answers in this order:

1. **Schema.** No change, more than 16 changes, a field name that is not one of Hotend's, the
   same field twice, a value of the wrong kind, a number that is not finite or is a negative
   zero, a fraction where a whole number belongs, a number outside the field's hard range, a
   token that is not in the field's list, a reason that is not one printable line of 1 to 240
   characters, a summary over 300 characters or with a control character: the **whole
   proposal** is refused with `invalid_arguments`, every item says which fault it has
   (`invalid.*`; a valid item beside a faulty one says `invalid.other_item`), and nothing
   changes. No answer repeats what was sent: `note` is `null` for every item and `summary` is
   `null`, also for the items and the summary that were valid.
2. **A blocked plan** has no envelopes: `plan_blocked`, nothing changes.
3. **Authority.** A field without A is refused with `not_settable`, or with
   `locked_by_calibration` when it is a calibrated field on a plate with a calibration. Its
   value is not read. The other changes go on.

   **What the person made safer stays.** A change by the agent is refused with `kept_by_person`
   when the person has an accepted change for the same field that is in effect, the field holds
   the print or the fit (`envelope::side` is not `Either`: the level, the sticking group, the
   caps) and the agent's value would hold it less well (`envelope::not_weaker`). The person's
   change, and the person's level, stay as they are. The agent may ask for the same value or for
   a safer one, and the person may take the choice back at any time. If the agent's other
   changes would make the plan drop such a choice of the person, all changes of that proposal
   are refused the same way. For the fields of side `Either` the newest change wins, whoever
   asked. (A plain "the newest per field wins" would let an agent's clamped value replace the
   person's wider brim or higher level; this rule cannot weaken adhesion.)
4. **Envelope, order and cross-field rules** are the plan's own (above). The plan is computed
   with the new changes beside the accepted ones. A change the plan does not apply is taken
   out again, the field gets back the change it had before, and the plan is computed once
   more, until the set holds only what the plan applies.

`ProposalOutcome` is `{by, refused, items, dropped, summary, inputs, plan}`:

- `refused`: `{code, detail, message}` when the whole proposal was refused, else `null`;
- `items`: one `ItemOutcome` per change, in the order written:
  `{field, outcome, code, detail, asked, applied, rule, message, note}`. `outcome` is
  `accepted`, `clamped` or `refused`; `code` is `accepted`, `clamped`, `invalid_arguments`,
  `plan_blocked`, `not_settable`, `locked_by_calibration`, `locked`, `not_allowed_here`,
  `conflict` or `kept_by_person`; `detail` is the id of the `[answer]` sentence in `message`; `rule` names the rule
  that owns the bound or the set. Its `Display` form is one line of ids and values, such as
  `adhesion.brim_width_mm: clamped 12 -> 10 by ENV-1` or
  `adhesion.brim_kind: refused: not_allowed_here (AD-1)`;
- `dropped`: changes accepted earlier that the new plan no longer allows (an inner brim after
  the brim was widened past the hole). They are taken out of the set and reported here, so no
  change disappears without an answer;
- `inputs`: the inputs with the accepted set; `plan`: the plan of exactly these inputs.

A clamped change stays in the set as asked, an accepted one too; a refused one is not stored,
and an earlier accepted change of the same field stays in place. The answers do not depend on
the order in which the changes of one proposal are written. Proposals that arrive one after
the other end in the same plan in every order, unless one order answered a change with a
refusal or reported a dropped change; the property tests check exactly this.

### Reasons and summaries

`reason` and `summary` are kept as a `Note`: one printable line for display. `Note::new` refuses
control characters (line breaks among them) and anything too long. It keeps the characters
Unicode has assigned as letters, digits, punctuation and symbols, and turns everything else into
a space: format and tag characters (zero-width characters, direction overrides, the soft
hyphen), variation selectors, fillers that look like a blank (the Hangul fillers, the braille
blank), private-use and unassigned code points and the line separators. At most four combining
marks are kept on one character. A reason that then shows nothing is refused like an empty one.
The character classes come from the `unicode-general-category` crate. A note is part of the
answer (`ItemOutcome.note`, `ProposalOutcome.summary`) and of nothing else: `Changes`,
`PlanInputs`, the plan, its sentences and its list have no place that could hold one. The same
change with another reason gives the same plan, byte for byte. The core may store the notes
beside the proposal; it must not pass them anywhere a value, a file name or a list line is
built.

### What the core does with it

- Keep `inputs.intent`, `inputs.level` and `inputs.changes` with the plate, and recompute the
  plan from the stored inputs whenever it is needed; the stored plan is a record.
- Apply the JSON Schema (`schema::proposal_schema()`) at the tool gateway, then call
  `propose_json`. The person's changes from the window or the command line take the same path
  with `Asker::Person`.
- Show `ItemOutcome` and `dropped` as they are; the sentences come from the catalogue.
- "Reset to the rules" is `inputs.reset()`.
- Check that the brim fits on the bed with the layout before a plate file is written.

### Properties under test

`tests/properties.rs` uses a small deterministic generator of its own (`tests/common`), so every
run sees the same plates; the whole file runs in a few seconds. The generator has a fixed seed
and a fixed number of cases, and no shrinking library. A failure names the plate by its number
in the sequence, which reproduces it exactly. `tests/review.rs` holds regression tests, each
stating the plate that showed the defect, and `tests/schema.rs` the validator run of the
proposal schema. `tests/engine_facts.rs` joins the two halves: it reads the engine's example
`python/tests/engine/data/ring-gauge-facts.json` as written and checks that the plan is the
gauge's recipe, field by field the plan of the hand-written gauge facts.

- Every value lies in its envelope and every envelope in its hard range, on every plate and
  after every proposal; the test recomputes this and the cross-field rules itself and also
  asks `verify`.
- No sequence of proposals moves a sticking field to the weak side of the rule plan (the plan
  of `inputs.reset()`), comparing values as they take effect.
- The same inputs give the same plan and the same answer, also from a rule book loaded afresh;
  a proposal applied twice changes nothing more; the order of the items of a proposal does
  not matter; proposals in any order give the same plan or an answer that says what was
  refused or dropped.
- Every corner of every envelope is delivered exactly and gives a valid plan, alone and all at
  once. The one exception is stated above: a brim kind on a plate without a brim width is
  answered `conflict` alone and is delivered together with a width.
- With a proven baseline (`tests/baseline.rs`, random baselines on the generated plates and
  random proposals): no sticking field and no wall loop count is less careful than the plan
  of the rules alone, every field of the baseline is kept (held only to the plate's limits),
  the rules still go beyond the baseline where the plate needs it, `verify` passes, the plan
  is a pure function of its inputs, and no proposal moves a field below the baseline.
- Malformed proposals (unknown fields, slicer keys, look-alike names, wrong types, NaN,
  infinities, huge numbers, negative zero, fractions, texts where numbers belong, very long
  reasons, control characters, unknown properties, ten thousand changes) are refused without a
  panic, change nothing and are not repeated in the answer.
- Every answer sentence is reached, except the general `conflict`, which no known input
  produces.
- A test reads this document and fails when a code that blocks a plan, a warning code or an
  answer code of the catalogue is not named in it.

## In the slice

Source: `crates/hotend-slicer/src/keymap.rs`, `src/proof.rs`, `src/project.rs`,
`profiles/keymap-bambu-studio.toml`, `profiles/bambu-gcode-facts.toml`. Details of the files and
codes are in `docs/PROTOCOL.md` section 6.

The core hands `hotend-slicer` the plate's `PrintSettings` in the slice request
(`SliceRequest.print_settings`) together with the fitted nozzle size
(`SliceRequest.nozzle_diameter_mm`). It never names a slicer key. With the real Bambu Studio:

- **Preset and material.** `shell.process` selects the process preset and the plan's filament
  selects the material of `system-profiles.toml` (`pla-generic` is Generic PLA). The plate of
  the settings is the plate sliced for; a slice asked for another plate is refused. A nozzle
  size Hotend has no profile for (only 0.4 mm today) is refused (`nozzle_not_supported`).
- **The keymap.** Each field that is not "as the installed profile has it" becomes one or more
  profile keys through `keymap-bambu-studio.toml`, written by fixed formatters and checked twice
  against the field's hard range (here and in the profile helper inside the sandbox). The limits
  of the motion group and the first layer speeds are caps: they lower and never raise an entry of
  the installed profile. Only the bed rows of the asked plate are written, and while supports are
  off the support kind, angle and where are not written (they change nothing then).
- **Measured or refused.** A key is marked measured only when a real slice changed it and the
  sliced file stated the asked value: plate, brim kind (`outer`, and `none` since 18 November
  2026), brim width and gap, skirt, both first layer speeds, the Textured PEI bed pair, part fan
  delay, auxiliary fan on the first layers and its limit, wall loops, supports off, prime tower
  off and the filament colour. Any other field is used only when its value equals what the
  installed profile already holds (nothing is written then). A different value is refused before
  the slicer runs with `setting_unmeasured`: "Hotend's <field> of <value> has never been
  measured with this slicer". So a plan with the measured recipe (level 1, 5 mm brim, Textured
  PEI) slices, while a plan that asks, for example, for another bed pair on the Cool Plate or
  the gauge's outer wall limit is refused until that setting has been measured.
- **The proof.** The sliced file must state every asked value (per extruder for a six-entry key),
  every unwritten sticking field must state the STK-1 baseline, every mapped key must lie in its
  field's hard range, and the first layer must keep to the plan in its commands: bed
  temperatures, part fan off and auxiliary fans at most the asked percentage, the part fan first
  on after `cooling.part_fan_off_layers` layers, feed rates at most the first layer speeds, the
  brim exactly when asked, one filament, no prime tower. A file that fails, or does not show
  enough to tell, is not a slice. The report holds parsed values and codes only; messages name a
  field by `Field::plain_name()`.
- **Without settings.** A real slice without print settings runs with the installed profiles as
  they are and carries the warning `print_settings_missing`. The stand-in slicer ignores the
  settings and stays labelled.
- **The editable project.** `Slicer::export_project` writes the project with the same settings,
  fills its list of changed settings with the keys the helper changed (sorted, `;`-joined, per
  preset), slices it alone and hands it over only when that slice passes the proof.

**Measured on 18 November 2026 for the bundled example and the test holder** (Linux build
02.09.01.70 in Hotend's sandbox, each with a gated test in `tests/real_bambu_studio.rs` that
repeats it):

| What | Evidence from the slicer's own output | Now |
| --- | --- | --- |
| `brim_type = no_brim` (brim kind `none`) | the calm plan on the placed box: the file's settings say `no_brim`, no layer has a `Brim` feature, the proof passes; the recipe on the same box says `outer_only` and its first layer has the brim | measured (`verified_texts`) |
| the first layer feature `Gap infill` | the bundled example's plate (cable box) has gap fill on its first layer; every such move runs at the first layer speed of the walls: 30 mm/s with the calm plan (walls 30, infill 50) and 20 mm/s with the recipe (walls 20, infill 30) | a first layer feature bound by the wall speed (`profiles/bambu-gcode-facts.toml`) |
| `M142 ... U0.5` in the body (the G-code check, `crates/hotend-printer/profiles/bambu.toml`) | U is one hundredth of the file's `additional_cooling_fan_speed`: 0.5 with 50 in the calm box and the holder by the rules alone, 0.3 with 30 in the recipe runs, as in the textured test slices | 0.5 added to the measured values (0.3, 0.7) |

Without these the plan of the bundled example (level 0, no brim) and the holder's plan by the
rules alone were refused (`setting_unmeasured`, then `proof_incomplete` for the gap fill and
`gcode_rejected` for M142). Other unmeasured values stay refused, for example the 0.25 mm
first layer of level 2 on Textured PEI.

Measured with the real program (Linux build of 02.09.01.70, 18 November 2026): the
recipe on a 20 x 30 x 10 mm box sliced to 50 layers, 3.32 g and 894 s, and the proof passed; the
box's exported project sliced alone to the same numbers and passed; its list of changed settings
equals the one in a project saved from the program's own window. The test slice of the holder
itself passes the proof of the recipe and fails the proof of a stock plan.

## Engine: print facts and plate files

What the Python engine owes the print settings: the facts of a plate and the plate file. This
section is the full text behind `docs/PROTOCOL.md` section 3.5.

### Plate files

Source: `python/hotend_engine/plate3mf.py` (the writer), `export.py` (`print_mesh`,
`write_plate_3mf`), `worker.py` (`_write_plates`); the allowlist table
`crates/hotend-slicer/tests/data/plate-shape.json`; tests
`python/tests/engine/test_plate_file.py`.

`plates/<index>/plate.3mf` of a `prepare` or `gauge` result is the file a slicer is given. Bambu
Studio gives each distinct display colour of an imported 3MF a filament slot of its own, so a
plate file that carries the viewer colour of every part (as lib3mf writes it) would split a
plate of one material over two filaments. Nobody can list everything a slicer reads from a 3MF,
so there is an allowlist, not a list of forbidden words: the engine writes plate files with a
minimal writer of its own that can only produce the shape below. (The slicer adapter checks
which parts the archive holds; a reader that holds the model part to this shape is not built.)
The 3MF core model is an open standard; this is not the slicer's project format.

`export/model.3mf` is unchanged: lib3mf, one colour per part, random UUIDs. It is for viewing
and is never given to a slicer.

**The shape.** One table, `plate-shape.json`, states it; the engine's test reads that file.

- The archive holds exactly three entries, in this order: `[Content_Types].xml`,
  `_rels/.rels` and `3D/3dmodel.model`. The first two are constant bytes (in the table as
  `archive.constant_parts`) and are compared, not parsed.
- The model part is UTF-8 (plain ASCII in fact) with the declaration
  `<?xml version="1.0" encoding="UTF-8"?>`, at most 64 MiB uncompressed, one element per
  line. No byte order mark, DOCTYPE, entity, processing instruction, CDATA, comment, character
  reference, prefixed element or text between elements.
- Elements, in the core 3MF namespace, without a prefix: `model` (`unit="millimeter"`,
  `xml:lang="en-US"`, `xmlns`) holds one `resources` and one `build`; `resources` holds one
  `object` per piece (`id`, `type="model"`, `name`); an `object` holds one `mesh` with one
  `vertices` (of `vertex` with `x`, `y`, `z`) and one `triangles` (of `triangle` with `v1`,
  `v2`, `v3`); `build` holds one `item` (`objectid`) per object, in the objects' order. No
  other element or attribute exists: no transform, component, metadata, colour, material,
  property, thumbnail or extension namespace, and the writer has no argument that could add
  one.
- Values: ids are 1, 2, 3, ... in the order of `plates[].items`. Coordinates are final bed
  coordinates in millimetres (the item's `transform` is already applied; `bbox` of the item is
  the box of its vertices within 0.01 mm), written as plain decimals with at most six
  decimals, no exponent and no minus zero, within 1000 mm of the origin. A triangle names
  three different vertices of its mesh, counter-clockwise seen from outside.

**Names.** The piece names are the only text from outside that reaches a plate file, and a
slicer copies object names into its own outputs. Every name must match
`^[a-z][a-z0-9_.]{0,63}$` and be used once; the object names of a plate are exactly the `name`
values of its `items`, in that order. Part names and copies (`lid`, `lid.2`) always pass. A
gauge piece is `<card name>.<half>`, and a card may be named with capitals, spaces and dashes:
such a `gauge` request with `threemf: true` ends as a bad request (exit code 2, error type
`bad_request`) before any folder under `plates/` exists. The message states the rule and the
piece's position and never repeats the name. With `threemf: false` no plate file is written
and the request answers as before.

**What a refusal repeats.** Three refusals of a `gauge` request name the offender by its place
and never by its text: a piece name that cannot go into a plate file (above), a card name that
is no card name ("`<kind>` card `<n>` could not be rebuilt: the card's name is not allowed;
use 1 to 64 letters, ...", from `hotend_fit.cards.base`), and a card kind that does not exist
("card `<n>` is not a known card; known: ..."). This is not true of every bad request: the
refusals of the request's shape name the field that is wrong (and with it an unknown key),
and the refusals of a pocket name, a reference name or a component id in
`hotend_fit/reference.py` and `components` still quote what was asked. Those messages go to
`result.json` as JSON text and to the worker's log; nothing of them reaches a plate file, a
file name or a fact.

**Meshes.** The triangles are made by the same mesher with the same settings as the export op's
3MF (chord error at most 0.01 mm): vertices that the faces share are merged after rounding to a
millionth of a millimetre, triangles that collapse are dropped. For shapes in the same place the
two ways give the same triangles index for index; coordinates here are decimal text with six
decimals, where lib3mf stores single precision. A mesh that is not a closed, consistently
oriented surface is still written, with the warning `plate <n>: 3mf: the mesh of '<name>' is not
a closed surface; a slicer may fill it wrongly`.

**Equal input, equal bytes.** The entries have a fixed order, date (1980-01-01), compression
level and file attributes, and nothing in the file depends on a clock, a random number or the
machine. Two runs of one request give byte-identical plate files.

**Refusals** (`plate3mf.PlateFileError`, turned into a bad request by the worker; nothing is
written for the plate): a name outside the pattern or used twice; a piece without triangles; a
coordinate that is not a number (a whole number too large to be one included), is not finite
or lies more than 1000 mm from the origin; a triangle index outside its mesh; a model part
above 64 MiB ("the plate's mesh takes more than the 64 MiB the slicer step accepts as a plate
file. Put fewer copies on the plate or print the largest piece on its own."). The size is
judged before the text exists where the counts allow it: a vertex takes at least 28 bytes
and a triangle at least 33, and a plate whose counts alone pass the limit is refused without
a line being written. Otherwise the text is made 20,000 lines at a time and stops at the
first lot after which what is written, plus the least the rest can take, passes the limit.

**For callers.** `threemf` (default true) asks for the file, and `plates[].threemf` =
`plates/<index>/plate.3mf` names it.

**Not measured.** That Bambu Studio's command line loads this minimal file is not measured: the
plate of the first real slice came from lib3mf. The gated real-slicer test settles it; if the
program wants more, exactly that is added to the writer and to the table. Hotend's fake slicer
reads the new file and gives the same estimates as for a file from lib3mf.

### Print facts and the layout

Source: `python/hotend_engine/printfacts.py` (the facts), `prepare.py` (`lay_out`,
`touched_zones`, `choose_orientation(pinned=...)`), `request.py` (`_PlateRequest`, `Keepout`),
`worker.py` (`run_prepare`, `run_gauge`, `_keepout_check`); tests
`python/tests/engine/test_printfacts.py` and `test_layout_clearance.py`; examples
`python/tests/engine/data/ring-gauge-facts.json` (a ring gauge) and
`overhang-kinds-facts.json` (one piece with a bridge, a hole ceiling and a free overhang). Both
examples are the engine's own answers, the tests compare them with what the engine measures
now, and each is a plate object for `PlateFacts::from_json`.

The engine measures and the rules decide: nothing in the engine compares a fact with a limit,
chooses a brim or names a support. Every fact is a number, a flag, `null` or one of the tokens
below. The only text is `fit_at_bed[].pair`, held to `^[a-z][a-z0-9_.-]{0,63}$` (any other
label is replaced by `pair_<n>`).

**Request.** `prepare` and `gauge` take four more fields. A request that names none of them is
answered exactly as before, without any of the result keys below (the core that reads results
refuses unknown keys, so the new keys come only to a caller that asked in the new way).

| Field | Values | Default | Meaning |
|---|---|---|---|
| `stage` | `all`, `orient`, `plate` | `all` | which part of the preparation runs |
| `clearance` | 0 to 20 mm | 0 | room kept free round every piece for its brim |
| `anchor` | `front_left`, `centre` | `front_left` | where the group of pieces lies on the plate |
| `keepouts` | at most 16 of `{id, min: [x, y], max: [x, y]}` | none | boxes on the bed that pieces and brims stay off; `id` matches `^[a-z][a-z0-9_]{0,31}$` and is used once, `max` is larger than `min`, coordinates within 1000 mm |

The lengths of a layout (`clearance`, the coordinates of a keep-out, and the older `gap` and
`margin`) are numbers and nothing else: `"6"`, `true` and `[6]` are refused, a whole number is a
number, and a minus zero is read as zero. The text of every `request.json` is read strictly
(`request.decode_request`): a key that an object names twice is refused (`"clearance": 6,
"clearance": 0` would ask for a brim's room and take it back); `NaN` and the infinities are
refused wherever they stand; and a text nested deeper than the reader follows is a bad request
like the others (exit code 2, "request.json is refused: ...").

The two passes:

1. `stage: "orient"` chooses the orientations and measures the facts. Nothing is laid out:
   `plates` and `unplaced` are empty, no `plate_fit` or `keepout` check is reported and no
   plate file or view is written. `clearance` already counts here: an orientation must fit the
   nozzle's reach with `margin + clearance` on every side.
2. `stage: "plate"` lays the same orientations out. `prepare` requires `orientations` to give
   an `up` for every part (a bad request otherwise, exit code 2) and uses each one as it is:
   it is not scored, not replaced, and `overridden` stays `null`, because the plan was made
   for exactly these facts. For `gauge`, whose pieces are always printed as drawn, `plate` is
   the same as `all`.

`stage: "all"` does both in one call.

**Result.** With one of the four fields named:

- `stage`: the stage that ran.
- `orientations[].facts` (`prepare`) and `gauges[].facts` (`gauge`, an object keyed by piece
  name): the piece object of `facts.rs`, keys in its order.
- `facts`: `{family, piece_count, total_contact_mm2, pieces}` for all pieces of the request,
  copies included, in the order of the orientations (or of the gauges' parts). It is there in
  every stage, also when a piece could not be placed.
- `plates[].facts`: the same object for the pieces of that plate, in the order of `items`.
  This is the object the rules read for the plate.
- `plates[].layout`: `{clearance, margin, anchor, offset}`; `offset` is the `[x, y]` the group
  was moved by from the front left (zeros for `front_left`).
- `plate_fit` check data gain `clearance`.
- `unplaced[].cause`: one word beside the sentence in `reason`. `clearance`: this layout
  would have placed the piece on an empty plate without its clearance, so it is the brim that
  has no room; `keepout`: the keep-out zones leave it no room even bare; `too_large`: it is
  larger than its nozzle's reach inside the margin, or taller; `no_reach`: the request gives
  its nozzle no reach. A request from before the feature gets the entries without this key.
- `keepout` check (severity `error`, no part), reported when the request has keep-outs and a
  layout: `data` is `{clearance, zones: [{id, min, max}], collisions: [{piece, plate, zone}]}`.
  It is a second look at the finished layout; a collision fails the preparation
  (`check_error`).
- `gauge` results carry `checks` when a layout was made: one `plate_fit` check for the whole
  request (severity `error`, no part; `data` is `{nozzle_id, placed, unplaced: [names],
  plates, reach, margin, clearance}`), then the `keepout` check when the request has
  keep-outs. `plate_fit` fails, and the status is `check_error`, when any gauge piece found
  no place: a gauge tests a fit with all of its pieces. A `gauge` request
  that names none of the four fields is answered with status `ok`, the warning and no `checks`.

**For the core.** A result with a non-empty `unplaced` is never a plate to plan, slice or
deliver: `prepare` and `gauge` both answer `check_error` then, and the plates that were written
lack those pieces. `cause` says which problem it is. `clearance`: the layout is asked again with
the narrower brim the rules allow, and when no brim is left to give up the plan is blocked with
`brim_does_not_fit`. `keepout`, `too_large` and `no_reach` are not solved by any brim; they are
the existing `plate_fit` failure ("make it smaller, split it, or give it to a nozzle with more
reach"). The engine never makes the clearance smaller by itself.

`family` is `PLA`, `PETG`, `TPU`, `ABS` or `ASA`, or `null`; the rules refuse `null`, which is
intended: they have no card for it. The token is the material's stated `family` (capitals and
spaces round it aside) when that is one of the five. It is never a guess: a stated family that
is none of the five gives `null` (`PETG-CF` with the id `pla-basic`), and
so does a stated family that the id contradicts, that is, the id is another family's name or
begins with it and a dash or an underscore (`tpu-95a` stated as `PLA`): one of the two is
wrong, and the bed must not be heated for the wrong one. Only a material without a stated
family is known by such an id, as everywhere in the fit library (`pla-basic`, `tpu_90a`;
`plasma` is no family).

**Piece facts.** For the direction the piece is printed in:

- `contact_mm2`, `height_mm`, `overhang_mm2`, `fit_overhang_mm2`: the measures preparation
  already took, unchanged.
- `contact_perimeter_mm`, `strip_width_mm`: the planar faces on the bed are grouped into
  contact regions (faces that share an edge are one region). A region's perimeter is the exact
  length of its boundary edges, holes included. `strip_width_mm` is the smallest
  `2 x area / perimeter` of a region, or the region's narrowest wall when that is smaller
  (below). The ring gauge (74 mm outside, 70 mm inside) gives 2.0. A piece with nothing on the
  bed has 0.
- `contact_span_mm`: the longer side of the contact's box along the bed axes.
- `slenderness`: the height over the width of the footing, at most 1e4, and 1e4 with nothing
  on the bed. The footing is as wide as `sqrt(contact)`, or as the narrowest width of the
  convex hull of everything the piece stands on when that is smaller (below).
- `inner_loops`: every hole of a contact region as `{min_width_mm, fit}`: the diameter of a
  circle, otherwise the smaller of the narrowest width of the loop's hull and
  `4 x area / perimeter`; `fit` when a declared mating face borders the loop. Listed widest
  first; past 64 the loops a fit owns are kept first, then the widest.
- `fit_at_bed`: declared mating faces that rise from the bed, one entry per pair and side,
  as `{pair, side}`. A face counts when one of its edges lies on the bed and the face itself
  does not lie flat on it. `side` is `inside` when the face looks into a hole or a
  pocket and `outside` when it looks away from the piece: for a cylinder by its normal against
  its axis, otherwise by whether the piece itself lies in front of the face. The pair is the
  fit pair's label; the pocket ring of a reference gauge is named by its pocket. A label that
  is not text, or not of the pattern, is replaced by `pair_<n>`, with numbers that no real
  pair of the piece has taken (a real `pair_1` keeps its name and the replaced one is
  `pair_2`).

  *Why flat faces are left out.* A mating face that lies flat on the bed touches it too and is
  not listed. What the rules do with an entry is all about the bottom edge of a wall: no tuning
  of the elephant foot and of the first layer (FIT-4), the bed pair of FIT-5, and the list line
  "scrape the bottom edge of the mating face flush" (FIT-3). A face flat on the bed has no
  bottom edge that grows a foot into the fit; its size across is set by the walls round it,
  which are listed when they are mating faces themselves. Listing flat faces would add that list
  line for a seat that needs no scraping and would lock the first layer on plates that have no
  fit in their lowest millimetre. The condition is in `printfacts._fit_at_bed`.
- `overhang_regions`: the connected regions of `overhang_mm2`, largest first, as
  `{area_mm2, kind, span_mm, diameter_mm, reach_mm, over_part}`; the length of the region's
  kind is a number and the other two are `null`. `[]` when nothing hangs. `null` when the
  piece has no usable tessellation, and then `overhang_over_part_mm2` equals `overhang_mm2`,
  so that nothing is played down.
- `overhang_over_part_mm2`: the area of the overhang that has the piece itself below it (a
  vertical ray from the centre of each hanging triangle). A region is `over_part` when at
  least 4 mm2 or half of it does.
- `min_wall_mm`: the thinnest wall of the piece, wherever on it, as the `min_wall` check
  samples it (the same rays, `checks/min_wall.py`, about 6000 points, the same seed, so equal
  pieces give equal numbers). The preparation runs that measurement for every part and every
  gauge piece; the check's own limit plays no part and no check result is reported. `null`
  when the piece has no closed mesh to sample, no two opposing surfaces, or a mesh of more
  than 60 000 triangles (`printfacts.MAX_WALL_TRIANGLES`: the sampling time grows with the
  square of the count, about a second at 16 000 triangles and a minute at 110 000, and a
  preparation that runs out of time gives no plate at all; the build's own `min_wall` check
  still judges such a piece). This is what rule
  MAT-1 reads for walls that do not stand on the bed: an open box with 2 mm walls on a floor
  states 2.0 and gets its third wall loop. The number is sampled on the mesh, so a round wall
  reads a little thin (the 2 mm ring reads 1.97): to the careful side.

**The two careful measures.** Both are additions to two plain measures, which the facts still
contain: `2 x contact_mm2 / contact_perimeter_mm` and `height_mm / sqrt(contact_mm2)` can be
computed from the object, and the rules crate takes the careful side of each pair
(`thin_strip_mm()`, `tipping()`). The stated `strip_width_mm` is never larger than the plain one
and the stated `slenderness` never smaller, so neither can ask for less adhesion than the plain
measure. They close two gaps of the plain measures; the limits they are compared with (3.0 mm;
2, 4 and 6) were chosen for the plain measures.

- *The narrowest wall of a contact region.* The ratio is blind to a thin wall that shares its
  region with a wide face: the 2 mm ring alone reads 2.0, with a 60 x 60 mm pad grown onto it
  10.9 (no brim, level 0, two walls), and a 40 mm block with a fin of 150 x 2 mm 8.3. So
  the outline of every region, holes included, is walked in steps of 0.5 mm (longer steps
  past 2 m of outline), and from each point a line is drawn square to the outline into the
  material; where it leaves the region is the local width. A point counts only where the line
  leaves through an edge that runs the same way within 30 degrees, the other face of a wall,
  as the `min_wall` check judges walls: the corner of a wedge and the neck beside a round
  hole are no wall. The narrowest wall is the smallest width that one unbroken 10 mm of a
  wall's face is at most as wide as, `printfacts.WALL_RUN_MM`, a judgment: a stub shorter
  than that has no corner to lift, and short necks that lie apart do not add up (a plate
  with a row of holes 2 mm from its edge keeps its ratio). It replaces the ratio when it is
  smaller by more than 1 percent. All three pieces state 2.0. A region whose outline would take
  more than 8000 steps keeps its ratio.
- *The footing of a standing wall.* `height / sqrt(contact)` reads 3.65 for a wall of
  60 x 2 mm that stands 40 mm tall, which is 20 times as tall as its footing is wide. The
  footing is therefore the smaller of `sqrt(contact)` and the narrowest width of the convex
  hull of the contact (all regions together). The hull is as wide as the stance: a ring, an L
  of two walls and four feet far apart keep the plain number, and so does every square
  or round foot. A plate that stands on a long narrow foot reads its height over its
  thickness (80 x 15 mm, 40 mm tall: 2.7 instead of 1.2).

**Overhang kinds.** An edge of a region is carried when the surface beyond it goes down from
the edge (a wall under a ceiling) or lies on the bed.

- `hole_ceiling`: the region lies on round faces whose axis lies flat and that curve round
  air (a cross hole, an arch), with both long sides carried. `diameter_mm` is twice the radius.
- `bridge`: the region is flat and level and every edge that is not carried runs in one
  direction. `span_mm` is the extent in that direction, or the narrowest width when the region
  is carried all round. A level ceiling with a hole through it, or with open sides in two
  directions, is `free`.
- `free`: anything else. `reach_mm` is the farthest horizontal distance of a point of the
  overhang from its nearest carried edge, measured square to the edge (past an end or a sharp
  corner of the carried line the larger of the two distances, not the diagonal), so that a rim
  that sticks out 2 mm all round a square foot reaches 2.0 at its corners too. Where the
  overhang ends on a curved face because the face gets steeper, the face is followed as far as
  it faces down: a bottom fillet reaches its radius (a 2 mm fillet gives 2.0),
  although only its flat part counts as area. An overhang that nothing carries reaches the
  diagonal of its box.

Past 64 regions the 63 largest are listed and the rest are merged into one `free` region with
their summed area and, as `reach_mm`, the longest box diagonal among them, which no reach
can exceed.

**Layout.** `lay_out(items, gap, margin, clearance, keepouts, anchor)`:

- Every piece is laid out as its footprint grown by `clearance` on all sides. Two pieces are
  therefore at least `2 x clearance + gap` apart, and a piece at least `clearance + margin`
  inside its nozzle's reach in X and Y.
- A grown footprint never touches a keep-out box (with 0.001 mm to spare). From the front
  left a piece slides along its row past a box, and an empty row starts behind it.
- `centre` moves the pieces of a plate together so that the box round their grown footprints
  is centred in the reach every piece shares. When the centred plain layout would touch a
  keep-out, the rows are laid round the boxes first and then moved as far as stays clear
  (both axes, else one, else not at all).
- A piece that cannot lie with this room is not placed: it is listed in `unplaced` with a
  reason that states the margin and the clearance and with its `cause` (above), its
  `plate_fit` check fails (for a gauge, the one `plate_fit` check of the request), and no
  plate file is written for it. The other pieces are laid out. The engine does not make the
  clearance smaller; asking again with a narrower brim is the rules' decision.

**Gauge texts.** Every gauge dimension's `what` of the feature cards (`peg_hole`,
`screw_boss`, `snap_lid`) ends with "; measure above the first millimetre from the bed"
(`hotend_fit.cards.base.ABOVE_THE_BED`). The `gauge` op adds the same sentence to the two
dimensions of a pocket's ring, whose inside is a mating face that rises from the bed, whatever
the form of the request; and, when the facts are computed, to every dimension of a piece
that reports a `fit_at_bed` entry, so that a card added later cannot leave it out. The
sentence is added by the op because the ring's own text is made in
`hotend_fit/reference.py`: `Pocket.gauge_set()` called directly, and the fit library's
listing of a pocket's dimensions (`capabilities.py`), still give the text without it.

**How the two halves are held together.** The names are compared with `facts.rs` by a test that
reads the source file, the engine's tests hold every piece they measure to the crate's
cross-checks (no outline shorter than a circle's, no strip wider than that circle's radius, no
contact larger than the square on its span), and `tests/engine_facts.rs` of the crate reads the
ring gauge's example file and checks its plan. `side` of the ring gauge's wall is `inside`
(the wall looks into the ring), where the crate's own test fixture writes `outside`; no rule
reads the difference today. The limits of the two careful measures (10 mm of run, 30 degrees,
the 1 percent margin) are judgments, and no print has been made with either.
