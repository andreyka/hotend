//! Fixtures shared by the integration tests: handcrafted sliced files.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use hotend_printer::{
    ConfirmationAuthority, DeclaredFilament, DeclaredJob, PrinterProfile, Profiles,
};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

pub fn x2d() -> PrinterProfile {
    Profiles::builtin().unwrap().printer("bambu-x2d").unwrap()
}

/// The process's confirmation authority, standing in for the confirmation dialog.
pub fn dialog() -> &'static ConfirmationAuthority {
    static AUTHORITY: OnceLock<ConfirmationAuthority> = OnceLock::new();
    AUTHORITY.get_or_init(|| ConfirmationAuthority::take().expect("taken once per test process"))
}

/// The build plate of the standard jobs: the one the real slice was made for.
pub const COOL_PLATE: &str = "Cool Plate";
/// A plate with a higher bed temperature ceiling.
pub const TEXTURED_PLATE: &str = "Textured PEI Plate";

/// One filament of a job on a 0.4 mm nozzle.
pub fn filament(slot: u32, material: &str, nozzle: u8) -> DeclaredFilament {
    DeclaredFilament {
        slot,
        material: material.to_owned(),
        nozzle,
        nozzle_diameter_mm: 0.4,
    }
}

/// A job for plate 1 on `plate_type` with these filaments.
pub fn job_on(plate_type: &str, filaments: Vec<DeclaredFilament>) -> DeclaredJob {
    DeclaredJob {
        plate: 1,
        plate_type: plate_type.to_owned(),
        filaments,
    }
}

/// A file written for the Cool Plate as the slicer writes it for the textured plate: the
/// plate's name and the Z trim the start routine sets for it.
pub fn for_textured(gcode: &str) -> String {
    gcode
        .replace("= Cool Plate", "= Textured PEI Plate")
        .replace("G29.1 Z0.022", "G29.1 Z0.002")
}

/// One PLA filament on the main nozzle (device extruder id 1), on the Cool Plate.
pub fn pla_job() -> DeclaredJob {
    job_on(COOL_PLATE, vec![filament(1, "PLA", 1)])
}

/// PLA on the main nozzle (id 1) and PETG on the auxiliary nozzle (id 0), on the
/// textured plate (PETG wants a warmer bed than the Cool Plate takes).
pub fn two_nozzle_job() -> DeclaredJob {
    job_on(
        TEXTURED_PLATE,
        vec![filament(1, "PLA", 1), filament(2, "PETG", 0)],
    )
}

/// What a file says it was sliced for, as the comments the check reads: this printer,
/// the Cool Plate, 0.4 mm nozzles.
pub const STATEMENTS: &str = "\
; printer_model = Bambu Lab X2D
; curr_bed_type = Cool Plate
; nozzle_diameter = 0.4,0.4
";

/// A small, valid single-filament print: three layers, 10.25 mm of filament, bed at
/// 35 C, nozzle at 220 C, every heater off at the end.
pub const GOOD_GCODE: &str = "\
; HEADER_BLOCK_START
; total layer number: 3
; HEADER_BLOCK_END
; printer_model = Bambu Lab X2D
; curr_bed_type = Cool Plate
; nozzle_diameter = 0.4,0.4
M73 P0 R12
M140 S35
M104 S220
G90
M83
G28
M190 S35
M109 S220
M1002 gcode_claim_action : 2
T0
G1 Z0.2 F600
G1 X50 Y50 F9000
G1 X100 Y50 E2.5 F1800
G3 X110 Y60 I0 J10 E0.6
G1 X110 Y100 E1.9
G1 Z0.4 F600
G1 X50 Y100 E2.85 F1800
G1 Z0.6 F600
G1 X50 Y50 E2.4 F1800
G92 E0
M73 P100 R0
M104 S0
M140 S0
";

/// Filament the commands of [`GOOD_GCODE`] push, in millimetres.
pub const GOOD_FILAMENT_MM: f64 = 10.25;

/// MD5 of `bytes` as the slicer writes it into `Metadata/plate_<n>.gcode.md5`: 32
/// upper-case hex digits. Written here independently of the crate's own.
pub fn md5_upper(bytes: &[u8]) -> String {
    let shifts: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    let mut message = bytes.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((bytes.len() as u64) * 8).to_le_bytes());
    for block in message.chunks_exact(64) {
        let word = |at: usize| {
            u32::from_le_bytes([
                block[4 * at],
                block[4 * at + 1],
                block[4 * at + 2],
                block[4 * at + 3],
            ])
        };
        let [mut a, mut b, mut c, mut d] = state;
        for step in 0..64usize {
            let (f, g) = match step / 16 {
                0 => ((b & c) | (!b & d), step),
                1 => ((d & b) | (!d & c), (5 * step + 1) % 16),
                2 => (b ^ c ^ d, (3 * step + 5) % 16),
                _ => (c ^ (b | !d), (7 * step) % 16),
            };
            let k = (f64::from(step as u32 + 1).sin().abs() * 4_294_967_296.0) as u32;
            let turned = a
                .wrapping_add(f)
                .wrapping_add(k)
                .wrapping_add(word(g))
                .rotate_left(shifts[step / 16 * 4 + step % 4]);
            (a, d, c, b) = (d, c, b, b.wrapping_add(turned));
        }
        for (kept, add) in state.iter_mut().zip([a, b, c, d]) {
            *kept = kept.wrapping_add(add);
        }
    }
    state
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .map(|byte| format!("{byte:02X}"))
        .collect()
}

/// `slice_info.config` for plate 1 with the given filament lines and filament map. A
/// filament line is `(material, colour, grams)`; the length is stated as a third of the
/// grams in metres, which is what 1.75 mm PLA weighs.
pub fn slice_info(prediction: u32, filaments: &[(&str, &str, f64)], maps: &str) -> String {
    let mut lines = String::new();
    for (index, (material, colour, grams)) in filaments.iter().enumerate() {
        lines.push_str(&format!(
            "    <filament id=\"{}\" type=\"{material}\" color=\"{colour}\" used_m=\"{:.2}\" used_g=\"{grams:.2}\"/>\n",
            index + 1,
            grams / 3.0
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<config>\n  <plate>\n    <metadata key=\"index\" value=\"1\"/>\n    <metadata key=\"prediction\" value=\"{prediction}\"/>\n    <metadata key=\"filament_maps\" value=\"{maps}\"/>\n{lines}  </plate>\n</config>\n"
    )
}

/// The slice info of the standard good file: 20 minutes, 0.03 g (0.01 m) of white PLA.
pub fn good_slice_info() -> String {
    slice_info(1200, &[("PLA", "#FFFFFF", 0.03)], "1")
}

pub fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in entries {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// The entries of a sliced file for plate 1 with this G-code and this slice info: the
/// G-code, its MD5 as the slicer writes it, the slice info, and the plate preview (a stub:
/// Bambu Connect does not import a file without one, and no check reads it).
pub fn sliced_entries(gcode: &str, info: &str) -> Vec<(String, Vec<u8>)> {
    vec![
        ("[Content_Types].xml".to_owned(), b"<Types/>".to_vec()),
        (
            "Metadata/plate_1.gcode".to_owned(),
            gcode.as_bytes().to_vec(),
        ),
        (
            "Metadata/plate_1.gcode.md5".to_owned(),
            md5_upper(gcode.as_bytes()).into_bytes(),
        ),
        (
            "Metadata/slice_info.config".to_owned(),
            info.as_bytes().to_vec(),
        ),
        ("Metadata/plate_1.png".to_owned(), b"png".to_vec()),
    ]
}

/// `entries` as borrowed pairs, for [`zip_bytes`].
pub fn borrowed(entries: &[(String, Vec<u8>)]) -> Vec<(&str, &[u8])> {
    entries
        .iter()
        .map(|(name, content)| (name.as_str(), content.as_slice()))
        .collect()
}

/// The bytes of a `.gcode.3mf` with this G-code for plate 1 and this slice info.
pub fn sliced_bytes(gcode: &str, info: &str) -> Vec<u8> {
    zip_bytes(&borrowed(&sliced_entries(gcode, info)))
}

/// Writes a `.gcode.3mf` with this G-code for plate 1 and this slice info.
pub fn write_sliced(dir: &Path, file: &str, gcode: &str, info: &str) -> PathBuf {
    let path = dir.join(file);
    std::fs::write(&path, sliced_bytes(gcode, info)).unwrap();
    path
}

/// Writes a `.gcode.3mf` with the standard good content and these further entries; an
/// entry of the same name replaces the standard one.
pub fn write_with(dir: &Path, file: &str, extra: &[(&str, &[u8])]) -> PathBuf {
    let mut entries = sliced_entries(GOOD_GCODE, &good_slice_info());
    for (name, content) in extra {
        entries.retain(|(known, _)| known != name);
        entries.push(((*name).to_owned(), content.to_vec()));
    }
    let path = dir.join(file);
    std::fs::write(&path, zip_bytes(&borrowed(&entries))).unwrap();
    path
}

/// The standard good file: one PLA filament, 20 minutes.
pub fn write_good(dir: &Path, file: &str) -> PathBuf {
    write_sliced(dir, file, GOOD_GCODE, &good_slice_info())
}

/// A valid print with PLA on the main nozzle and PETG on the auxiliary nozzle. After a
/// tool change the file says where the nozzle is before it lays plastic.
pub const TWO_GCODE: &str = "\
; total layer number: 2
; printer_model = Bambu Lab X2D
; curr_bed_type = Textured PEI Plate
; nozzle_diameter = 0.4,0.4
M73 P0 R30
M140 S70
G90
M83
G28
M190 S70
T0
M109 S220
G1 Z0.2 F600
G1 X50 Y50 F9000
G1 X100 Y50 E2.5 F1800
T1
M109 S250
G1 Z0.4 F600
G1 X60 Y80 F9000
G1 X120 Y80 E3.0 F1800
M104 S0 T1
M104 S0 T0
M140 S0
";

/// The slice info of the standard two-nozzle file.
pub fn two_slice_info() -> String {
    slice_info(
        1800,
        &[("PLA", "#FFFFFF", 0.01), ("PETG", "#30A050", 0.01)],
        "1 2",
    )
}

/// The standard two-nozzle file: PLA on slicer extruder 1, PETG on slicer extruder 2.
pub fn write_two(dir: &Path, file: &str) -> PathBuf {
    write_sliced(dir, file, TWO_GCODE, &two_slice_info())
}

/// Checks `path` as the job `name` for the X2D with default limits.
pub fn check(
    path: &Path,
    name: &str,
    declared: &DeclaredJob,
) -> Result<hotend_printer::CheckedJob, hotend_printer::JobError> {
    hotend_printer::check_job(
        path,
        name,
        declared,
        &x2d(),
        &hotend_printer::ArchiveLimits::default(),
    )
}

use hotend_printer::{
    CheckedJob, ConfirmationRequest, FilamentSource, Printer, SimConfig, SimulatedPrinter,
    StartToken,
};

/// A simulated X2D on manual time.
pub fn sim() -> SimulatedPrinter {
    let profile = x2d();
    let config = SimConfig::x2d(&profile);
    SimulatedPrinter::new(profile, config)
}

/// The first PLA tray of the AMS on the main nozzle.
pub const PLA_TRAY: FilamentSource = FilamentSource::Ams { unit: 0, tray: 0 };
/// The PETG tray of the AMS HT on the auxiliary nozzle.
pub const PETG_TRAY: FilamentSource = FilamentSource::Ams { unit: 128, tray: 0 };

/// A token as the confirmation dialog would mint it after a click, for `job` on `printer`.
pub fn confirmed(job: &CheckedJob, printer: &dyn Printer) -> StartToken {
    let request = ConfirmationRequest::start_print(job, printer.identity());
    StartToken::mint(click(request), StartToken::DEFAULT_TTL).unwrap()
}

/// The dialog shows the request's text and the person confirms.
pub fn click(request: ConfirmationRequest) -> hotend_printer::HumanConfirmation {
    let shown = request.review_text();
    dialog().confirm(request, &shown).unwrap()
}

/// The four section markers of a Bambu Studio file, as comment lines.
pub const OPEN: &str = "; EXECUTABLE_BLOCK_START";
pub const START_END: &str = "; MACHINE_START_GCODE_END";
pub const END_START: &str = "; MACHINE_END_GCODE_START";
pub const CLOSE: &str = "; EXECUTABLE_BLOCK_END";

/// The leading comments of a file in the shape Bambu Studio writes: a header and the
/// statements the check reads from the config block.
pub const STOCK_PREAMBLE: &str = "\
; HEADER_BLOCK_START
; total layer number: 2
; HEADER_BLOCK_END
; CONFIG_BLOCK_START
; printer_model = Bambu Lab X2D
; curr_bed_type = Cool Plate
; nozzle_diameter = 0.4,0.4
; CONFIG_BLOCK_END
";

/// A start routine in the shape of the stock one. Hand-written for these tests from the
/// command table, in a made-up order and length; single commands follow the shape of real
/// files. It ends with the position unknown, absolute positioning and filament 1 selected.
pub const STOCK_START: &str = "\
M73 P0 R9
M201 X18000 Y18000 Z400 E4000
M203 X450 Y450 Z18 E25
M204 P9000 R4000 T9000
M205 X8 Y8 Z2 E2
M211 X0 Y0 Z0
M500 D1
M17 D
M140 S35
M104 S150 T1
G91
G380 S2 Z10 F900
G1 Z4 F900
G90
G28 X T300 R
G28 Z P0 T400
M1002 gcode_claim_action : 13
M622 J1
G29 A1 X60 Y60 I80 J80 R
M623
G29.1 Z0.022
M620 M
M620 S0A H-1 B
M104 S220
T0 H-1
M621 S0A B
M1002 set_filament_type:PLA
M620.10 A0 F300 H0.4 T235 P220 S1
M109 S220
G150.1 F6000
G130 O0 X90 Y-0.2 Z0.6 F4 L40 E12 D4
M190 S35
M106 P1 S0
";

/// A two-layer print. It ends at X 60, Y 100, Z 0.4 with plastic laid.
pub const STOCK_BODY: &str = "\
G90
M83
G1 Z0.4 F1200
G1 X60 Y60 F9000
G1 Z0.2
M204 S6000
M73 P10 R8
M624 AQAAAAAAAAA=
G1 X100 Y60 E1.6 F1800
G3 X110 Y70 I0 J10 E0.5
G1 X110 Y100 E1.2
M625
M991 S0 P0
M73 L1
G1 Z0.4 F1200
G1 X60 Y100 E1.9 F1800
M73 P90 R1
";

/// An end routine in the shape of the stock one, written the same way as [`STOCK_START`].
pub const STOCK_END: &str = "\
G92 E0
M400
G1 E-0.8 F1800
G1 Z5.4 F900
G1 X128 Y250 F6000
M140 S0
M106 S0
M620 S65279 B
T65279
M621 S65279 B
M104 S0 T0
M104 S0 T1
M17 Z0.4
M17 S
M17 R
M211 Z1
M201.2 K1
M220 S100
M73 P100 R0
";

/// A whole file with section markers from its four parts.
pub fn marked(preamble: &str, start: &str, body: &str, end: &str) -> String {
    format!("{preamble}{OPEN}\n{start}{START_END}\n{body}{END_START}\n{end}{CLOSE}\n")
}

/// The standard file in the shape Bambu Studio writes.
pub fn stock() -> String {
    marked(STOCK_PREAMBLE, STOCK_START, STOCK_BODY, STOCK_END)
}

/// The standard stock-shaped file with `lines` added at the end of its start routine.
pub fn stock_with_start(lines: &str) -> String {
    marked(
        STOCK_PREAMBLE,
        &format!("{STOCK_START}{lines}\n"),
        STOCK_BODY,
        STOCK_END,
    )
}

/// The standard stock-shaped file with `lines` added at the end of the print.
pub fn stock_with_body(lines: &str) -> String {
    marked(
        STOCK_PREAMBLE,
        STOCK_START,
        &format!("{STOCK_BODY}{lines}\n"),
        STOCK_END,
    )
}

/// The standard stock-shaped file with `lines` added at the end of its end routine.
pub fn stock_with_end(lines: &str) -> String {
    marked(
        STOCK_PREAMBLE,
        STOCK_START,
        STOCK_BODY,
        &format!("{STOCK_END}{lines}\n"),
    )
}

/// `slice_info.config` in the shape Bambu Studio writes for one PLA filament on the main
/// nozzle: 15 minutes, 0.02 g (0.01 m).
pub fn stock_slice_info() -> String {
    slice_info(900, &[("PLA", "#00AE42", 0.02)], "1")
        .replace("type=\"PLA\"", "type=\"PLA\" nozzle_diameter=\"0.40\"")
}

/// The standard stock-shaped sliced file.
pub fn write_stock(dir: &Path, file: &str) -> PathBuf {
    write_sliced(dir, file, &stock(), &stock_slice_info())
}

// ---------------------------------------------------------------------------------------
// Archives written byte by byte, for shapes no zip writer produces.
// ---------------------------------------------------------------------------------------

/// CRC-32 as zip uses it.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One stored (uncompressed) entry of a hand-written archive. What the local header says
/// can be set apart from what the central directory says and from what is stored.
#[derive(Debug, Clone)]
pub struct RawEntry {
    /// The name in the central directory.
    pub name: Vec<u8>,
    /// The name in the local header.
    pub local_name: Vec<u8>,
    pub data: Vec<u8>,
    pub flags: u16,
    /// Size the central directory declares, and the local header.
    pub size: u32,
    pub local_size: u32,
    /// Checksum the central directory declares, and the local header.
    pub crc: u32,
    pub local_crc: u32,
    /// Whether the central directory lists the entry at all.
    pub listed: bool,
    /// Sizes and checksum follow the data (as the real slicer writes) instead of
    /// standing in the local header.
    pub descriptor: bool,
    pub made_by: u16,
    pub external: u32,
    /// The comment of the entry in the central directory.
    pub comment: Vec<u8>,
}

impl RawEntry {
    pub fn new(name: &str, data: &[u8]) -> Self {
        let crc = crc32(data);
        Self {
            name: name.as_bytes().to_vec(),
            local_name: name.as_bytes().to_vec(),
            data: data.to_vec(),
            flags: 0,
            size: data.len() as u32,
            local_size: data.len() as u32,
            crc,
            local_crc: crc,
            listed: true,
            descriptor: false,
            made_by: 20,
            external: 0,
            comment: Vec::new(),
        }
    }
}

/// How a hand-written archive is put together around its entries.
#[derive(Debug, Clone, Default)]
pub struct RawLayout {
    /// Bytes before the first entry.
    pub prefix: Vec<u8>,
    /// The archive comment.
    pub comment: Vec<u8>,
    /// Bytes after the end record.
    pub trailing: Vec<u8>,
    /// A zip64 end record and locator in front of the end record.
    pub zip64: bool,
}

/// Writes the archive.
pub fn raw_zip(entries: &[RawEntry], layout: &RawLayout) -> Vec<u8> {
    let mut out = layout.prefix.clone();
    let u16le = |value: u16| value.to_le_bytes();
    let u32le = |value: u32| value.to_le_bytes();
    let mut offsets = Vec::new();
    for entry in entries {
        offsets.push(out.len() as u32);
        let flags = entry.flags | if entry.descriptor { 0x0008 } else { 0 };
        let stored = entry.data.len() as u32;
        let (crc, packed, size) = if entry.descriptor {
            (0, 0, 0)
        } else {
            (entry.local_crc, stored, entry.local_size)
        };
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&u16le(20));
        out.extend_from_slice(&u16le(flags));
        out.extend_from_slice(&u16le(0)); // stored
        out.extend_from_slice(&u16le(0)); // time
        out.extend_from_slice(&u16le(0x5A21)); // date
        out.extend_from_slice(&u32le(crc));
        out.extend_from_slice(&u32le(packed));
        out.extend_from_slice(&u32le(size));
        out.extend_from_slice(&u16le(entry.local_name.len() as u16));
        out.extend_from_slice(&u16le(0));
        out.extend_from_slice(&entry.local_name);
        out.extend_from_slice(&entry.data);
        if entry.descriptor {
            out.extend_from_slice(b"PK\x07\x08");
            out.extend_from_slice(&u32le(entry.local_crc));
            out.extend_from_slice(&u32le(stored));
            out.extend_from_slice(&u32le(entry.local_size));
        }
    }
    let directory_at = out.len() as u32;
    let mut listed = 0u16;
    for (entry, offset) in entries.iter().zip(&offsets) {
        if !entry.listed {
            continue;
        }
        listed += 1;
        let flags = entry.flags | if entry.descriptor { 0x0008 } else { 0 };
        out.extend_from_slice(b"PK\x01\x02");
        out.extend_from_slice(&u16le(entry.made_by));
        out.extend_from_slice(&u16le(20));
        out.extend_from_slice(&u16le(flags));
        out.extend_from_slice(&u16le(0));
        out.extend_from_slice(&u16le(0));
        out.extend_from_slice(&u16le(0x5A21));
        out.extend_from_slice(&u32le(entry.crc));
        out.extend_from_slice(&u32le(entry.data.len() as u32));
        out.extend_from_slice(&u32le(entry.size));
        out.extend_from_slice(&u16le(entry.name.len() as u16));
        out.extend_from_slice(&u16le(0)); // extra
        out.extend_from_slice(&u16le(entry.comment.len() as u16));
        out.extend_from_slice(&u16le(0)); // disk
        out.extend_from_slice(&u16le(0)); // internal attributes
        out.extend_from_slice(&u32le(entry.external));
        out.extend_from_slice(&u32le(*offset));
        out.extend_from_slice(&entry.name);
        out.extend_from_slice(&entry.comment);
    }
    let directory_len = out.len() as u32 - directory_at;
    if layout.zip64 {
        let record_at = out.len() as u64;
        out.extend_from_slice(b"PK\x06\x06");
        out.extend_from_slice(&44u64.to_le_bytes());
        out.extend_from_slice(&u16le(45));
        out.extend_from_slice(&u16le(45));
        out.extend_from_slice(&u32le(0));
        out.extend_from_slice(&u32le(0));
        out.extend_from_slice(&u64::from(listed).to_le_bytes());
        out.extend_from_slice(&u64::from(listed).to_le_bytes());
        out.extend_from_slice(&u64::from(directory_len).to_le_bytes());
        out.extend_from_slice(&u64::from(directory_at).to_le_bytes());
        out.extend_from_slice(b"PK\x06\x07");
        out.extend_from_slice(&u32le(0));
        out.extend_from_slice(&record_at.to_le_bytes());
        out.extend_from_slice(&u32le(1));
    }
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&u16le(0));
    out.extend_from_slice(&u16le(0));
    if layout.zip64 {
        out.extend_from_slice(&u16le(0xFFFF));
        out.extend_from_slice(&u16le(0xFFFF));
        out.extend_from_slice(&u32le(0xFFFF_FFFF));
        out.extend_from_slice(&u32le(0xFFFF_FFFF));
    } else {
        out.extend_from_slice(&u16le(listed));
        out.extend_from_slice(&u16le(listed));
        out.extend_from_slice(&u32le(directory_len));
        out.extend_from_slice(&u32le(directory_at));
    }
    out.extend_from_slice(&u16le(layout.comment.len() as u16));
    out.extend_from_slice(&layout.comment);
    out.extend_from_slice(&layout.trailing);
    out
}

/// The standard good file as stored entries of a hand-written archive.
pub fn raw_good() -> Vec<RawEntry> {
    sliced_entries(GOOD_GCODE, &good_slice_info())
        .iter()
        .map(|(name, content)| RawEntry::new(name, content))
        .collect()
}

/// Writes a hand-written archive to `dir/file`.
pub fn write_raw(dir: &Path, file: &str, entries: &[RawEntry], layout: &RawLayout) -> PathBuf {
    let path = dir.join(file);
    std::fs::write(&path, raw_zip(entries, layout)).unwrap();
    path
}
