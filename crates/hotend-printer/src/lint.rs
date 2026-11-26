//! The G-code check: reads the G-code of a sliced job as hostile input and decides whether
//! it may be delivered to a printer.
//!
//! # What the check is for
//!
//! The G-code is written by a real slicer that the core runs on geometry an AI made, with
//! profiles the core put together. A person confirms the print from a short summary; nobody
//! reads forty thousand lines. The check stands in for that reading. This module judges a
//! stream of G-code bytes; [`crate::job`] is what makes those the bytes that are hashed,
//! confirmed and sent (it reads the file once and never again). The check looks for the
//! things a person confirming the summary would not see:
//!
//! 1. **A tampered or substituted file.** Whoever can write the file can write anything in
//!    it, comments and markers included. Nothing in the file is taken on trust: every line
//!    is held to the table, wherever it stands, and what the file says about itself is
//!    compared with what its commands do.
//! 2. **Custom G-code injected through a profile.** It lands in the start routine, the end
//!    routine or between layers. Those places get no free pass: a line passes only in a
//!    shape the stock routine was seen to use.
//! 3. **A slicer bug or a wrong profile that drives a nozzle outside its reach or into
//!    the plate.** Every move that lays plastic must stay inside the printable reach of
//!    the nozzle that makes it, and every other move inside that nozzle's travel box; no
//!    move names a height below the first layer. The stock start routine switches the
//!    printer's own travel limit check off, so this is the only one left.
//! 4. **Temperatures above the limit of the material, the build plate or the machine**,
//!    in the plain commands or in a vendor command that carries one; and a heater left on
//!    when the file ends.
//! 5. **Firmware and configuration commands.** The stock routines contain a few (travel
//!    limit check, stored settings, motion limits, motor current, Z trim). Those are
//!    accepted only in the exact shape and place they were seen, counted, and reported so
//!    the person confirming sees them. Every other one is refused.
//! 6. **A file that says it is for another printer, another build plate, another nozzle
//!    size or another filament** than the job declares, or that does not say. The file's
//!    statements are part of the file: this catches a wrong profile or a wrong file. It
//!    cannot catch a file whose statements were written to match; such a file is still
//!    held to every other rule, and those do not depend on what it says.
//! 7. **Plastic where no print puts it**: pushed without moving, pushed faster than the
//!    nozzle lays it down, laid into the air, or pushed through a nozzle that no command
//!    has heated.
//!
//! What it does not do: it does not simulate acceleration, cooling or collisions with what
//! is already printed, it does not know the shape that was meant (a file that prints
//! another object within every rule passes), and it cannot know what the vendor's own
//! commands do inside the firmware. For those it only guarantees that the line is one the
//! stock routine uses, with values no larger than the stock ones. Its numbers come from
//! three real slices (one for the Cool Plate, two for the Textured PEI Plate); each is
//! recorded in `profiles/bambu.toml` with what was seen and the margin that was added.
//!
//! # The rules
//!
//! **One command per line.** The first word is the command: `G`, `M` or `T` and a number,
//! with an optional sub-code (`G150.1`, `M620.11`). Every later word is a parameter, one
//! letter and its value, whatever the letter: this dialect really writes `M1006 ... M50
//! N50`, `M204 P R T`, `G28 X T300 R` and `T0 H-1`. What stays refused is what firmware
//! could run as two commands: a `G` word after the command (never a parameter here), an
//! `M` or `T` word the row does not list, a number glued to the next letter, a value
//! without a letter, a letter given twice, parentheses, control and non-ASCII characters.
//! Letters are case-insensitive; `N` line numbers and `*` checksums are stripped; a lone
//! carriage return ends a line.
//!
//! **Every line matches a row** of the table in `profiles/bambu.toml`: the command word,
//! the section, the exact set of parameter letters and a limit for every value. There is
//! no opaque parameter. A command that is not in the table is refused as unknown; one that
//! is in it but stands in another section or has other parameters is refused too.
//!
//! **Sections do not grant trust.** Bambu Studio writes marker comments around the machine
//! start and end routines. The check uses them only to choose the rows that apply, and the
//! design assumes the attacker wrote the markers: the start routine opens only before the
//! first command and closes once; the end routine opens once and runs to the end marker;
//! neither may lay plastic along a path; both are limited in length; and their rows are
//! the stock commands with the stock values, which the check would accept from anyone. A
//! file that claims the whole print is "start routine" therefore cannot print, and one
//! that claims nothing is held to the rows of the print alone. The most a forger gains is
//! the stock routine's own commands in another order, and of its moves from a position
//! the file does not give only the ones that cannot close the gap to the bed.
//!
//! **Position.** Relative moves are added up and arcs are checked over their whole path.
//! `G92` may reset the extrusion counter only: on X, Y or Z it would make every later
//! coordinate mean another place, the stock slice never does it, and it is refused. After
//! homing, a tool change or any firmware routine the
//! nozzle is somewhere the file does not say; so is it after lines that run only if the
//! printer decides so (`M622` to `M623`, `M620 S` to `M621 S`) when anything in them could
//! move it. From there an absolute move is checked by its target and plastic cannot be
//! laid along a path. A relative move from there is the one kind of move nothing can
//! check, while the stock routine has the printer's own travel limit check switched off.
//! It is accepted only in the start routine, without an `E` word, and only as the stock
//! routine makes it: a plain move no longer than the stock ones and never to a lower Z
//! (towards the bed); a guarded move (`G380`) to a lower Z only as far as a move lifted
//! the nozzle since the printer itself last placed it; and all such moves of a file
//! together no further and no more often than in the stock routine (`unplaced` in the
//! table). A positioning mode changed inside such conditional lines and not put back is
//! refused, because what follows would be read differently depending on the printer.
//! Whether a move lays plastic is decided by the extrusion counter, which is followed in both
//! extrusion modes; after conditional lines that changed it, a move with an absolute `E`
//! is taken to lay plastic. Homing is refused once plastic has been laid.
//!
//! **Height.** No move, purge line or probing height names a Z below the first layer
//! height (`min_z_mm`), and the Z trim a file may set is 0 or the value the stock routine
//! was seen to set for the build plate the job declares (`z_trim_mm` of the plate's row).
//! Plastic is laid on the plate or on plastic: at most one layer step
//! (`max_layer_step_mm`) above the highest plastic laid before.
//!
//! **Plastic.** An `E` value is accepted only once the file has said how E is read (`M82`
//! or `M83`) and never as an absolute value under `G91`, where firmware differs. Along a
//! path, a move pushes at most `max_e_per_mm` of filament per millimetre. Without a move
//! in X or Y, filament may be pushed only as far as it was pulled back before (the
//! un-retraction), and pulled back at most `max_retracted_mm` in all. The stock routine's
//! purge and prime are firmware routines and one purge-line command, each a row of the
//! start routine with the stock values, so they pass there and nowhere else. Filament is
//! pushed only while a command before it has set the printing nozzle to `min_extrude_c`
//! or more, outside lines the printer may skip. In a job with several filaments plastic
//! is laid only after the file has selected one, so that every amount is counted for a
//! filament. `F` is at most `max_feed_mm_min`.
//!
//! **Temperatures.** Every form that sets a nozzle, bed or chamber temperature is held to
//! the declared filament and the machine, and the bed also to the declared build plate. A
//! vendor value that may be a nozzle temperature is held to the same limits and reported
//! apart. The end routine may not heat, and when the file ends every heater it switched
//! on must have been switched off by a command the printer cannot skip.
//!
//! **What the file says about itself.** The printer, the build plate and the nozzle
//! sizes are read from the config comments wherever they stand; each must be there, must
//! not be stated twice with different values, and must be what the job declares. A
//! stated layer count must be the number of heights at which the commands lay plastic.
//! [`GcodeStats`] also carries the time the moves take at their commanded speeds and the
//! filament each slot pushes, which [`crate::job`] compares with the stated time and
//! amounts. A file with more lines than `max_lines` is not read to the end and fails.

use std::collections::BTreeSet;
use std::io::Read;

use serde::Serialize;

use crate::profile::{
    ArgSpec, AxisBox, BuildPlate, CommandRow, ConfigKind, FilamentLimits, GcodeClass,
    PrinterProfile, Section, VerbValue,
};

/// Longest line that holds a command, in bytes. A line that is only a comment may be
/// longer (the slicer writes whole routines into config comments).
pub const MAX_LINE_BYTES: usize = 4096;
/// Most bytes that comment-only lines may run past [`MAX_LINE_BYTES`], all together.
pub const MAX_LONG_COMMENT_BYTES: u64 = 1024 * 1024;
/// Findings kept in a report; further ones are only counted.
pub const MAX_FINDINGS: usize = 200;
/// Most filaments a job may declare.
pub const MAX_FILAMENTS: usize = 32;
/// Rounding slack when a value is compared with a limit of the table.
const EPS: f64 = 1e-9;

/// One filament of the job as the application sliced it.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredFilament {
    /// The slicer's 1-based filament number.
    pub slot: u32,
    /// Material family, for example `PLA`.
    pub material: String,
    /// Device extruder id of the nozzle that prints it.
    pub nozzle: u8,
    /// The size of that nozzle, in millimetres. It must be a size the G-code table was
    /// written from, and the file must state the same size.
    pub nozzle_diameter_mm: f64,
}

/// What the application says the sliced file is: the plate, the build plate the person
/// chose and the filament map it sliced with. The check holds the file to it.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredJob {
    /// The plate to print, 1-based.
    pub plate: u32,
    /// The build plate on the printer, by the name the slicer gives it (`Cool Plate`).
    /// The person chooses it; the file must have been sliced for it.
    pub plate_type: String,
    /// The filaments, slots 1, 2, ... without gaps.
    pub filaments: Vec<DeclaredFilament>,
}

/// Whether a finding blocks delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The job must not be delivered.
    Error,
    /// Worth showing; does not block.
    Warning,
}

/// One thing the check found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    /// Whether it blocks delivery.
    pub severity: Severity,
    /// Stable code. Errors about a line: `unknown_command`, `denied_command`,
    /// `command_out_of_place`, `unexpected_parameters`, `value_out_of_range`, `too_many`,
    /// `multiple_commands`, `ambiguous_syntax`, `malformed_number`, `text_argument`,
    /// `line_too_long`; about motion: `out_of_reach`, `too_close_to_bed`,
    /// `extrusion_outside_reach`, `extrusion_in_the_air`, `printing_outside_body`,
    /// `unverifiable_move`, `malformed_arc`, `branch`; about extrusion:
    /// `ambiguous_extrusion`, `over_extrusion`, `extrusion_in_place`,
    /// `retraction_too_long`, `cold_extrusion`; about heat and tools:
    /// `temperature_over_limit`, `heaters_left_on`, `heating_in_end_routine`,
    /// `undeclared_tool`, `undeclared_nozzle`, `filament_mismatch`; about the file:
    /// `section_marker`, `section_too_long`, `truncated`, `wrong_printer`,
    /// `plate_mismatch`, `nozzle_mismatch`, `layer_count_mismatch`, `comment_too_large`,
    /// `gcode_too_large`, `invalid_declaration`, `unknown_filament`. Warnings:
    /// `configuration_commands`, `no_moves`.
    pub code: &'static str,
    /// 1-based line number; 0 for a finding about the job as a whole.
    pub line: u64,
    /// What is wrong, for a person.
    pub message: String,
    /// The start of the offending line, printable characters only.
    pub excerpt: String,
}

/// The smallest box around everything the job prints.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Extent {
    /// Lowest X, Y, Z.
    pub min: [f64; 3],
    /// Highest X, Y, Z.
    pub max: [f64; 3],
}

/// A command that changes limits or stored configuration and was accepted because it is
/// part of the stock routine. Shown to the person who confirms the print.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigurationUse {
    /// What it changes.
    pub kind: ConfigKind,
    /// The command as written, upper case, single spaces.
    pub command: String,
    /// Where it stands.
    pub section: Section,
    /// How often it occurs there.
    pub count: u32,
    /// What it does, from the table; starts with "unknown" when nobody knows.
    pub what: String,
}

/// What the file says about itself in its leading comments. These are claims of the file,
/// not facts about it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StatedFacts {
    /// `printer_model`.
    pub printer_model: Option<String>,
    /// `printer_settings_id`.
    pub printer_settings_id: Option<String>,
    /// `curr_bed_type`: the build plate the slicer assumed.
    pub plate_type: Option<String>,
    /// `nozzle_diameter`, one value per slicer extruder, in millimetres.
    pub nozzle_diameters_mm: Vec<f64>,
}

/// How many commands each section holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SectionCounts {
    /// Whether the file has the slicer's section markers.
    pub marked: bool,
    /// Commands in the machine start routine.
    pub start: u64,
    /// Commands in the print.
    pub body: u64,
    /// Commands in the machine end routine.
    pub end: u64,
}

/// What the G-code itself does, measured by the check.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GcodeStats {
    /// Lines read.
    pub lines: u64,
    /// Commands read.
    pub commands: u64,
    /// Moves (lines, arcs and guarded moves).
    pub moves: u64,
    /// Moves that lay plastic while moving in X or Y.
    pub printing_moves: u64,
    /// Where plastic is laid.
    pub extent: Option<Extent>,
    /// Highest nozzle temperature a command sets, °C.
    pub max_nozzle_c: Option<f64>,
    /// Highest value of a vendor parameter that may be a nozzle temperature, °C.
    pub max_nozzle_reference_c: Option<f64>,
    /// Highest bed temperature commanded, °C.
    pub max_bed_c: Option<f64>,
    /// Highest chamber temperature commanded, °C.
    pub max_chamber_c: Option<f64>,
    /// Filament slots (1-based) the G-code selects.
    pub slots_used: BTreeSet<u32>,
    /// Remaining minutes stated by the first progress command that has them.
    pub first_remaining_minutes: Option<u32>,
    /// The layer count the slicer wrote in the header comment `; total layer number: N`.
    /// A claim of the file; it must equal [`Self::layers_printed`].
    pub total_layers: Option<u32>,
    /// Layers counted from the commands: the distinct heights at which plastic is laid
    /// along a path.
    pub layers_printed: u32,
    /// Seconds the moves and dwells take at their commanded speeds. Acceleration, heating
    /// and the printer's own routines come on top, so the print takes at least this long.
    pub min_seconds: f64,
    /// Filament the moves push, less what they pull back, per declared filament in slot
    /// order, in millimetres.
    pub filament_mm: Vec<f64>,
    /// The same for moves made before any filament was selected in a job with several.
    pub unassigned_filament_mm: f64,
    /// Commands per section.
    pub sections: SectionCounts,
    /// Moves made relative to a position only the printer knows.
    pub unplaced_moves: u32,
    /// Accepted commands that change limits or stored configuration.
    pub configuration: Vec<ConfigurationUse>,
    /// What the file says about itself.
    pub stated: StatedFacts,
}

/// The result of checking one G-code stream.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LintReport {
    /// The first [`MAX_FINDINGS`] findings.
    pub findings: Vec<Finding>,
    /// How many findings block delivery, including those not kept.
    pub errors: u64,
    /// How many findings are warnings, including those not kept.
    pub warnings: u64,
    /// What the G-code does.
    pub stats: GcodeStats,
}

impl LintReport {
    /// Whether the job may be delivered.
    pub fn passed(&self) -> bool {
        self.errors == 0
    }

    /// Whether a finding with this code was recorded.
    pub fn has(&self, code: &str) -> bool {
        self.findings.iter().any(|finding| finding.code == code)
    }
}

/// One parameter word of a command: its letter and whatever follows up to the next blank.
#[derive(Debug, Clone, Copy)]
struct Word<'a> {
    letter: u8,
    value: &'a str,
}

/// A refusal: the finding's code and its message.
struct Problem(&'static str, String);

/// A plain decimal number: optional sign, digits, at most one point.
fn plain_number(text: &str) -> Option<f64> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty()
        || digits.len() > 24
        || digits.bytes().filter(|b| *b == b'.').count() > 1
        || !digits.bytes().any(|b| b.is_ascii_digit())
        || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
    {
        return None;
    }
    text.parse::<f64>().ok().filter(|value| value.is_finite())
}

/// The value of a parameter as a plain number.
fn number_of(name: &str, word: &Word<'_>) -> Result<f64, Problem> {
    let letter = word.letter as char;
    if word.value.is_empty() {
        return Err(Problem(
            "malformed_number",
            format!("{name}: {letter} needs a value"),
        ));
    }
    if word.value.bytes().any(|b| b.is_ascii_alphabetic()) {
        return Err(Problem(
            "ambiguous_syntax",
            format!(
                "{name}: `{letter}{}` runs a number into a letter; firmware may read it as two words",
                word.value
            ),
        ));
    }
    plain_number(word.value).ok_or_else(|| {
        Problem(
            "malformed_number",
            format!(
                "{name}: `{letter}{}` is not a plain decimal number",
                word.value
            ),
        )
    })
}

/// The command word: `G` or `M` plus a number with an optional sub-code, or `T` plus a
/// number; leading zeros dropped (`G01` is `G1`). Returns the name and, for `T`, the number.
fn command_name(word: &str) -> Option<(String, Option<u32>)> {
    let letter = *word.as_bytes().first()?;
    let rest = &word[1..];
    let all_digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    match letter {
        b'T' => {
            if !all_digits(rest) || rest.len() > 9 {
                return None;
            }
            let number: u32 = rest.parse().ok()?;
            Some((format!("T{number}"), Some(number)))
        }
        b'G' | b'M' => {
            let (whole, sub) = match rest.split_once('.') {
                Some((whole, sub)) => (whole, Some(sub)),
                None => (rest, None),
            };
            if !all_digits(whole)
                || whole.len() > 9
                || sub.is_some_and(|sub| !all_digits(sub) || sub.len() > 3)
            {
                return None;
            }
            let number: u32 = whole.parse().ok()?;
            Some((
                match sub {
                    Some(sub) => format!("{}{number}.{sub}", letter as char),
                    None => format!("{}{number}", letter as char),
                },
                None,
            ))
        }
        _ => None,
    }
}

/// Splits the upper-cased parameters of a line into words. `Err` names what cannot be read.
fn words(code: &str) -> Result<Vec<Word<'_>>, &'static str> {
    let mut out = Vec::new();
    for token in code.split([' ', '\t']).filter(|token| !token.is_empty()) {
        let letter = token.as_bytes()[0];
        if !letter.is_ascii_uppercase() {
            return Err("a value without a parameter letter");
        }
        out.push(Word {
            letter,
            value: &token[1..],
        });
        if out.len() > 64 {
            return Err("too many parameters");
        }
    }
    Ok(out)
}

/// Bounds of an arc from `start` to `end` around `center`: `[min_x, max_x, min_y, max_y]`.
fn arc_bounds(start: [f64; 2], end: [f64; 2], center: [f64; 2], clockwise: bool) -> [f64; 4] {
    use std::f64::consts::{FRAC_PI_2, TAU};
    let radius = (start[0] - center[0]).hypot(start[1] - center[1]);
    let mut bounds = [
        start[0].min(end[0]),
        start[0].max(end[0]),
        start[1].min(end[1]),
        start[1].max(end[1]),
    ];
    // Work counter-clockwise from `from` to `to`.
    let (from, to) = if clockwise {
        (end, start)
    } else {
        (start, end)
    };
    let a0 = (from[1] - center[1]).atan2(from[0] - center[0]);
    let mut a1 = (to[1] - center[1]).atan2(to[0] - center[0]);
    if a1 <= a0 + 1e-12 {
        // Same point: a full circle.
        a1 += TAU;
    }
    // Every multiple of 90 degrees inside the sweep is an extreme of the circle.
    let mut k = (a0 / FRAC_PI_2).ceil();
    while k * FRAC_PI_2 <= a1 {
        let angle = k * FRAC_PI_2;
        let x = center[0] + radius * angle.cos();
        let y = center[1] + radius * angle.sin();
        bounds = [
            bounds[0].min(x),
            bounds[1].max(x),
            bounds[2].min(y),
            bounds[3].max(y),
        ];
        k += 1.0;
    }
    bounds
}

/// A value the file states about itself, to be compared with a closed list. A value that
/// is long or holds anything but printable ASCII becomes a word that equals nothing.
fn stated_text(value: &str) -> Option<String> {
    let value = value.trim().trim_matches('"').trim();
    if value.is_empty() {
        return None;
    }
    let plain = value.len() <= 60 && value.chars().all(|c| c.is_ascii_graphic() || c == ' ');
    Some(if plain {
        value.to_owned()
    } else {
        "(not a plain name)".to_owned()
    })
}

/// Text from the file or from the declaration for a message that a person reads. Only a
/// plain name is repeated, in quotes: at most 32 letters, digits, spaces and `-_+./`,
/// with a dot only inside a word (`0.4`, `plate_2.gcode`). Anything else, and so anything
/// that could read as a sentence, is replaced by a fixed phrase.
pub(crate) fn quoted(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let plain = !characters.is_empty()
        && characters.len() <= 32
        && characters.iter().enumerate().all(|(at, c)| match c {
            '.' => {
                at > 0
                    && characters[at - 1].is_ascii_alphanumeric()
                    && characters
                        .get(at + 1)
                        .is_some_and(char::is_ascii_alphanumeric)
            }
            ' ' | '-' | '_' | '+' | '/' => true,
            other => other.is_ascii_alphanumeric(),
        });
    if plain {
        format!("\"{text}\"")
    } else {
        "a name that is not a plain name".to_owned()
    }
}

/// What the file commands a nozzle heater to. `low` is the least it is certainly set to
/// and `high` the most it may be set to; they differ after a command whose target is not
/// known or that stands in lines only the printer decides to run.
#[derive(Debug, Clone, Copy)]
struct NozzleHeat {
    id: u8,
    low: f64,
    high: f64,
}

/// Where the reader is in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Before the first command and before any marker.
    Preamble,
    /// In the machine start routine.
    Start,
    /// In the print of a file with markers.
    Body,
    /// In the machine end routine.
    End,
    /// After the last marker.
    Done,
    /// In a file without markers: everything is the print.
    Plain,
}

/// Lines that run only if the printer decides so: between `M622` and `M623` (a flag of the
/// print job), or between `M620 S..` and `M621 S..` (a filament switch).
#[derive(Debug, Clone, Copy)]
struct Branch {
    /// A filament switch, as opposed to a flag.
    switch: bool,
    /// The filament a switch is for (index); `None` for a service tool or a flag.
    filament: Option<usize>,
    absolute: bool,
    e_relative: bool,
    /// Something inside may have moved the nozzle.
    touched: bool,
    /// Something inside changed the extrusion counter.
    e_touched: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Heat {
    Nozzle,
    NozzleReference,
    Bed,
    Chamber,
}

struct Linter<'a> {
    profile: &'a PrinterProfile,
    declared: &'a DeclaredJob,
    /// Limits of each declared filament, by index (slot - 1).
    limits: Vec<Option<&'a FilamentLimits>>,
    findings: Vec<Finding>,
    errors: u64,
    warnings: u64,
    stats: GcodeStats,
    line: u64,
    stage: Stage,
    /// Uses of each row of the table, by row index.
    row_uses: Vec<u32>,
    branch: Option<Branch>,
    /// Section length limits already reported.
    too_long_reported: [bool; 3],
    /// Stated facts that occurred twice with different values: printer model, build
    /// plate, nozzle sizes, printer settings id.
    stated_conflict: [bool; 4],
    /// The file states its layer count twice with different numbers, or not as a number.
    layers_conflict: bool,
    /// The build plate the job declares, when the printer has it.
    plate: Option<&'a BuildPlate>,
    absolute: bool,
    e_relative: bool,
    /// Whether the file has said how E values are to be read (`M82` or `M83`).
    e_mode_known: bool,
    /// Position per axis; `None` while the file does not say where the nozzle is.
    position: [Option<f64>; 3],
    /// The feed rate in force, mm/min.
    feed: Option<f64>,
    /// Filament certainly pulled back and not pushed forward again, mm: what a move
    /// without X or Y may push.
    retracted: f64,
    /// Filament that may be pulled back, lines only the printer decides to run included.
    pulled_back: f64,
    /// The highest height at which plastic has been laid along a path.
    top_printed: f64,
    /// The heights at which plastic was laid, in micrometres.
    heights: BTreeSet<i64>,
    /// What each nozzle heater of the printer is commanded to.
    nozzle_heat: Vec<NozzleHeat>,
    /// The most the bed and the chamber may be commanded to.
    bed_high: f64,
    chamber_high: f64,
    /// Per axis, the sum of the moves written from a position the file does not give,
    /// counted from where the printer itself last left the axis. 0 while the axis is
    /// known.
    unplaced_net: [f64; 3],
    /// Per axis, the distance all such moves of the file cover.
    unplaced_total: [f64; 3],
    /// Per axis, whether the file homed it (`G28`) in lines the printer cannot skip.
    /// Until then an absolute coordinate on it names a place nobody knows.
    homed: [bool; 3],
    e: f64,
    /// The extrusion counter was changed in lines that may not have run, so an absolute
    /// `E` value says nothing about whether a move lays plastic.
    e_unknown: bool,
    /// Index of the active filament (slot - 1).
    active: Option<usize>,
}

impl<'a> Linter<'a> {
    fn new(profile: &'a PrinterProfile, declared: &'a DeclaredJob) -> Self {
        let mut linter = Self {
            profile,
            declared,
            limits: Vec::new(),
            findings: Vec::new(),
            errors: 0,
            warnings: 0,
            stats: GcodeStats::default(),
            line: 0,
            stage: Stage::Preamble,
            row_uses: vec![0; profile.family().gcode.row_count],
            branch: None,
            too_long_reported: [false; 3],
            stated_conflict: [false; 4],
            layers_conflict: false,
            plate: profile.plate(&declared.plate_type),
            absolute: true,
            e_relative: false,
            e_mode_known: false,
            position: [None; 3],
            feed: None,
            retracted: 0.0,
            pulled_back: 0.0,
            top_printed: 0.0,
            heights: BTreeSet::new(),
            nozzle_heat: profile
                .nozzles()
                .iter()
                .map(|nozzle| NozzleHeat {
                    id: nozzle.device_extruder,
                    low: 0.0,
                    high: 0.0,
                })
                .collect(),
            bed_high: 0.0,
            chamber_high: 0.0,
            unplaced_net: [0.0; 3],
            unplaced_total: [0.0; 3],
            homed: [false; 3],
            e: 0.0,
            e_unknown: false,
            active: None,
        };
        linter.stats.filament_mm = vec![0.0; declared.filaments.len().min(MAX_FILAMENTS)];
        linter.check_declaration();
        linter
    }

    fn report(&mut self, severity: Severity, code: &'static str, message: String, raw: &[u8]) {
        match severity {
            Severity::Error => self.errors += 1,
            Severity::Warning => self.warnings += 1,
        }
        if self.findings.len() < MAX_FINDINGS {
            let excerpt = raw
                .iter()
                .take(80)
                .map(|&b| {
                    if b.is_ascii_graphic() || b == b' ' {
                        b as char
                    } else {
                        '?'
                    }
                })
                .collect();
            self.findings.push(Finding {
                severity,
                code,
                line: self.line,
                message,
                excerpt,
            });
        }
    }

    fn error(&mut self, code: &'static str, message: String, raw: &[u8]) {
        self.report(Severity::Error, code, message, raw);
    }

    fn refuse(&mut self, problem: Problem, raw: &[u8]) {
        self.error(problem.0, problem.1, raw);
    }

    fn check_declaration(&mut self) {
        let declared = self.declared;
        let slots_ok = !declared.filaments.is_empty()
            && declared.filaments.len() <= MAX_FILAMENTS
            && declared
                .filaments
                .iter()
                .enumerate()
                .all(|(index, filament)| filament.slot as usize == index + 1);
        if !slots_ok {
            self.error(
                "invalid_declaration",
                format!(
                    "the job must declare 1 to {MAX_FILAMENTS} filaments in slots 1, 2, ... without gaps"
                ),
                b"",
            );
        }
        if self.plate.is_none() {
            let known: Vec<&str> = self
                .profile
                .plates()
                .iter()
                .map(|plate| plate.name.as_str())
                .collect();
            self.error(
                "invalid_declaration",
                format!(
                    "the job names the build plate {}, which is not one Hotend knows for this printer ({})",
                    quoted(&declared.plate_type),
                    known.join(", ")
                ),
                b"",
            );
        }
        for filament in &declared.filaments {
            if !self
                .profile
                .slice_file()
                .knows_nozzle(filament.nozzle_diameter_mm)
            {
                let known: Vec<String> = self
                    .profile
                    .slice_file()
                    .nozzle_diameters_mm
                    .iter()
                    .map(f64::to_string)
                    .collect();
                self.error(
                    "invalid_declaration",
                    format!(
                        "filament {} is declared for a nozzle of {} mm; the commands of this printer were only measured with {} mm, so such a job cannot be checked",
                        filament.slot,
                        filament.nozzle_diameter_mm,
                        known.join(", ")
                    ),
                    b"",
                );
            }
            if self.profile.nozzle_by_device_id(filament.nozzle).is_none() {
                self.error(
                    "invalid_declaration",
                    format!(
                        "filament {} is assigned to nozzle id {}, which this printer does not have",
                        filament.slot, filament.nozzle
                    ),
                    b"",
                );
            }
            let limits = self.profile.filament_limits(&filament.material);
            if limits.is_none() {
                self.error(
                    "unknown_filament",
                    format!(
                        "filament {} is {}; Hotend has no temperature limits for it and cannot check the job",
                        filament.slot,
                        quoted(&filament.material)
                    ),
                    b"",
                );
            }
            self.limits.push(limits);
        }
    }

    /// The nozzle of the active filament, if one is active.
    fn active_nozzle(&self) -> Option<u8> {
        self.active
            .and_then(|index| self.declared.filaments.get(index))
            .map(|filament| filament.nozzle)
    }

    /// The nozzle that pushes filament now: the active filament's, or the one nozzle all
    /// filaments of the job are on. `None` when the file has not said which.
    fn printing_nozzle(&self) -> Option<u8> {
        self.active_nozzle().or_else(|| {
            let first = self.declared.filaments.first()?.nozzle;
            self.declared
                .filaments
                .iter()
                .all(|filament| filament.nozzle == first)
                .then_some(first)
        })
    }

    /// Whether the nozzle that pushes filament is certainly commanded hot enough for it.
    fn hot_enough(&self) -> bool {
        let needed = self.profile.family().gcode.limits.min_extrude_c;
        let low = |id: u8| {
            self.nozzle_heat
                .iter()
                .find(|heat| heat.id == id)
                .map_or(0.0, |heat| heat.low)
        };
        match self.printing_nozzle() {
            Some(id) => low(id) >= needed,
            None => self
                .declared
                .filaments
                .iter()
                .all(|filament| low(filament.nozzle) >= needed),
        }
    }

    /// Refuses a line that pushes filament while the nozzle is not commanded hot.
    fn refuse_cold(&mut self, name: &str, raw: &[u8]) -> bool {
        if self.hot_enough() {
            return false;
        }
        let needed = self.profile.family().gcode.limits.min_extrude_c;
        self.error(
            "cold_extrusion",
            format!(
                "{name} pushes filament, but no command before it certainly sets the nozzle that prints to {needed} C or more"
            ),
            raw,
        );
        true
    }

    /// Follows what the heaters are commanded to. The values were held to their limits
    /// before. `false` when the line heats where nothing may.
    fn note_heat(
        &mut self,
        name: &str,
        row: &CommandRow,
        params: &[Word<'_>],
        section: Section,
        raw: &[u8],
    ) -> bool {
        let conditional = self.branch.is_some();
        let named = params
            .iter()
            .find(|word| row.args.get(&word.letter) == Some(&ArgSpec::Nozzle))
            .and_then(|word| plain_number(word.value))
            .map(|id| id as u8);
        // A command with nothing but the temperature is for the nozzle that prints. With
        // any other word (the stock routine writes `A`) nobody here knows which nozzle
        // it is for.
        let target = named.or_else(|| {
            (params.len() == 1)
                .then(|| self.printing_nozzle())
                .flatten()
        });
        let mut heats = false;
        for word in params {
            let Some(spec) = row.args.get(&word.letter) else {
                continue;
            };
            let Some(value) = plain_number(word.value) else {
                continue;
            };
            match spec {
                ArgSpec::NozzleTemp => {
                    for heat in &mut self.nozzle_heat {
                        match target {
                            Some(id) if id == heat.id && !conditional => {
                                heat.low = value;
                                heat.high = value;
                            }
                            Some(id) if id != heat.id => {}
                            _ => {
                                heat.low = heat.low.min(value);
                                heat.high = heat.high.max(value);
                            }
                        }
                    }
                }
                ArgSpec::NozzleTempRef => {
                    for heat in &mut self.nozzle_heat {
                        heat.high = heat.high.max(value);
                    }
                }
                ArgSpec::BedTemp => {
                    self.bed_high = if conditional {
                        self.bed_high.max(value)
                    } else {
                        value
                    };
                }
                ArgSpec::ChamberTemp => {
                    self.chamber_high = if conditional {
                        self.chamber_high.max(value)
                    } else {
                        value
                    };
                }
                _ => continue,
            }
            heats |= value > 0.0;
        }
        if heats && section == Section::End {
            self.error(
                "heating_in_end_routine",
                format!(
                    "{name} heats in the end routine; the stock end routine only switches heaters off"
                ),
                raw,
            );
            return false;
        }
        true
    }

    /// `(reach, travel, who)` for the nozzle that would make a move now.
    fn boxes(&self) -> (AxisBox, AxisBox, String) {
        let profile = self.profile;
        if let Some(nozzle) = self
            .active_nozzle()
            .and_then(|id| profile.nozzle_by_device_id(id))
        {
            return (
                nozzle.reach,
                nozzle.travel.unwrap_or(nozzle.reach),
                format!("the {} nozzle", nozzle.role),
            );
        }
        // No filament selected yet: the move must fit every nozzle the job uses.
        let mut used: Vec<_> = self
            .declared
            .filaments
            .iter()
            .filter_map(|filament| profile.nozzle_by_device_id(filament.nozzle))
            .collect();
        if used.is_empty() {
            used = profile.nozzles().iter().collect();
        }
        let mut reach = used[0].reach;
        let mut travel = used[0].travel.unwrap_or(used[0].reach);
        for nozzle in &used[1..] {
            reach = reach.intersection(&nozzle.reach);
            travel = travel.intersection(&nozzle.travel.unwrap_or(nozzle.reach));
        }
        (reach, travel, "every nozzle of the job".to_owned())
    }

    /// The rows that apply where the reader is.
    fn section(&self) -> Section {
        match self.stage {
            Stage::Start => Section::Start,
            Stage::End => Section::End,
            Stage::Preamble | Stage::Body | Stage::Done | Stage::Plain => Section::Body,
        }
    }

    /// The printer itself moved, or may have moved, the nozzle: it is somewhere the file
    /// does not say, and moves written from there start counting anew.
    fn forget_position(&mut self) {
        self.position = [None; 3];
        self.unplaced_net = [0.0; 3];
    }

    /// The nozzle is somewhere the file does not say.
    fn lose_position(&mut self) {
        self.forget_position();
        if let Some(branch) = &mut self.branch {
            branch.touched = true;
        }
    }

    fn feed(&mut self, raw: &[u8], too_long: bool) {
        self.line += 1;
        self.stats.lines += 1;
        if too_long {
            self.error(
                "line_too_long",
                format!("a line with a command is longer than {MAX_LINE_BYTES} bytes"),
                raw,
            );
            return;
        }
        let code = match raw.iter().position(|&b| b == b';') {
            Some(at) => &raw[..at],
            None => raw,
        };
        if code.iter().all(|b| b.is_ascii_whitespace()) {
            self.comment(&raw[code.len()..], raw);
            return;
        }
        if code
            .iter()
            .any(|&b| !(b == b'\t' || (0x20..0x7f).contains(&b)))
        {
            self.error(
                "ambiguous_syntax",
                "the line has a control or non-ASCII character outside a comment".to_owned(),
                raw,
            );
            return;
        }
        if code.iter().any(|&b| b == b'(' || b == b')') {
            self.error(
                "ambiguous_syntax",
                "parentheses are a comment to some firmware and not to other".to_owned(),
                raw,
            );
            return;
        }
        // Printable ASCII only from here on.
        let Ok(text) = std::str::from_utf8(code) else {
            return;
        };
        let mut code = text.trim();
        if let Some((before, checksum)) = code.split_once('*') {
            if !checksum.trim().bytes().all(|b| b.is_ascii_digit()) {
                self.error("ambiguous_syntax", "a malformed checksum".to_owned(), raw);
                return;
            }
            code = before.trim_end();
        }
        if let Some(rest) = code.strip_prefix(['N', 'n']) {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0 && rest[digits..].starts_with([' ', '\t']) {
                code = rest[digits..].trim_start();
            }
        }
        if code.is_empty() {
            return;
        }
        self.command(code, raw);
    }

    /// A line that is only a comment: section markers and the file's leading statements.
    fn comment(&mut self, comment: &[u8], raw: &[u8]) {
        let Ok(text) = std::str::from_utf8(comment) else {
            return;
        };
        let text = text.trim_start_matches(';').trim();
        if text.is_empty() {
            return;
        }
        let profile = self.profile;
        let markers = &profile.family().gcode.sections;
        if text == markers.open {
            match self.stage {
                Stage::Preamble => {
                    self.stage = Stage::Start;
                    self.stats.sections.marked = true;
                }
                _ => self.marker_out_of_place(text, raw),
            }
        } else if text == markers.start_end {
            match self.stage {
                Stage::Start => self.enter(Stage::Body, raw),
                _ => self.marker_out_of_place(text, raw),
            }
        } else if text == markers.end_start {
            match self.stage {
                Stage::Body => self.enter(Stage::End, raw),
                _ => self.marker_out_of_place(text, raw),
            }
        } else if text == markers.close {
            match self.stage {
                Stage::End => self.enter(Stage::Done, raw),
                _ => self.marker_out_of_place(text, raw),
            }
        }
        // The slicer's layer count: a claim, compared with the layers the commands print.
        // Like the statements below it is read wherever it stands: a second count that
        // says something else cannot both be the number of layers.
        if let Some(count) = text.strip_prefix("total layer number:") {
            let count = count.trim().parse::<u32>().ok();
            match (self.stats.total_layers, count) {
                (None, Some(count)) if !self.layers_conflict => {
                    self.stats.total_layers = Some(count);
                }
                (Some(known), Some(count)) if known == count => {}
                _ => self.layers_conflict = true,
            }
        }
        // What the file says it was sliced for (`; key = value`; Bambu Studio writes these
        // into the config comments before the first command). They are read wherever
        // they stand, so a second statement that says something else is a conflict and
        // not a way to say one thing to the check and another to a reader.
        let Some((key, value)) = text.split_once(" = ") else {
            return;
        };
        match key.trim() {
            "printer_model" => {
                let value = stated_text(value);
                let stated = &mut self.stats.stated;
                Self::state(
                    &mut stated.printer_model,
                    value,
                    &mut self.stated_conflict[0],
                );
            }
            "printer_settings_id" => {
                let value = stated_text(value);
                let stated = &mut self.stats.stated;
                Self::state(
                    &mut stated.printer_settings_id,
                    value,
                    &mut self.stated_conflict[3],
                );
            }
            "curr_bed_type" => {
                let value = stated_text(value);
                let stated = &mut self.stats.stated;
                Self::state(&mut stated.plate_type, value, &mut self.stated_conflict[1]);
            }
            "nozzle_diameter" => {
                let sizes: Option<Vec<f64>> = value
                    .split(',')
                    .take(8)
                    .map(|size| plain_number(size.trim()).filter(|mm| *mm > 0.0 && *mm <= 2.0))
                    .collect();
                let sizes = sizes.unwrap_or_default();
                let known = &mut self.stats.stated.nozzle_diameters_mm;
                if self.stated_conflict[2] {
                    // Stays empty.
                } else if sizes.is_empty() || (!known.is_empty() && *known != sizes) {
                    known.clear();
                    self.stated_conflict[2] = true;
                } else {
                    *known = sizes;
                }
            }
            _ => {}
        }
    }

    fn state(slot: &mut Option<String>, value: Option<String>, conflict: &mut bool) {
        if *conflict {
            return;
        }
        match (&*slot, value) {
            (None, Some(value)) => *slot = Some(value),
            (Some(known), Some(value)) if *known != value => {
                *slot = None;
                *conflict = true;
            }
            _ => {}
        }
    }

    fn marker_out_of_place(&mut self, marker: &str, raw: &[u8]) {
        self.error(
            "section_marker",
            format!(
                "the section marker `{marker}` stands where it cannot be: each marker belongs once, in order, and the first before any command"
            ),
            raw,
        );
    }

    fn enter(&mut self, stage: Stage, raw: &[u8]) {
        if self.branch.take().is_some() {
            self.error(
                "branch",
                "a section ends inside lines started by M622 and not ended by M623".to_owned(),
                raw,
            );
            self.forget_position();
            self.e_unknown = true;
        }
        self.stage = stage;
    }

    fn command(&mut self, code: &str, raw: &[u8]) {
        self.stats.commands += 1;
        match self.stage {
            Stage::Preamble => self.stage = Stage::Plain,
            Stage::Done => {
                self.error(
                    "section_marker",
                    "a command follows the marker that ends the program".to_owned(),
                    raw,
                );
                return;
            }
            _ => {}
        }
        let section = self.section();
        let profile = self.profile;
        let gcode = &profile.family().gcode;
        let (count, limit) = match section {
            Section::Start => (
                &mut self.stats.sections.start,
                gcode.sections.start_max_commands,
            ),
            Section::Body => (&mut self.stats.sections.body, u64::MAX),
            Section::End => (
                &mut self.stats.sections.end,
                gcode.sections.end_max_commands,
            ),
        };
        *count += 1;
        let over = *count > limit;
        let reported = self.too_long_reported[section as usize];
        self.too_long_reported[section as usize] = reported || over;
        if over && !reported {
            self.error(
                "section_too_long",
                format!(
                    "{} has more than {limit} commands; the stock routine is much shorter",
                    section.phrase()
                ),
                raw,
            );
        }

        // The command word first; what follows is read as the row says.
        let head_end = code.find([' ', '\t']).unwrap_or(code.len());
        let head = code[..head_end].to_ascii_uppercase();
        let Some((name, tool)) = command_name(&head) else {
            self.error(
                "unknown_command",
                "the line does not start with a G, M or T command".to_owned(),
                raw,
            );
            return;
        };
        let rest = &code[head_end..];
        // `T<n>` is a row of its own when the number is a service tool, and the
        // selection of filament n otherwise.
        let rows = match tool {
            Some(_) if !gcode.rows_of(&name).is_empty() => gcode.rows_of(&name),
            Some(_) => gcode.rows_of("T"),
            None => gcode.rows_of(&name),
        };
        let denied = |name: &str| {
            gcode.deny.get(name).map(|denied| {
                Problem(
                    "denied_command",
                    format!("{name} is refused ({}): it {}", denied.category, denied.why),
                )
            })
        };
        if rows.is_empty() {
            let problem = denied(&name).unwrap_or_else(|| {
                Problem(
                    "unknown_command",
                    format!(
                        "{name} is not in the table of commands this printer family may be sent"
                    ),
                )
            });
            self.refuse(problem, raw);
            return;
        }
        let here: Vec<&CommandRow> = rows.iter().filter(|row| row.applies_in(section)).collect();
        if here.is_empty() {
            let seen: BTreeSet<Section> = rows
                .iter()
                .flat_map(|row| row.sections.iter().copied())
                .collect();
            let seen: Vec<&str> = seen.into_iter().map(Section::phrase).collect();
            let place = format!(
                "it is accepted only in {}, not in {}",
                seen.join(" and "),
                section.phrase()
            );
            let problem = match denied(&name) {
                Some(Problem(code, message)) => Problem(code, format!("{message}; {place}")),
                None => Problem("command_out_of_place", format!("{name}: {place}")),
            };
            self.refuse(problem, raw);
            return;
        }
        match here[0].class {
            GcodeClass::Text => {
                self.text_command(&name, here[0], rest, raw);
                return;
            }
            GcodeClass::Mask => {
                self.mask_command(&name, here[0], rest, raw);
                return;
            }
            _ => {}
        }
        let upper = rest.to_ascii_uppercase();
        let params = match words(&upper) {
            Ok(params) => params,
            Err(why) => {
                self.error("ambiguous_syntax", format!("{name}: {why}"), raw);
                return;
            }
        };
        let mut seen = [false; 26];
        for word in &params {
            // A G word is never a parameter in this dialect; an M or T word is one only
            // where a row of the command lists it.
            let second_command = match word.letter {
                b'G' => true,
                b'M' | b'T' => !rows.iter().any(|row| row.takes(word.letter)),
                _ => false,
            };
            if second_command {
                self.error(
                    "multiple_commands",
                    format!(
                        "a second command ({}{}) follows {name} on the same line",
                        word.letter as char, word.value
                    ),
                    raw,
                );
                return;
            }
        }
        for word in &params {
            let slot = &mut seen[(word.letter - b'A') as usize];
            if *slot {
                self.error(
                    "ambiguous_syntax",
                    format!("{name}: parameter {} is given twice", word.letter as char),
                    raw,
                );
                return;
            }
            *slot = true;
        }
        let mut letters: Vec<u8> = params.iter().map(|word| word.letter).collect();
        letters.sort_unstable();
        let letters: String = letters.into_iter().map(char::from).collect();
        let Some(row) = here.iter().copied().find(|row| row.accepts(&letters)) else {
            let shapes: Vec<String> = here
                .iter()
                .flat_map(|row| match &row.letters {
                    Some(letters) => vec![format!("any of {letters}")],
                    None => row
                        .forms
                        .iter()
                        .map(|form| {
                            if form.is_empty() {
                                "none".to_owned()
                            } else {
                                form.clone()
                            }
                        })
                        .collect(),
                })
                .collect();
            let shape = format!(
                "with the parameters `{letters}` it is not a form the table has for {} (it has: {})",
                section.phrase(),
                shapes.join(", ")
            );
            let problem = match denied(&name) {
                Some(Problem(code, message)) => Problem(code, format!("{message}; {shape}")),
                None => Problem("unexpected_parameters", format!("{name}: {shape}")),
            };
            self.refuse(problem, raw);
            return;
        };
        if let Err(problem) = self.check_values(&name, row, &params) {
            self.refuse(problem, raw);
            return;
        }
        if row.requires_relative && self.absolute {
            self.error(
                "unverifiable_move",
                format!("{name} is accepted only in relative positioning (G91)"),
                raw,
            );
            return;
        }
        if let Some(max) = row.max_count {
            let uses = &mut self.row_uses[row.index];
            *uses += 1;
            if *uses > max {
                self.error(
                    "too_many",
                    format!(
                        "{name} occurs more than {max} time(s) in this form; the stock routine has it {max} time(s)"
                    ),
                    raw,
                );
                return;
            }
        }
        if let Some(kind) = row.config {
            self.note_configuration(kind, &name, &params, section, row);
        }
        if !self.note_heat(&name, row, &params, section, raw) {
            return;
        }
        let errors_before = self.errors;
        let done = self.apply(&name, tool, row, &params, section, raw);
        if done && row.moves {
            self.lose_position();
        }
        if !done && self.errors == errors_before {
            // Every refusal must be a finding; this one has no better description.
            self.error(
                "unexpected_parameters",
                format!("{name}: the line cannot be read as the table describes it"),
                raw,
            );
        }
    }

    /// Holds every value the class does not read itself to the row's limit.
    fn check_values(
        &mut self,
        name: &str,
        row: &CommandRow,
        params: &[Word<'_>],
    ) -> Result<(), Problem> {
        let profile = self.profile;
        let outside = |word: &Word<'_>, allowed: String| {
            Problem(
                "value_out_of_range",
                format!(
                    "{name}: {}{} is not a value the table accepts for this command ({allowed})",
                    word.letter as char, word.value
                ),
            )
        };
        // A nozzle named on the line decides whose limits a temperature is held to.
        let mut named_nozzle = None;
        for word in params {
            if row.args.get(&word.letter) != Some(&ArgSpec::Nozzle) {
                continue;
            }
            let id = number_of(name, word)?;
            let nozzle = (0.0..=255.0)
                .contains(&id)
                .then_some(id as u8)
                .filter(|nozzle| f64::from(*nozzle) == id)
                .and_then(|nozzle| profile.nozzle_by_device_id(nozzle));
            match nozzle {
                Some(nozzle) => named_nozzle = Some(nozzle.device_extruder),
                None => {
                    return Err(Problem(
                        "undeclared_nozzle",
                        format!(
                            "{name}: {}{} names a nozzle this printer does not have",
                            word.letter as char, word.value
                        ),
                    ));
                }
            }
        }
        for word in params {
            let Some(spec) = row.args.get(&word.letter) else {
                continue;
            };
            match spec {
                ArgSpec::Nozzle => {}
                ArgSpec::Flag => {
                    if !word.value.is_empty() {
                        return Err(outside(word, "the letter alone".to_owned()));
                    }
                }
                ArgSpec::Range(low, high) => {
                    let value = number_of(name, word)?;
                    if value < low - EPS || value > high + EPS {
                        return Err(outside(word, format!("{low} to {high}")));
                    }
                }
                ArgSpec::OneOf(values) => {
                    let value = number_of(name, word)?;
                    if !values.iter().any(|allowed| (allowed - value).abs() <= EPS) {
                        let list: Vec<String> = values.iter().map(f64::to_string).collect();
                        return Err(outside(word, format!("one of {}", list.join(", "))));
                    }
                }
                ArgSpec::UpTo(max) => {
                    let value = number_of(name, word)?;
                    if value <= 0.0 || value > max + EPS {
                        return Err(outside(word, format!("above 0, at most {max}")));
                    }
                }
                ArgSpec::NozzleTemp => {
                    let value = number_of(name, word)?;
                    self.temperature(name, Heat::Nozzle, value, named_nozzle)?;
                }
                ArgSpec::NozzleTempRef => {
                    let value = number_of(name, word)?;
                    self.temperature(name, Heat::NozzleReference, value, None)?;
                }
                ArgSpec::BedTemp => {
                    let value = number_of(name, word)?;
                    self.temperature(name, Heat::Bed, value, None)?;
                }
                ArgSpec::ChamberTemp => {
                    let value = number_of(name, word)?;
                    self.temperature(name, Heat::Chamber, value, None)?;
                }
                ArgSpec::FilamentA => {
                    let index = word
                        .value
                        .strip_suffix('A')
                        .filter(|digits| {
                            !digits.is_empty()
                                && digits.len() <= 9
                                && digits.bytes().all(|b| b.is_ascii_digit())
                        })
                        .and_then(|digits| digits.parse::<usize>().ok());
                    match index {
                        Some(index) if index < self.declared.filaments.len() => {}
                        Some(index) => {
                            return Err(Problem(
                                "undeclared_tool",
                                format!(
                                    "{name} names filament {}, but the job declares {} filament(s)",
                                    index + 1,
                                    self.declared.filaments.len()
                                ),
                            ));
                        }
                        None => {
                            return Err(outside(
                                word,
                                "a filament number followed by A".to_owned(),
                            ));
                        }
                    }
                }
                ArgSpec::PlateZTrim => {
                    let value = number_of(name, word)?;
                    let trim = self.plate.and_then(|plate| plate.z_trim_mm);
                    let fits =
                        value.abs() <= EPS || trim.is_some_and(|trim| (trim - value).abs() <= EPS);
                    if !fits {
                        let allowed = match (self.plate, trim) {
                            (Some(plate), Some(trim)) => {
                                format!("0, or {trim} for the {}", plate.name)
                            }
                            _ => "0; no trim is known for the build plate of this job".to_owned(),
                        };
                        return Err(outside(word, allowed));
                    }
                }
                ArgSpec::ServiceTool => {
                    let value = number_of(name, word)?;
                    let known = profile
                        .family()
                        .gcode
                        .service_tools
                        .iter()
                        .any(|tool| f64::from(*tool) == value);
                    if !known {
                        return Err(Problem(
                            "undeclared_tool",
                            format!(
                                "{name}: {}{} is neither a filament of the job nor a tool number the stock routine uses",
                                word.letter as char, word.value
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Holds one temperature to the filament's and the machine's limits and records it.
    fn temperature(
        &mut self,
        name: &str,
        heat: Heat,
        value: f64,
        named_nozzle: Option<u8>,
    ) -> Result<(), Problem> {
        let profile = self.profile;
        let machine = *profile.limits();
        let limit_of = |limits: &FilamentLimits| match heat {
            Heat::Nozzle | Heat::NozzleReference => limits.nozzle_max_c,
            Heat::Bed => limits.bed_max_c,
            Heat::Chamber => limits.chamber_max_c,
        };
        let (machine_limit, what) = match heat {
            Heat::Nozzle => (machine.nozzle_max_c, "sets the nozzle to"),
            Heat::NozzleReference => (
                machine.nozzle_max_c,
                "carries what may be a nozzle temperature,",
            ),
            Heat::Bed => (machine.bed_max_c, "sets the bed to"),
            Heat::Chamber => (machine.chamber_max_c, "sets the chamber to"),
        };
        let highest = |indexes: &mut dyn Iterator<Item = usize>| {
            indexes
                .filter_map(|index| self.limits.get(index).copied().flatten())
                .map(limit_of)
                .fold(0.0, f64::max)
        };
        // A nozzle named on the line is held to the filaments on that nozzle; the nozzle
        // of the active filament to that filament; anything else (bed, chamber, nothing
        // selected yet, a value that only may be a temperature) to the highest limit among
        // the job's filaments.
        let (filament_limit, whose) = match (heat, named_nozzle) {
            (Heat::Nozzle, Some(nozzle)) => {
                let mut on_nozzle = self
                    .declared
                    .filaments
                    .iter()
                    .enumerate()
                    .filter(|(_, filament)| filament.nozzle == nozzle)
                    .map(|(index, _)| index)
                    .peekable();
                if on_nozzle.peek().is_none() {
                    if value > 0.0 {
                        let role = profile
                            .nozzle_by_device_id(nozzle)
                            .map_or("that", |nozzle| nozzle.role.as_str());
                        return Err(Problem(
                            "undeclared_nozzle",
                            format!(
                                "{name} heats the {role} nozzle (id {nozzle}) to {value} C, but no filament of the job is on it"
                            ),
                        ));
                    }
                    (0.0, "an unused nozzle".to_owned())
                } else {
                    (
                        highest(&mut on_nozzle),
                        "the filaments on that nozzle".to_owned(),
                    )
                }
            }
            (Heat::Nozzle, None) => match self
                .active
                .and_then(|index| self.limits.get(index).copied().flatten())
            {
                Some(limits) => (limit_of(limits), limits.family.clone()),
                None => (
                    highest(&mut (0..self.limits.len())),
                    "the job's filaments".to_owned(),
                ),
            },
            _ => (
                highest(&mut (0..self.limits.len())),
                "the job's filaments".to_owned(),
            ),
        };
        // The bed is also held to the build plate the person chose, whatever the file
        // says it was sliced for.
        let plate = self.plate.filter(|_| heat == Heat::Bed);
        let plate_limit = plate.map_or(f64::INFINITY, |plate| plate.bed_max_c);
        if value < 0.0 || value > filament_limit || value > machine_limit || value > plate_limit {
            let limit = filament_limit.min(machine_limit).min(plate_limit);
            let on = match plate {
                Some(plate) if plate_limit <= limit => format!(" on the {}", plate.name),
                _ => String::new(),
            };
            return Err(Problem(
                "temperature_over_limit",
                format!(
                    "{name} {what} {value} C; the limit for {whose}{on} on this printer is {limit} C"
                ),
            ));
        }
        let record = match heat {
            Heat::Nozzle => &mut self.stats.max_nozzle_c,
            Heat::NozzleReference => &mut self.stats.max_nozzle_reference_c,
            Heat::Bed => &mut self.stats.max_bed_c,
            Heat::Chamber => &mut self.stats.max_chamber_c,
        };
        *record = Some(record.map_or(value, |seen| seen.max(value)));
        Ok(())
    }

    fn note_configuration(
        &mut self,
        kind: ConfigKind,
        name: &str,
        params: &[Word<'_>],
        section: Section,
        row: &CommandRow,
    ) {
        let mut command = name.to_owned();
        for word in params {
            command.push(' ');
            command.push(word.letter as char);
            command.push_str(word.value);
        }
        let uses = &mut self.stats.configuration;
        let known = uses
            .iter()
            .position(|known| known.command == command && known.section == section);
        match known {
            Some(at) => uses[at].count += 1,
            // Bounded by the rows' own limits; the cap only guards the table.
            None if uses.len() < 64 => uses.push(ConfigurationUse {
                kind,
                command,
                section,
                count: 1,
                what: row.what.clone(),
            }),
            None => {}
        }
    }

    /// Does what the row's class says. `false` when the line was refused.
    fn apply(
        &mut self,
        name: &str,
        tool: Option<u32>,
        row: &CommandRow,
        params: &[Word<'_>],
        section: Section,
        raw: &[u8],
    ) -> bool {
        match row.class {
            GcodeClass::Move => self.motion(name, params, None, section, raw),
            GcodeClass::ArcCw => self.motion(name, params, Some(true), section, raw),
            GcodeClass::ArcCcw => self.motion(name, params, Some(false), section, raw),
            GcodeClass::Probe => self.probe(name, params, section, raw),
            GcodeClass::Home => {
                if self.stats.printing_moves > 0 {
                    self.error(
                        "command_out_of_place",
                        format!(
                            "{name} homes the printer after plastic has been laid; the nozzle or the bed could hit the print"
                        ),
                        raw,
                    );
                    return false;
                }
                let mut named = Vec::new();
                for word in params {
                    let Some(axis) = b"XYZ".iter().position(|axis| *axis == word.letter) else {
                        continue;
                    };
                    if !word.value.is_empty() {
                        self.error(
                            "unexpected_parameters",
                            format!(
                                "{name}: {} names an axis and takes no value",
                                word.letter as char
                            ),
                            raw,
                        );
                        return false;
                    }
                    named.push(axis);
                }
                // Measured: the stock start routine homes with `G28 X T300 R` and then moves
                // Y to absolute places before it homes Z; it never names Y. There, and only
                // in that form (X with T and R), homing X counts as homing Y too (Y's
                // position is not known after it). Everywhere else an axis is homed only by
                // a `G28` that names it or names none.
                let has = |letter: u8| params.iter().any(|word| word.letter == letter);
                let stock_x_home = named == [0] && has(b'T') && has(b'R');
                if section == Section::Start && stock_x_home {
                    named.push(1);
                }
                // Z is homed where the bed probe meets the bed, at an X and Y the file must
                // know: the stock routine homes X (and with it Y) before it homes Z alone.
                if named.contains(&2)
                    && !(named.contains(&0) && named.contains(&1))
                    && !(self.homed[0] && self.homed[1])
                {
                    self.error(
                        "command_out_of_place",
                        format!(
                            "{name} homes Z before X and Y are homed; the probe would touch the bed at a place the file does not know"
                        ),
                        raw,
                    );
                    return false;
                }
                for axis in 0..3 {
                    if named.is_empty() || named.contains(&axis) {
                        self.position[axis] = None;
                        self.unplaced_net[axis] = 0.0;
                        // Homing in lines the printer may skip homes nothing for certain.
                        if self.branch.is_none() {
                            self.homed[axis] = true;
                        }
                    }
                }
                if let Some(branch) = &mut self.branch {
                    branch.touched = true;
                }
                true
            }
            GcodeClass::Absolute => {
                self.absolute = true;
                true
            }
            GcodeClass::Relative => {
                self.absolute = false;
                true
            }
            GcodeClass::EAbsolute => {
                self.e_relative = false;
                self.e_mode_known = true;
                true
            }
            GcodeClass::ERelative => {
                self.e_relative = true;
                self.e_mode_known = true;
                true
            }
            GcodeClass::Dwell => {
                // `P` is milliseconds, `S` seconds; the row limits both.
                let waited = value_of(params, b'P').map(|ms| ms / 1000.0).unwrap_or(0.0)
                    + value_of(params, b'S').unwrap_or(0.0);
                self.stats.min_seconds += waited.max(0.0);
                true
            }
            GcodeClass::SetPosition => self.set_position(name, params, raw),
            GcodeClass::Tool => self.select_tool(name, tool, raw),
            GcodeClass::Progress => {
                if self.stats.first_remaining_minutes.is_none()
                    && let Some(minutes) = value_of(params, b'R')
                {
                    self.stats.first_remaining_minutes = Some(minutes.round() as u32);
                }
                true
            }
            GcodeClass::Area => self.area(name, params, raw),
            GcodeClass::Line => self.purge_line(name, params, raw),
            GcodeClass::BranchStart | GcodeClass::SwitchStart => {
                if self.branch.is_some() {
                    self.error(
                        "branch",
                        format!("{name} starts conditional lines inside conditional lines"),
                        raw,
                    );
                    return false;
                }
                let switch = row.class == GcodeClass::SwitchStart;
                // The filament a switch is for; its limit was checked with the values.
                let filament = params
                    .iter()
                    .find(|word| switch && word.letter == b'S')
                    .and_then(|word| word.value.strip_suffix('A'))
                    .and_then(|index| index.parse::<usize>().ok());
                self.branch = Some(Branch {
                    switch,
                    filament,
                    absolute: self.absolute,
                    e_relative: self.e_relative,
                    touched: false,
                    e_touched: false,
                });
                true
            }
            GcodeClass::BranchEnd | GcodeClass::SwitchEnd => {
                let switch = row.class == GcodeClass::SwitchEnd;
                let Some(branch) = self.branch.take().filter(|open| open.switch == switch) else {
                    self.forget_position();
                    self.error(
                        "branch",
                        format!("{name} ends conditional lines that it did not start"),
                        raw,
                    );
                    return false;
                };
                if branch.touched {
                    // Whether those lines ran is the printer's decision.
                    self.forget_position();
                }
                if branch.e_touched {
                    self.e_unknown = true;
                }
                if branch.absolute != self.absolute || branch.e_relative != self.e_relative {
                    self.absolute = branch.absolute;
                    self.e_relative = branch.e_relative;
                    self.error(
                        "branch",
                        format!(
                            "the positioning mode changes inside the conditional lines ended by {name}; what follows would depend on a printer flag"
                        ),
                        raw,
                    );
                    return false;
                }
                true
            }
            GcodeClass::ServiceTool | GcodeClass::Opaque => true,
            // Handled before the parameters are split.
            GcodeClass::Text | GcodeClass::Mask => true,
        }
    }

    fn select_tool(&mut self, name: &str, tool: Option<u32>, raw: &[u8]) -> bool {
        let Some(tool) = tool else {
            return false;
        };
        // Inside conditional lines only the filament a switch is for may be selected.
        if let Some(branch) = &self.branch
            && branch.filament != Some(tool as usize)
        {
            self.error(
                "branch",
                format!(
                    "{name} selects a filament inside conditional lines that are not the switch to it; which filament prints would depend on the printer"
                ),
                raw,
            );
            return false;
        }
        if (tool as usize) < self.declared.filaments.len() {
            self.active = Some(tool as usize);
            self.stats.slots_used.insert(tool + 1);
            true
        } else {
            self.error(
                "undeclared_tool",
                format!(
                    "{name} selects filament {}, but the job declares {} filament(s)",
                    u64::from(tool) + 1,
                    self.declared.filaments.len()
                ),
                raw,
            );
            false
        }
    }

    fn text_command(&mut self, name: &str, row: &CommandRow, rest: &str, raw: &[u8]) {
        let plain = rest.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b' ' | b'\t' | b'_' | b':' | b'.' | b'-')
        });
        let tokens: Vec<&str> = rest
            .split([' ', '\t', ':'])
            .filter(|token| !token.is_empty())
            .collect();
        let hides_command = tokens.iter().any(|token| {
            matches!(token.as_bytes()[0].to_ascii_uppercase(), b'G' | b'M' | b'T')
                && plain_number(&token[1..]).is_some()
        });
        if !plain || hides_command {
            self.error(
                "text_argument",
                format!("{name}: the text holds something other than names and numbers"),
                raw,
            );
            return;
        }
        let kind = match tokens.as_slice() {
            [verb, _value] => row.verbs.get(*verb),
            _ => None,
        };
        let Some(kind) = kind else {
            let verbs: Vec<&str> = row.verbs.keys().map(String::as_str).collect();
            self.error(
                "text_argument",
                format!(
                    "{name} is accepted only as one of {} with one value",
                    verbs.join(", ")
                ),
                raw,
            );
            return;
        };
        let value = tokens[1];
        let fits = match kind {
            VerbValue::Number => value.len() <= 3 && value.bytes().all(|b| b.is_ascii_digit()),
            VerbValue::Name => {
                value.len() <= 64
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            }
            VerbValue::FilamentType => true,
        };
        if !fits {
            self.error(
                "text_argument",
                format!("{name} {}: `{value}` is not a value it takes", tokens[0]),
                raw,
            );
            return;
        }
        if *kind == VerbValue::FilamentType && !value.eq_ignore_ascii_case("UNKNOWN") {
            let family = |material: &str| {
                self.profile
                    .filament_limits(material)
                    .map(|limits| limits.family.as_str())
            };
            let stated = family(value);
            let declared = self.declared.filaments.iter().any(|filament| {
                (stated.is_some() && family(&filament.material) == stated)
                    || filament.material.eq_ignore_ascii_case(value)
            });
            if !declared {
                self.error(
                    "filament_mismatch",
                    format!(
                        "{name} tells the printer the filament is `{value}`, which is not a filament of the job"
                    ),
                    raw,
                );
            }
        }
    }

    fn mask_command(&mut self, name: &str, row: &CommandRow, rest: &str, raw: &[u8]) {
        let word = rest.trim();
        let length = row.mask_chars.unwrap_or(0);
        let bytes = word.as_bytes();
        let padding = bytes.iter().rev().take_while(|b| **b == b'=').count();
        let ok = bytes.len() == length
            && padding <= 2
            && bytes[..bytes.len() - padding]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/'));
        if !ok {
            self.error(
                "text_argument",
                format!("{name} takes one base64 word of {length} characters and nothing else"),
                raw,
            );
        }
    }

    /// `G92`: sets the extrusion counter. The table has no row for a reset of X, Y or Z,
    /// so the line never gets here with one.
    fn set_position(&mut self, name: &str, params: &[Word<'_>], raw: &[u8]) -> bool {
        if params.is_empty() {
            self.error(
                "ambiguous_syntax",
                format!("{name} without parameters means different things to different firmware"),
                raw,
            );
            return false;
        }
        for word in params {
            let value = match number_of(name, word) {
                Ok(value) => value,
                Err(problem) => {
                    self.refuse(problem, raw);
                    return false;
                }
            };
            if word.letter != b'E' {
                self.error(
                    "unverifiable_move",
                    format!(
                        "{name} redefines {}; only the extrusion counter may be reset",
                        word.letter as char
                    ),
                    raw,
                );
                return false;
            }
            self.e = value;
            match &mut self.branch {
                Some(branch) => branch.e_touched = true,
                None => self.e_unknown = false,
            }
        }
        true
    }

    /// Counts a move made from a position only the printer knows. `false` when the table
    /// does not allow it: here, this far, in this direction, or on top of the ones before.
    ///
    /// Nothing can check where such a move ends, so it is held to the direction and the
    /// distances of the stock routine. On each axis the moves are added up from where the
    /// printer itself last left the axis, and the sum must stay within the table's `net`
    /// range. For Z that range starts at 0: such a move never brings the nozzle closer to
    /// the bed than the printer left it.
    fn unplaced(
        &mut self,
        name: &str,
        moved: &[(usize, f64)],
        guarded: bool,
        section: Section,
        raw: &[u8],
    ) -> bool {
        let profile = self.profile;
        let rules = &profile.family().gcode.unplaced;
        let axis_name = |axis: usize| b"XYZ"[axis] as char;
        if !rules.sections.contains(&section) {
            let places: Vec<&str> = rules.sections.iter().map(|s| s.phrase()).collect();
            let (axis, by) = moved[0];
            self.error(
                "unverifiable_move",
                format!(
                    "{name} moves {} by {by} mm from a position the file does not give; that is accepted only in {}",
                    axis_name(axis),
                    places.join(" and ")
                ),
                raw,
            );
            return false;
        }
        let step = if guarded {
            &rules.guarded
        } else {
            &rules.plain
        };
        let mut net = self.unplaced_net;
        let mut total = self.unplaced_total;
        for &(axis, by) in moved {
            let [low, high] = step[axis];
            if by < low - EPS || by > high + EPS {
                self.error(
                    "unverifiable_move",
                    format!(
                        "{name} moves {} by {by} mm from a position the file does not give; one such {} move may cover {low} to {high} mm on that axis",
                        axis_name(axis),
                        if guarded { "guarded" } else { "plain" }
                    ),
                    raw,
                );
                return false;
            }
            net[axis] += by;
            total[axis] += by.abs();
            let [low, high] = rules.net[axis];
            if net[axis] < low - EPS || net[axis] > high + EPS {
                self.error(
                    "unverifiable_move",
                    format!(
                        "{name} moves {} by {by} mm from a position the file does not give; with the moves before it that is {:.3} mm from where the printer left the nozzle, and only {low} to {high} mm is accepted{}",
                        axis_name(axis),
                        net[axis],
                        if axis == 2 && net[axis] < 0.0 {
                            " (the nozzle would come closer to the bed, and nothing can check how close)"
                        } else {
                            ""
                        }
                    ),
                    raw,
                );
                return false;
            }
        }
        let over = (0..3).find(|axis| total[*axis] > rules.total_mm[*axis] + EPS);
        if let Some(axis) = over {
            self.error(
                "too_many",
                format!(
                    "the moves from a position the file does not give cover more than {} mm in {}; the stock routine covers no more",
                    rules.total_mm[axis],
                    axis_name(axis)
                ),
                raw,
            );
            return false;
        }
        if self.stats.unplaced_moves >= rules.max_count {
            self.error(
                "too_many",
                format!(
                    "more than {} moves start from a position the file does not give",
                    rules.max_count
                ),
                raw,
            );
            return false;
        }
        self.stats.unplaced_moves += 1;
        self.unplaced_net = net;
        self.unplaced_total = total;
        true
    }

    /// A guarded move: relative, short, and its end is not known.
    fn probe(&mut self, name: &str, params: &[Word<'_>], section: Section, raw: &[u8]) -> bool {
        let mut moved = Vec::new();
        for word in params {
            let Some(axis) = b"XYZ".iter().position(|axis| *axis == word.letter) else {
                continue;
            };
            match number_of(name, word) {
                Ok(by) => moved.push((axis, by)),
                Err(problem) => {
                    self.refuse(problem, raw);
                    return false;
                }
            }
        }
        if moved.is_empty() {
            return false;
        }
        // From a position the file gives, the move must also end inside the travel box
        // and above the floor.
        let (_, travel, who) = self.boxes();
        let floor = self.profile.family().gcode.limits.min_z_mm;
        for &(axis, by) in &moved {
            if axis == 2
                && let Some(from) = self.position[axis]
                && from + by < floor - EPS
            {
                self.too_close(name, from + by, raw);
                return false;
            }
            if let Some(from) = self.position[axis]
                && !travel.contains_axis(axis, from + by)
            {
                let [low, high] = [travel.x, travel.y, travel.z][axis];
                self.error(
                    "out_of_reach",
                    format!(
                        "{name} moves {} to {:.3} mm; {who} can go from {low} to {high} mm",
                        b"XYZ"[axis] as char,
                        from + by
                    ),
                    raw,
                );
                return false;
            }
        }
        // It may stop on the way, so wherever it started it is counted as a move from a
        // position that is not known, and it ends at one.
        if !self.unplaced(name, &moved, true, section, raw) {
            return false;
        }
        for (axis, _) in moved {
            self.position[axis] = None;
        }
        if let Some(branch) = &mut self.branch {
            branch.touched = true;
        }
        self.stats.moves += 1;
        true
    }

    /// `true` when every axis in `axes` was homed; otherwise refuses the line and returns
    /// `false`. Before homing, the printer's coordinates are wherever its axes happened to
    /// stop, so an absolute place on such an axis is not a place the check can hold to the
    /// reach of the nozzle.
    fn unhomed_refused(&mut self, name: &str, axes: &[usize], raw: &[u8]) -> bool {
        let Some(&axis) = axes.iter().find(|axis| !self.homed[**axis]) else {
            return true;
        };
        self.error(
            "unverifiable_move",
            format!(
                "{name} names an absolute {} position before the file homed that axis (G28); where it goes is not known",
                b"XYZ"[axis] as char
            ),
            raw,
        );
        false
    }

    /// Refuses a height below the floor of the table.
    fn too_close(&mut self, name: &str, z: f64, raw: &[u8]) {
        let floor = self.profile.family().gcode.limits.min_z_mm;
        self.error(
            "too_close_to_bed",
            format!(
                "{name} takes the nozzle to Z {z:.3} mm; nothing in a stock slice goes below {floor} mm, the first layer, and lower is a nozzle on the plate"
            ),
            raw,
        );
    }

    /// A probing area: corner `X Y`, size `I J`, all of it inside the travel box.
    fn area(&mut self, name: &str, params: &[Word<'_>], raw: &[u8]) -> bool {
        let read = |letter: u8| {
            params
                .iter()
                .find(|word| word.letter == letter)
                .map(|word| number_of(name, word))
        };
        let mut corner = [0.0; 4];
        for (slot, letter) in corner.iter_mut().zip(*b"XYIJ") {
            match read(letter) {
                Some(Ok(value)) => *slot = value,
                Some(Err(problem)) => {
                    self.refuse(problem, raw);
                    return false;
                }
                None => return false,
            }
        }
        let [x, y, i, j] = corner;
        if !self.unhomed_refused(name, &[0, 1], raw) {
            return false;
        }
        let (_, travel, who) = self.boxes();
        let inside = i > 0.0
            && j > 0.0
            && travel.contains_axis(0, x)
            && travel.contains_axis(0, x + i)
            && travel.contains_axis(1, y)
            && travel.contains_axis(1, y + j);
        if !inside {
            self.error(
                "out_of_reach",
                format!(
                    "{name} probes X {x} to {}, Y {y} to {} mm, outside what {who} can reach",
                    x + i,
                    y + j
                ),
                raw,
            );
            return false;
        }
        true
    }

    /// A purge line: start `X Y Z`, length `L`. The file does not say which way it runs,
    /// so it must fit along X in both directions.
    fn purge_line(&mut self, name: &str, params: &[Word<'_>], raw: &[u8]) -> bool {
        let read = |letter: u8| {
            params
                .iter()
                .find(|word| word.letter == letter)
                .map(|word| number_of(name, word))
        };
        let mut start = [0.0; 4];
        for (slot, letter) in start.iter_mut().zip(*b"XYZL") {
            match read(letter) {
                Some(Ok(value)) => *slot = value,
                Some(Err(problem)) => {
                    self.refuse(problem, raw);
                    return false;
                }
                None => return false,
            }
        }
        let [x, y, z, length] = start;
        if !self.unhomed_refused(name, &[0, 1, 2], raw) {
            return false;
        }
        if z < self.profile.family().gcode.limits.min_z_mm - EPS {
            self.too_close(name, z, raw);
            return false;
        }
        // The line pushes filament (its `E` is held to the stock value by the row).
        if self.refuse_cold(name, raw) {
            return false;
        }
        let (_, travel, who) = self.boxes();
        let inside = length > 0.0
            && travel.contains_axis(0, x - length)
            && travel.contains_axis(0, x + length)
            && travel.contains_axis(1, y)
            && travel.contains_axis(2, z);
        if !inside {
            self.error(
                "out_of_reach",
                format!(
                    "{name} draws a line of {length} mm from X {x}, Y {y}, Z {z} mm, outside where {who} may travel"
                ),
                raw,
            );
            return false;
        }
        true
    }

    fn motion(
        &mut self,
        name: &str,
        params: &[Word<'_>],
        arc: Option<bool>,
        section: Section,
        raw: &[u8],
    ) -> bool {
        let profile = self.profile;
        let limits = &profile.family().gcode.limits;
        let mut given = [None; 3];
        let (mut e, mut i, mut j, mut r) = (None, None, None, None);
        let mut whole_turn = false;
        let mut feed = self.feed;
        for word in params {
            let value = match number_of(name, word) {
                Ok(value) => value,
                Err(problem) => {
                    self.refuse(problem, raw);
                    return false;
                }
            };
            let slot = match word.letter {
                b'X' => &mut given[0],
                b'Y' => &mut given[1],
                b'Z' => &mut given[2],
                b'E' => &mut e,
                b'I' => &mut i,
                b'J' => &mut j,
                b'R' => &mut r,
                b'F' => {
                    if value <= 0.0 {
                        self.error(
                            "malformed_number",
                            format!("{name}: F needs a positive speed"),
                            raw,
                        );
                        return false;
                    }
                    if value > limits.max_feed_mm_min + EPS {
                        self.error(
                            "value_out_of_range",
                            format!(
                                "{name}: F{} is faster than any move of a stock slice (at most {} mm/min)",
                                word.value, limits.max_feed_mm_min
                            ),
                            raw,
                        );
                        return false;
                    }
                    feed = Some(value);
                    continue;
                }
                b'P' => {
                    // Seen with 1 only: one whole turn.
                    if (value - 1.0).abs() > EPS {
                        self.error(
                            "value_out_of_range",
                            format!(
                                "{name}: P{} is not a value the table accepts (1)",
                                word.value
                            ),
                            raw,
                        );
                        return false;
                    }
                    whole_turn = true;
                    continue;
                }
                _ => continue,
            };
            *slot = Some(value);
        }
        if e.is_some() {
            // How an E value is read must not depend on the firmware's dialect. The stock
            // slice says `M83` before its first E and writes E in absolute positioning
            // only; some firmware makes E relative with `G91`, other firmware does not.
            let unclear = if !self.e_mode_known {
                Some(
                    "before the file has said whether E values are absolute or relative (M82 or M83)",
                )
            } else if !self.absolute && !self.e_relative {
                Some(
                    "as an absolute value while positioning is relative (G91); firmware differs on whether G91 makes E relative too",
                )
            } else {
                None
            };
            if let Some(why) = unclear {
                self.error(
                    "ambiguous_extrusion",
                    format!("{name} carries E {why}"),
                    raw,
                );
                return false;
            }
        }
        let (reach, travel, who) = self.boxes();
        let start = self.position;
        let mut end = start;
        let mut blind = Vec::new();
        // How far each axis moves, when the file says.
        let mut moved: [Option<f64>; 3] = [Some(0.0); 3];
        for axis in 0..3 {
            let Some(value) = given[axis] else { continue };
            let letter = b"XYZ"[axis] as char;
            if self.absolute {
                if !self.unhomed_refused(name, &[axis], raw) {
                    return false;
                }
                end[axis] = Some(value);
                moved[axis] = start[axis].map(|from| value - from);
            } else if let Some(from) = start[axis] {
                end[axis] = Some(from + value);
                moved[axis] = Some(value);
            } else {
                // Relative to a position the file does not give: it cannot be placed.
                blind.push((axis, value));
                moved[axis] = Some(value);
            }
            let Some(target) = end[axis] else { continue };
            if !travel.contains_axis(axis, target) {
                let [low, high] = [travel.x, travel.y, travel.z][axis];
                self.error(
                    "out_of_reach",
                    format!(
                        "{name} moves {letter} to {target:.3} mm; {who} can go from {low} to {high} mm"
                    ),
                    raw,
                );
                return false;
            }
            if axis == 2 && target < limits.min_z_mm - EPS {
                self.too_close(name, target, raw);
                return false;
            }
        }
        let e_delta = match e {
            Some(value) if self.e_relative => value,
            Some(_) if self.e_unknown => {
                // The counter may or may not have been changed, so nobody can say how
                // much filament this value pushes.
                self.error(
                    "unverifiable_move",
                    format!(
                        "{name} carries an absolute E after lines that run only if the printer decides so changed the extrusion counter; how much filament it pushes cannot be known"
                    ),
                    raw,
                );
                return false;
            }
            Some(value) => value - self.e,
            None => 0.0,
        };
        let extruding = e_delta > 1e-9;
        let moves_xy = given[0].is_some() || given[1].is_some() || arc.is_some();

        // The area the move covers in X and Y: [min_x, max_x, min_y, max_y], and the
        // length of its path there.
        let mut covered: Option<[f64; 4]> = None;
        let mut path_mm: Option<f64> = None;
        if let Some(clockwise) = arc {
            let (Some(sx), Some(sy)) = (start[0], start[1]) else {
                self.error(
                    "unverifiable_move",
                    format!("{name} is an arc from a position that is not known"),
                    raw,
                );
                return false;
            };
            let (ex, ey) = (end[0].unwrap_or(sx), end[1].unwrap_or(sy));
            let center = match (i, j, r) {
                (None, None, Some(radius)) => {
                    let (dx, dy) = (ex - sx, ey - sy);
                    let chord = dx.hypot(dy);
                    let half = chord / 2.0;
                    if chord < 1e-9 || radius.abs() < half - 1e-6 {
                        self.error(
                            "malformed_arc",
                            format!("{name}: no arc of radius {radius} joins the two points"),
                            raw,
                        );
                        return false;
                    }
                    let height = (radius * radius - half * half).max(0.0).sqrt();
                    // The centre lies to the left of the chord for a counter-clockwise arc
                    // of less than half a turn; a negative radius asks for the longer arc.
                    let side = if clockwise == (radius > 0.0) {
                        -1.0
                    } else {
                        1.0
                    };
                    [
                        sx + dx / 2.0 - side * height * dy / chord,
                        sy + dy / 2.0 + side * height * dx / chord,
                    ]
                }
                (_, _, None) if i.is_some() || j.is_some() => {
                    [sx + i.unwrap_or(0.0), sy + j.unwrap_or(0.0)]
                }
                _ => {
                    self.error(
                        "malformed_arc",
                        format!("{name} needs I and J, or R, and not both"),
                        raw,
                    );
                    return false;
                }
            };
            let radius_start = (sx - center[0]).hypot(sy - center[1]);
            let radius_end = (ex - center[0]).hypot(ey - center[1]);
            if radius_start < 1e-9
                || (radius_start - radius_end).abs() > 0.05_f64.max(radius_start * 0.01)
            {
                self.error(
                    "malformed_arc",
                    format!("{name}: the end point is not on the circle around the given centre"),
                    raw,
                );
                return false;
            }
            let bounds = if whole_turn {
                // With a turn count the nozzle goes all the way round.
                [
                    center[0] - radius_start,
                    center[0] + radius_start,
                    center[1] - radius_start,
                    center[1] + radius_start,
                ]
            } else {
                arc_bounds([sx, sy], [ex, ey], center, clockwise)
            };
            let inside = |area: &AxisBox| {
                area.contains_axis(0, bounds[0])
                    && area.contains_axis(0, bounds[1])
                    && area.contains_axis(1, bounds[2])
                    && area.contains_axis(1, bounds[3])
            };
            if !inside(&travel) {
                self.error(
                    "out_of_reach",
                    format!(
                        "{name}: the arc passes through X {:.3} to {:.3}, Y {:.3} to {:.3} mm, outside what {who} can reach",
                        bounds[0], bounds[1], bounds[2], bounds[3]
                    ),
                    raw,
                );
                return false;
            }
            covered = Some(bounds);
            // The angle swept, counter-clockwise from `from` to `to`; the same point is
            // one whole turn. A turn count is taken as one turn, the least it can mean.
            let (from, to) = if clockwise {
                ([ex, ey], [sx, sy])
            } else {
                ([sx, sy], [ex, ey])
            };
            let a0 = (from[1] - center[1]).atan2(from[0] - center[0]);
            let a1 = (to[1] - center[1]).atan2(to[0] - center[0]);
            let mut swept = (a1 - a0).rem_euclid(std::f64::consts::TAU);
            if swept < 1e-12 || whole_turn {
                swept = std::f64::consts::TAU;
            }
            path_mm = Some(radius_start * swept);
        } else if let (Some(sx), Some(sy), Some(ex), Some(ey)) =
            (start[0], start[1], end[0], end[1])
        {
            covered = Some([sx.min(ex), sx.max(ex), sy.min(ey), sy.max(ey)]);
            path_mm = Some((ex - sx).hypot(ey - sy));
        }

        if extruding && moves_xy {
            if section != Section::Body {
                self.error(
                    "printing_outside_body",
                    format!(
                        "{name} lays plastic along a path in {}; only the print itself may",
                        section.phrase()
                    ),
                    raw,
                );
                return false;
            }
            // Plastic laid while moving: the whole path must be on the printable area of
            // the nozzle, at a known height.
            let (Some(area), Some(length)) = (covered, path_mm) else {
                self.error(
                    "unverifiable_move",
                    format!("{name} lays plastic from or to a position that is not known"),
                    raw,
                );
                return false;
            };
            let Some(z) = end[2] else {
                self.error(
                    "unverifiable_move",
                    format!("{name} lays plastic at a height that is not known"),
                    raw,
                );
                return false;
            };
            let on_bed = reach.contains_axis(0, area[0])
                && reach.contains_axis(0, area[1])
                && reach.contains_axis(1, area[2])
                && reach.contains_axis(1, area[3])
                && reach.contains_axis(2, z)
                && start[2].is_none_or(|from| reach.contains_axis(2, from));
            if !on_bed {
                self.error(
                    "extrusion_outside_reach",
                    format!(
                        "{name} lays plastic over X {:.3} to {:.3}, Y {:.3} to {:.3}, Z {z:.3} mm; {who} prints within X {} to {}, Y {} to {}, Z {} to {} mm",
                        area[0], area[1], area[2], area[3],
                        reach.x[0], reach.x[1], reach.y[0], reach.y[1], reach.z[0], reach.z[1]
                    ),
                    raw,
                );
                return false;
            }
            // Layers: plastic goes onto the plate or onto plastic. It may be laid at most
            // one layer above the highest plastic so far (above the plate, at first).
            let (low_z, high_z) = (z.min(start[2].unwrap_or(z)), z.max(start[2].unwrap_or(z)));
            if low_z < limits.min_z_mm - EPS {
                self.too_close(name, low_z, raw);
                return false;
            }
            if high_z > self.top_printed + limits.max_layer_step_mm + EPS {
                let below = if self.top_printed > 0.0 {
                    format!(
                        "the highest plastic laid before it is at {:.3} mm",
                        self.top_printed
                    )
                } else {
                    "nothing has been laid on the plate before it".to_owned()
                };
                self.error(
                    "extrusion_in_the_air",
                    format!(
                        "{name} lays plastic at Z {high_z:.3} mm, but {below}; a layer is at most {} mm above the one below",
                        limits.max_layer_step_mm
                    ),
                    raw,
                );
                return false;
            }
            // Amount: no more per millimetre of path than the nozzle can lay down. The
            // slack covers the three decimals of the coordinates.
            if e_delta > limits.max_e_per_mm * (length + 0.002) + 1e-4 {
                self.error(
                    "over_extrusion",
                    format!(
                        "{name} pushes {e_delta:.3} mm of filament along {length:.3} mm of path; the nozzle lays down at most {} mm per millimetre",
                        limits.max_e_per_mm
                    ),
                    raw,
                );
                return false;
            }
            if self.refuse_cold(name, raw) {
                return false;
            }
            // With several filaments, the amount each one uses is counted per filament.
            // Plastic laid while none is selected would be counted for none of them.
            if self.active.is_none() && self.declared.filaments.len() > 1 {
                self.error(
                    "unassigned_extrusion",
                    format!(
                        "{name} lays plastic before the file has selected a filament; with several filaments in the job nobody can say which one this uses"
                    ),
                    raw,
                );
                return false;
            }
            self.stats.printing_moves += 1;
            self.top_printed = self.top_printed.max(high_z);
            self.heights.insert((z * 1000.0).round() as i64);
            let low = [area[0], area[2], low_z];
            let high = [area[1], area[3], high_z];
            let extent = self.stats.extent.get_or_insert(Extent {
                min: low,
                max: high,
            });
            for axis in 0..3 {
                extent.min[axis] = extent.min[axis].min(low[axis]);
                extent.max[axis] = extent.max[axis].max(high[axis]);
            }
        } else if extruding {
            // Filament pushed without moving in X or Y leaves a blob where the nozzle
            // stands. It may only undo what was pulled back before.
            if e_delta > self.retracted + limits.in_place_slack_mm {
                self.error(
                    "extrusion_in_place",
                    format!(
                        "{name} pushes {e_delta:.3} mm of filament without moving in X or Y; only {:.3} mm were pulled back before it and may be pushed again",
                        self.retracted
                    ),
                    raw,
                );
                return false;
            }
            if self.refuse_cold(name, raw) {
                return false;
            }
        }
        if e_delta < 0.0 && self.pulled_back - e_delta > limits.max_retracted_mm + EPS {
            self.error(
                "retraction_too_long",
                format!(
                    "{name} pulls the filament back to {:.3} mm behind the nozzle; the stock slice pulls back at most {} mm",
                    self.pulled_back - e_delta,
                    limits.max_retracted_mm
                ),
                raw,
            );
            return false;
        }
        if !blind.is_empty() {
            if e.is_some() {
                // Nobody can say where the plastic would be left or taken from.
                self.error(
                    "unverifiable_move",
                    format!("{name} carries E on a move from a position the file does not give"),
                    raw,
                );
                return false;
            }
            if !self.unplaced(name, &blind, false, section, raw) {
                return false;
            }
        }
        // The line is accepted: follow what it does.
        self.stats.moves += 1;
        self.position = end;
        self.feed = feed;
        for (net, known) in self.unplaced_net.iter_mut().zip(end) {
            if known.is_some() {
                // The file says where this axis is: nothing is left to add up.
                *net = 0.0;
            }
        }
        if let Some(branch) = &mut self.branch
            && (moves_xy || given[2].is_some())
        {
            branch.touched = true;
        }
        // Time: the distance at the commanded speed, when the file gives both.
        let distance = match (path_mm, arc) {
            (Some(length), Some(_)) => Some(length.hypot(moved[2].unwrap_or(0.0))),
            _ => match moved {
                [Some(x), Some(y), Some(z)] => Some((x * x + y * y + z * z).sqrt()),
                _ => None,
            },
        };
        if let (Some(distance), Some(feed)) = (distance, self.feed) {
            let distance = if distance > 0.0 {
                distance
            } else {
                e_delta.abs()
            };
            self.stats.min_seconds += distance / (feed / 60.0);
        }
        if e_delta != 0.0 {
            // Amount: what this filament was pushed, less what it was pulled back.
            let single = self.declared.filaments.len() == 1;
            match self.active.or(single.then_some(0)) {
                Some(index) if index < self.stats.filament_mm.len() => {
                    self.stats.filament_mm[index] += e_delta;
                }
                _ => self.stats.unassigned_filament_mm += e_delta,
            }
            if e_delta < 0.0 {
                self.pulled_back -= e_delta;
                if self.branch.is_none() {
                    // Pulled back for certain: this much may be pushed again in place.
                    self.retracted -= e_delta;
                }
            } else {
                // Pushed forward again, in place or along a path: that much is no
                // longer pulled back, and cannot be pushed in place a second time.
                self.retracted = (self.retracted - e_delta).max(0.0);
                self.pulled_back = (self.pulled_back - e_delta).max(0.0);
            }
        }
        if let Some(value) = e {
            // The counter runs in both modes, so a later change of mode starts from the
            // right value.
            if self.e_relative {
                self.e += value;
            } else {
                self.e = value;
                if self.branch.is_none() {
                    self.e_unknown = false;
                }
            }
            if let Some(branch) = &mut self.branch {
                branch.e_touched = true;
            }
        }
        true
    }

    fn finish(mut self) -> LintReport {
        self.line = 0;
        let profile = self.profile;
        if self.branch.is_some() {
            self.error(
                "branch",
                "the file ends inside lines started by M622 and not ended by M623".to_owned(),
                b"",
            );
        }
        let markers = &profile.family().gcode.sections;
        let missing = match self.stage {
            Stage::Start => Some(&markers.start_end),
            Stage::Body => Some(&markers.end_start),
            Stage::End => Some(&markers.close),
            Stage::Preamble | Stage::Plain | Stage::Done => None,
        };
        if let Some(marker) = missing {
            self.error(
                "truncated",
                format!(
                    "the file has the slicer's section markers but ends before `{marker}`; it may be cut off, and a print that never reaches its end routine leaves the heaters on"
                ),
                b"",
            );
        }
        self.check_statements();
        self.check_end_state();
        if !self.stats.configuration.is_empty() && self.errors == 0 {
            let message = format!(
                "The printer's own start and end routines in this file change settings: {}. They are accepted because Bambu Studio's stock routine for this printer has them in exactly this form; any other form is refused.",
                configuration_sentence(&self.stats.configuration)
            );
            self.report(Severity::Warning, "configuration_commands", message, b"");
        }
        if self.stats.moves == 0 && self.errors == 0 {
            self.report(
                Severity::Warning,
                "no_moves",
                "the G-code contains no moves; nothing would be printed".to_owned(),
                b"",
            );
        }
        LintReport {
            findings: self.findings,
            errors: self.errors,
            warnings: self.warnings,
            stats: self.stats,
        }
    }
}

impl Linter<'_> {
    /// Holds what the file says it was sliced for to the printer and the declared job.
    /// The statements are comments of the file under check, so this catches a wrong
    /// profile or a mix-up, not a forgery; against a forger the protection is that every
    /// line is held to this printer's table and reach.
    fn check_statements(&mut self) {
        let profile = self.profile;
        let files = profile.slice_file();
        let stated = self.stats.stated.clone();
        let conflict = self.stated_conflict;
        let wrong = |linter: &mut Self, code: &'static str, message: String| {
            linter.error(code, message, b"");
        };
        // The printer: stated once, and one of the names a file for this printer has.
        match &stated.printer_model {
            _ if conflict[0] => wrong(
                self,
                "wrong_printer",
                "the file names the printer it was sliced for more than once, with different names"
                    .to_owned(),
            ),
            None => wrong(
                self,
                "wrong_printer",
                format!(
                    "the file does not say which printer it was sliced for (`; printer_model = ...`); a file sliced for the {} says so",
                    profile.name()
                ),
            ),
            Some(model) if !files.printer_models.iter().any(|name| name == model) => wrong(
                self,
                "wrong_printer",
                format!(
                    "the file says it was sliced for {}; this job is for the {}",
                    quoted(model),
                    profile.name()
                ),
            ),
            Some(_) => {}
        }
        match &stated.printer_settings_id {
            _ if conflict[3] => wrong(
                self,
                "wrong_printer",
                "the file names its printer settings more than once, with different names"
                    .to_owned(),
            ),
            Some(id) if !files.printer_settings_ids.iter().any(|name| name == id) => wrong(
                self,
                "wrong_printer",
                format!(
                    "the file says it was sliced with the printer settings {}, which are not settings of the {} that Hotend knows",
                    quoted(id),
                    profile.name()
                ),
            ),
            _ => {}
        }
        // The build plate: the one the person chose.
        let declared = self.declared;
        match &stated.plate_type {
            _ if conflict[1] => wrong(
                self,
                "plate_mismatch",
                "the file names the build plate it was sliced for more than once, with different names"
                    .to_owned(),
            ),
            None => wrong(
                self,
                "plate_mismatch",
                format!(
                    "the file does not say which build plate it was sliced for (`; curr_bed_type = ...`); the job is for the {}",
                    quoted(&declared.plate_type)
                ),
            ),
            Some(plate) if *plate != declared.plate_type => wrong(
                self,
                "plate_mismatch",
                format!(
                    "the file says it was sliced for the build plate {}; the plate chosen for this job is {}",
                    quoted(plate),
                    quoted(&declared.plate_type)
                ),
            ),
            Some(_) => {}
        }
        // The nozzle size of every filament's nozzle.
        if conflict[2] || stated.nozzle_diameters_mm.is_empty() {
            wrong(
                self,
                "nozzle_mismatch",
                "the file does not say, or says in two ways, which nozzle sizes it was sliced for (`; nozzle_diameter = ...`)"
                    .to_owned(),
            );
            return;
        }
        for filament in &declared.filaments {
            let size = profile
                .nozzle_by_device_id(filament.nozzle)
                .and_then(|nozzle| usize::from(nozzle.slicer_extruder).checked_sub(1))
                .and_then(|at| stated.nozzle_diameters_mm.get(at).copied());
            if size.is_none_or(|size| (size - filament.nozzle_diameter_mm).abs() > 1e-6) {
                wrong(
                    self,
                    "nozzle_mismatch",
                    format!(
                        "the file was sliced for a nozzle of {} mm where filament {} prints; the job declares {} mm",
                        size.map_or_else(|| "unstated size".to_owned(), |size| size.to_string()),
                        filament.slot,
                        filament.nozzle_diameter_mm
                    ),
                );
            }
        }
    }

    /// What must hold when the file has ended: the layer count it states is the number
    /// of layers its commands print, and every heater is switched off.
    fn check_end_state(&mut self) {
        self.stats.layers_printed = self.heights.len().min(u32::MAX as usize) as u32;
        if self.layers_conflict {
            self.error(
                "layer_count_mismatch",
                "the file states its number of layers more than once with different numbers, or not as a number"
                    .to_owned(),
                b"",
            );
        } else if let Some(stated) = self.stats.total_layers
            && stated != self.stats.layers_printed
        {
            let printed = self.stats.layers_printed;
            self.error(
                "layer_count_mismatch",
                format!(
                    "the file says it has {stated} layer(s), but its commands lay plastic at {printed} height(s)"
                ),
                b"",
            );
        }
        let profile = self.profile;
        let mut left_on = Vec::new();
        for heat in &self.nozzle_heat {
            if heat.high > 0.0 {
                let role = profile
                    .nozzle_by_device_id(heat.id)
                    .map_or("a", |nozzle| nozzle.role.as_str());
                left_on.push(format!(
                    "the {role} nozzle (id {}) at up to {} C",
                    heat.id, heat.high
                ));
            }
        }
        if self.bed_high > 0.0 {
            left_on.push(format!("the bed at up to {} C", self.bed_high));
        }
        if self.chamber_high > 0.0 {
            left_on.push(format!("the chamber at up to {} C", self.chamber_high));
        }
        if !left_on.is_empty() {
            self.error(
                "heaters_left_on",
                format!(
                    "when the file ends it has not switched off {}; the stock end routine switches every heater off, each by its own command",
                    left_on.join(", ")
                ),
                b"",
            );
        }
    }
}

/// The accepted configuration commands as one sentence part: what is changed, by which
/// commands, how often.
pub(crate) fn configuration_sentence(uses: &[ConfigurationUse]) -> String {
    let mut kinds: Vec<ConfigKind> = uses.iter().map(|used| used.kind).collect();
    kinds.sort();
    kinds.dedup();
    let parts: Vec<String> = kinds
        .into_iter()
        .map(|kind| {
            let commands: Vec<String> = uses
                .iter()
                .filter(|used| used.kind == kind)
                .map(|used| {
                    if used.count == 1 {
                        format!("{} once", used.command)
                    } else {
                        format!("{} {} times", used.command, used.count)
                    }
                })
                .collect();
            format!("{} ({})", kind.phrase(), commands.join(", "))
        })
        .collect();
    parts.join("; ")
}

fn value_of(params: &[Word<'_>], letter: u8) -> Option<f64> {
    params
        .iter()
        .find(|word| word.letter == letter)
        .and_then(|word| plain_number(word.value))
}

/// Checks a G-code stream for `declared` on a printer of `profile`. At most `max_bytes`
/// are read; a longer stream is a finding. `Err` is only a read error.
pub fn lint_gcode<R: Read>(
    mut reader: R,
    declared: &DeclaredJob,
    profile: &PrinterProfile,
    max_bytes: u64,
) -> std::io::Result<LintReport> {
    let mut linter = Linter::new(profile, declared);
    let mut buffer = vec![0u8; 64 * 1024];
    let mut line: Vec<u8> = Vec::with_capacity(256);
    let mut too_long = false;
    // Whether the line starts with `;` (after blanks): decided by its first other byte.
    let mut comment_only: Option<bool> = None;
    let mut long_comment_bytes: u64 = 0;
    let mut after_cr = false;
    let mut total: u64 = 0;
    let max_lines = profile.family().gcode.limits.max_lines;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > max_bytes {
            linter.line = 0;
            linter.error(
                "gcode_too_large",
                format!("the G-code is longer than {max_bytes} bytes and was not read to the end"),
                b"",
            );
            return Ok(linter.finish());
        }
        for &byte in &buffer[..read] {
            // A lone carriage return ends a line for some firmware, so it does here; the
            // line feed of a CR LF pair is not a second line.
            if byte == b'\n' && after_cr {
                after_cr = false;
                continue;
            }
            after_cr = byte == b'\r';
            if byte == b'\n' || byte == b'\r' {
                if !line.is_empty() || too_long {
                    linter.feed(&line, too_long);
                } else {
                    linter.line += 1;
                    linter.stats.lines += 1;
                }
                line.clear();
                too_long = false;
                comment_only = None;
                if linter.line >= max_lines {
                    linter.line = 0;
                    linter.error(
                        "gcode_too_large",
                        format!(
                            "the G-code has more than {max_lines} lines and was not read to the end"
                        ),
                        b"",
                    );
                    return Ok(linter.finish());
                }
            } else if line.len() < MAX_LINE_BYTES {
                if comment_only.is_none() && !matches!(byte, b' ' | b'\t') {
                    comment_only = Some(byte == b';');
                }
                line.push(byte);
            } else if comment_only == Some(true) {
                // The rest of a long comment is skipped, up to a total.
                long_comment_bytes += 1;
                if long_comment_bytes == MAX_LONG_COMMENT_BYTES + 1 {
                    linter.line += 1;
                    linter.error(
                        "comment_too_large",
                        format!(
                            "comment lines run more than {MAX_LONG_COMMENT_BYTES} bytes past the line length of {MAX_LINE_BYTES} bytes"
                        ),
                        b"",
                    );
                    linter.line -= 1;
                }
            } else {
                too_long = true;
            }
        }
    }
    if !line.is_empty() || too_long {
        linter.feed(&line, too_long);
    }
    Ok(linter.finish())
}

#[cfg(test)]
mod tests {
    use super::quoted;

    /// A refusal repeats a name only when it is a plain one. Anything that reads like a
    /// sentence, runs long, or carries a character a name does not need, is not repeated:
    /// the text of a file or of a declaration never writes a message of its own.
    #[test]
    fn a_refusal_repeats_plain_names_only() {
        for plain in [
            "Cool Plate",
            "Textured PEI Plate",
            "PLA",
            "PA-CF",
            "Bambu Lab X2D 0.4 nozzle",
            "Metadata/plate_2.gcode",
        ] {
            assert_eq!(quoted(plain), format!("\"{plain}\""));
        }
        for not_plain in [
            "",
            "Cool Plate. Time? 2 min.",
            "Cool Plate. Safe",
            "PLA\" is fine, so is \"ABS",
            "PLA\nERROR: none",
            "PLA\u{202e}",
            "Metadata?plate_2.gcode",
            "a name that is far longer than any name a profile has",
            "end.",
            ".start",
        ] {
            assert_eq!(quoted(not_plain), "a name that is not a plain name");
        }
    }
}
