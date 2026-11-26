//! Reads a Bambu Studio `.gcode.3mf`: a zip archive with the G-code, the slicer's own
//! numbers per plate (`Metadata/slice_info.config`, XML) and a few more metadata files.
//!
//! The layout was measured on a file sliced for the X2D by Bambu Studio 02.09.01.70
//! (17 November 2026). What Hotend reads, most trusted first:
//!
//! 1. `Metadata/plate_<n>.gcode`, the file the printer runs ([`super::gcode`]): its header
//!    (version, total time, layer count, filament length and weight per filament), its
//!    settings block (`curr_bed_type`, the plate temperatures) and its commands (the
//!    highest bed and nozzle temperature, the printed extent).
//! 2. `Metadata/slice_info.config`, for what the G-code does not state: the filament map
//!    and the slicer's warnings, and time and filament use when the header lacks them.
//!
//!    ```xml
//!    <config>
//!      <plate>
//!        <metadata key="index" value="1"/>
//!        <metadata key="prediction" value="710"/>         seconds
//!        <metadata key="weight" value="4.52"/>
//!        <metadata key="filament_maps" value="1"/>         slicer extruder per filament
//!        <filament id="1" type="PLA" used_m="1.49" used_g="4.52"/>
//!        <warning msg="..." level="1" error_code="..."/>
//!      </plate>
//!    </config>
//!    ```
//! 3. `Metadata/plate_<n>.json`, only for the extent of a file whose G-code has no moves
//!    to read: `bbox_all` is `[min x, min y, max x, max y]` of the objects on the plate.
//!
//! When two sources state the same number and differ by more than rounding, the G-code's
//! number is used and the result carries the warning `summary_disagrees`.
//!
//! Zip-bomb limits: the entry count and the central directory size are checked before the
//! archive library parses anything; every entry's name, declared size and compression ratio
//! and the total declared size are checked; encrypted entries, symbolic links, duplicate
//! names and overlapping entries are refused. Nothing is ever extracted to disk, and reads
//! are capped regardless of the declared sizes.

use std::io::{Read, Seek, SeekFrom};

use zip::ZipArchive;

use super::gcode::{self, Scan, ScanLimits};
use super::{OutputLimits, Parsed, number};
use crate::error::SlicerError;
use crate::job::{PrintedExtent, untrusted_text};
use crate::system::BedType;

const SLICE_INFO: &str = "Metadata/slice_info.config";
/// XML nodes accepted in `slice_info.config`.
const MAX_XML_NODES: u32 = 200_000;
/// Slicer warnings passed on from one file.
const MAX_SLICER_WARNINGS: usize = 32;

fn hostile(message: impl Into<String>) -> SlicerError {
    SlicerError::HostileOutput(message.into())
}

/// Parses the archive in `reader` for `plate`. `maps_key` is the plate metadata key that
/// reports the filament map, if the printer has one. `beds` are the plate types whose bed
/// temperature settings are known, for a file that commands no bed temperature.
pub fn parse<R: Read + Seek>(
    mut reader: R,
    plate: u32,
    maps_key: Option<&str>,
    beds: &[BedType],
    limits: &OutputLimits,
) -> Result<Parsed, SlicerError> {
    check_directory(&mut reader, limits)?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|e| hostile(format!("cannot be read: {e}")))?;
    let mut archive =
        ZipArchive::new(reader).map_err(|e| hostile(format!("not a readable zip archive: {e}")))?;
    check_entries(&mut archive, limits)?;

    let gcode_name = format!("Metadata/plate_{plate}.gcode");
    if archive.index_for_name(&gcode_name).is_none() {
        return Err(SlicerError::UnusableOutput(format!(
            "the archive holds no G-code for plate {plate} (`{gcode_name}`)"
        )));
    }

    let mut parsed = Parsed::default();
    match read_entry(&mut archive, SLICE_INFO, limits.max_metadata_bytes, true)? {
        Some(bytes) => parse_slice_info(&bytes, plate, maps_key, &mut parsed),
        None => parsed.warn(
            "slice_info_missing",
            format!("the sliced file has no `{SLICE_INFO}`; time and filament use are unknown"),
        ),
    }

    let scan = {
        let entry = archive
            .by_name(&gcode_name)
            .map_err(|e| hostile(format!("`{gcode_name}` cannot be read: {e}")))?;
        gcode::scan(
            entry,
            &ScanLimits {
                header_bytes: limits.max_gcode_header_bytes,
                scan_bytes: limits.max_gcode_scan_bytes,
                line_bytes: limits.max_line_bytes,
            },
        )
    };
    merge_gcode(&scan, beds, &mut parsed);
    if parsed.extent.is_none() {
        let plate_json = format!("Metadata/plate_{plate}.json");
        parsed.extent = read_entry(&mut archive, &plate_json, limits.max_metadata_bytes, true)?
            .and_then(|bytes| plate_outline(&bytes, scan.max_z));
    }
    if parsed.time_s.is_none() {
        parsed.warn(
            "time_unknown",
            "the sliced file does not state a print time",
        );
    }
    if parsed.layers.is_none() {
        parsed.warn(
            "layers_unknown",
            "the G-code header does not state the number of layers",
        );
    }
    Ok(parsed)
}

/// Opens the archive in `reader` with the same checks as [`parse`] and scans the G-code of
/// `plate` for the proof ([`crate::proof`]).
pub(crate) fn scan_for_proof<R: Read + Seek>(
    mut reader: R,
    plate: u32,
    facts: &crate::proof::GcodeFacts,
    limits: &OutputLimits,
) -> Result<crate::proof::ProofScan, SlicerError> {
    check_directory(&mut reader, limits)?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|e| hostile(format!("cannot be read: {e}")))?;
    let mut archive =
        ZipArchive::new(reader).map_err(|e| hostile(format!("not a readable zip archive: {e}")))?;
    check_entries(&mut archive, limits)?;
    let gcode_name = format!("Metadata/plate_{plate}.gcode");
    let entry = archive.by_name(&gcode_name).map_err(|_| {
        SlicerError::UnusableOutput(format!(
            "the archive holds no G-code for plate {plate} (`{gcode_name}`)"
        ))
    })?;
    Ok(crate::proof::scan(
        entry,
        facts,
        &ScanLimits {
            header_bytes: limits.max_gcode_header_bytes,
            scan_bytes: limits.max_gcode_scan_bytes,
            line_bytes: limits.max_line_bytes,
        },
    ))
}

/// Whether two statements of one number differ by more than rounding: more than `slack`
/// and more than 1 %.
fn differ(a: f64, b: f64, slack: f64) -> bool {
    (a - b).abs() > slack.max(0.01 * a.abs().max(b.abs()))
}

/// Puts what the G-code states into `parsed`, over what `slice_info.config` said.
fn merge_gcode(scan: &Scan, beds: &[BedType], parsed: &mut Parsed) {
    if scan.incomplete {
        parsed.warn(
            "gcode_scan_incomplete",
            "the G-code is longer than the scan limit or could not be read to its end; its \
             temperatures and printed extent are unknown",
        );
    }
    parsed.slicer_version = scan.slicer_version.clone();
    parsed.layers = scan.layers;
    if let Some(total) = scan.total_time_s {
        if let Some(info) = parsed.time_s
            && differ(total, info, 2.0)
        {
            parsed.warn(
                "summary_disagrees",
                format!(
                    "the G-code states a print time of {total} s, slice_info.config {info} s; \
                     the G-code's is used"
                ),
            );
        }
        parsed.time_s = Some(total);
    }
    // The header lists the filaments that are used and, parallel to them, length and
    // weight. Only a header whose three lists have the same length is believed.
    let ids = &scan.filament_ids;
    if !ids.is_empty() && ids.len() == scan.filament_g.len() && ids.len() == scan.filament_mm.len()
    {
        for (index, id) in ids.iter().enumerate() {
            let grams = scan.filament_g[index];
            let metres = scan.filament_mm[index] / 1000.0;
            if let Some((info_g, info_m)) = parsed.filaments.get(id)
                && (info_g.is_some_and(|g| differ(grams, g, 0.02))
                    || info_m.is_some_and(|m| differ(metres, m, 0.02)))
            {
                parsed.warn(
                    "summary_disagrees",
                    format!(
                        "the G-code and slice_info.config state different amounts for \
                         filament {id}; the G-code's are used"
                    ),
                );
            }
            parsed.filaments.insert(*id, (Some(grams), Some(metres)));
        }
    }
    parsed.density_missing = scan.filament_density.contains(&0.0);
    // The stand-in's estimate block: used only when it is whole.
    let estimate = &scan.estimate;
    if let (Some(time_s), Some(layers)) = (estimate.time_s, estimate.layers)
        && !ids.is_empty()
        && ids.len() == estimate.filament_g.len()
        && ids.len() == estimate.filament_mm.len()
    {
        parsed.stand_in = Some(super::StandInNumbers {
            time_s,
            layers,
            filaments: ids
                .iter()
                .enumerate()
                .map(|(index, id)| {
                    (
                        *id,
                        (
                            estimate.filament_g[index],
                            estimate.filament_mm[index] / 1000.0,
                        ),
                    )
                })
                .collect(),
            height_mm: estimate.height_mm,
        });
    }

    // The lowest and the highest number of a setting and of its form for the first layer.
    let range = |key: &str| -> Option<[f64; 2]> {
        let first_layer = format!("{key}_initial_layer");
        [key, first_layer.as_str()]
            .into_iter()
            .filter_map(|key| scan.config.get(key))
            .flat_map(|list| list.split(',').filter_map(number))
            .fold(None, |known: Option<[f64; 2]>, value| {
                Some(known.map_or([value, value], |[low, high]| {
                    [low.min(value), high.max(value)]
                }))
            })
    };
    parsed.bed_type = scan
        .config
        .get("curr_bed_type")
        .filter(|value| !value.is_empty())
        .map(|value| untrusted_text(value, 60));
    parsed.bed_setting_c = parsed
        .bed_type
        .as_deref()
        .and_then(|bed_type| beds.iter().find(|bed| bed.slicer_value == bed_type))
        .and_then(|bed| range(&bed.temperature_key));
    parsed.bed_commanded_c = scan.bed_max_c;
    parsed.bed_temperature_c = scan
        .bed_max_c
        .or(parsed.bed_setting_c.map(|[_, high]| high));
    parsed.nozzle_temperature_c = scan
        .nozzle_max_c
        .or_else(|| range("nozzle_temperature").map(|[_, high]| high));
    parsed.extent = scan.printed.map(|printed| PrintedExtent {
        x: [printed.min[0], printed.max[0]],
        y: [printed.min[1], printed.max[1]],
        z: Some([printed.min[2], printed.max[2]]),
        source: "gcode".to_owned(),
    });
}

/// The outline of the objects on the plate from `Metadata/plate_<n>.json`: `bbox_all` is
/// `[min x, min y, max x, max y]`. `max_z` is the height the G-code header states.
fn plate_outline(bytes: &[u8], max_z: Option<f64>) -> Option<PrintedExtent> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let list = value.get("bbox_all")?.as_array()?;
    let [min_x, min_y, max_x, max_y] = list.as_slice() else {
        return None;
    };
    let coordinate = |value: &serde_json::Value| {
        value
            .as_f64()
            .filter(|number| number.is_finite() && number.abs() <= 100_000.0)
    };
    let (min_x, min_y) = (coordinate(min_x)?, coordinate(min_y)?);
    let (max_x, max_y) = (coordinate(max_x)?, coordinate(max_y)?);
    (min_x <= max_x && min_y <= max_y).then(|| PrintedExtent {
        x: [min_x, max_x],
        y: [min_y, max_y],
        z: max_z.map(|max_z| [0.0, max_z]),
        source: "plate_metadata".to_owned(),
    })
}

/// Reads the end-of-central-directory record by hand and refuses an archive that declares
/// too many entries or too large a directory, before any library allocates for them.
///
/// The zip library is forgiving: when the last end record does not lead to a directory it
/// tries an earlier one, and it looks for a zip64 record anywhere between the place the
/// locator names and the locator. A check of "the" end record is therefore only worth
/// something if there is exactly one record the library could use. So the archive must
/// have the plain layout every writer produces, and is refused otherwise:
/// - one end record, whose comment ends the file, and no second end-record signature
///   anywhere the library searches (not in the comment, not after it);
/// - no disk spanning;
/// - if the end record defers to zip64: the locator directly before it, and the zip64
///   record directly before the locator, 56 bytes long, holding no second signature.
pub(crate) fn check_directory<R: Read + Seek>(
    reader: &mut R,
    limits: &OutputLimits,
) -> Result<(), SlicerError> {
    const EOCD: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    const LOCATOR: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
    const EOCD64: [u8; 4] = [0x50, 0x4b, 0x06, 0x06];
    const EOCD_LEN: usize = 22;
    const LOCATOR_LEN: usize = 20;
    const EOCD64_LEN: usize = 56;
    // The library searches the last 64 KiB and a little more; this covers it with room.
    const SEARCHED: u64 = EOCD_LEN as u64 + 65_536 + 64;
    let io = |e: std::io::Error| hostile(format!("cannot be read: {e}"));
    let len = reader.seek(SeekFrom::End(0)).map_err(io)?;
    if len < EOCD_LEN as u64 {
        return Err(hostile("too short to be a zip archive"));
    }
    let tail_len = len.min(SEARCHED);
    reader.seek(SeekFrom::Start(len - tail_len)).map_err(io)?;
    let mut tail = vec![0u8; tail_len as usize];
    reader.read_exact(&mut tail).map_err(io)?;

    let mut found = tail
        .windows(4)
        .enumerate()
        .filter(|(_, bytes)| *bytes == EOCD)
        .map(|(position, _)| position);
    let position = found
        .next()
        .ok_or_else(|| hostile("not a zip archive: no end-of-central-directory record"))?;
    if found.next().is_some() {
        return Err(hostile(
            "the archive has more than one end-of-central-directory record",
        ));
    }
    if position + EOCD_LEN > tail.len() {
        return Err(hostile("the end-of-central-directory record is cut off"));
    }
    let u16_at = |offset: usize| u64::from(u16::from_le_bytes([tail[offset], tail[offset + 1]]));
    let u32_at = |offset: usize| {
        u64::from(u32::from_le_bytes([
            tail[offset],
            tail[offset + 1],
            tail[offset + 2],
            tail[offset + 3],
        ]))
    };
    if position + EOCD_LEN + u16_at(position + 20) as usize != tail.len() {
        return Err(hostile(
            "the end-of-central-directory record does not end the file",
        ));
    }
    let spanned = || hostile("the archive claims to span several disks");
    if u16_at(position + 4) != 0
        || u16_at(position + 6) != 0
        || u16_at(position + 8) != u16_at(position + 10)
    {
        return Err(spanned());
    }
    let mut entries = u16_at(position + 10);
    let mut directory_bytes = u32_at(position + 12);
    let directory_offset = u32_at(position + 16);
    if entries == 0xFFFF || directory_bytes == 0xFFFF_FFFF || directory_offset == 0xFFFF_FFFF {
        // Zip64: the real numbers are in the zip64 record.
        let locator = position
            .checked_sub(LOCATOR_LEN)
            .filter(|&i| tail[i..i + 4] == LOCATOR)
            .ok_or_else(|| hostile("a zip64 archive without a zip64 locator"))?;
        let mut offset = [0u8; 8];
        offset.copy_from_slice(&tail[locator + 8..locator + 16]);
        let offset = u64::from_le_bytes(offset);
        let locator_offset = len - tail_len + locator as u64;
        if offset.checked_add(EOCD64_LEN as u64) != Some(locator_offset) {
            return Err(hostile(
                "the zip64 record is not directly before its locator",
            ));
        }
        reader.seek(SeekFrom::Start(offset)).map_err(io)?;
        let mut record = [0u8; EOCD64_LEN];
        reader.read_exact(&mut record).map_err(io)?;
        let u64_at = |at: usize| {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&record[at..at + 8]);
            u64::from_le_bytes(bytes)
        };
        if record[..4] != EOCD64
            || u64_at(4) != (EOCD64_LEN - 12) as u64
            || record[4..].windows(4).any(|bytes| bytes == EOCD64)
        {
            return Err(hostile("the zip64 record is malformed"));
        }
        // Disk numbers (two u32) and the entries on this disk against all entries.
        if u64_at(16) != 0 || u64_at(24) != u64_at(32) {
            return Err(spanned());
        }
        entries = u64_at(32);
        directory_bytes = u64_at(40);
    }
    if entries > limits.max_zip_entries {
        return Err(hostile(format!(
            "the archive declares {entries} entries; the limit is {}",
            limits.max_zip_entries
        )));
    }
    if directory_bytes > limits.max_central_directory_bytes || directory_bytes > len {
        return Err(hostile(format!(
            "the archive's directory is {directory_bytes} bytes; the limit is {}",
            limits.max_central_directory_bytes
        )));
    }
    Ok(())
}

/// Checks every entry's name and declared sizes.
pub(crate) fn check_entries<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    limits: &OutputLimits,
) -> Result<(), SlicerError> {
    if archive.len() as u64 > limits.max_zip_entries {
        return Err(hostile(format!(
            "the archive has {} entries; the limit is {}",
            archive.len(),
            limits.max_zip_entries
        )));
    }
    if archive.has_duplicate_names() {
        return Err(hostile("the archive has two entries with the same name"));
    }
    let mut total: u64 = 0;
    for index in 0..archive.len() {
        let entry = archive
            .by_index_data(index)
            .map_err(|e| hostile(format!("entry {index} cannot be read: {e}")))?;
        let name = std::str::from_utf8(entry.name_raw())
            .ok()
            .filter(|name| safe_entry_name(name))
            .ok_or_else(|| {
                hostile(format!(
                    "entry {index} has an unsafe name (`{}`)",
                    untrusted_text(&String::from_utf8_lossy(entry.name_raw()), 80)
                ))
            })?;
        if entry.is_symlink() || entry.encrypted() {
            return Err(hostile(format!(
                "`{name}` is a symbolic link or is encrypted"
            )));
        }
        let size = entry.size();
        if size > limits.max_entry_bytes {
            return Err(hostile(format!(
                "`{name}` declares {size} bytes; the limit per entry is {}",
                limits.max_entry_bytes
            )));
        }
        if size > 1024 * 1024
            && size / entry.compressed_size().max(1) > limits.max_compression_ratio
        {
            return Err(hostile(format!(
                "`{name}` declares a compression ratio above {}:1",
                limits.max_compression_ratio
            )));
        }
        total = total.saturating_add(size);
        if total > limits.max_total_bytes {
            return Err(hostile(format!(
                "the archive declares more than {} bytes in total",
                limits.max_total_bytes
            )));
        }
    }
    let overlapping = archive
        .has_overlapping_files()
        .map_err(|e| hostile(format!("the archive's layout cannot be checked: {e}")))?;
    if overlapping {
        return Err(hostile("two entries of the archive share the same bytes"));
    }
    Ok(())
}

/// A name is safe when it is a plain relative path with `/` separators: no empty, `.` or
/// `..` component, no backslash, colon, control character or leading slash. A single
/// trailing slash (a folder entry) is allowed.
fn safe_entry_name(name: &str) -> bool {
    let path = name.strip_suffix('/').unwrap_or(name);
    !path.is_empty()
        && path.len() <= 512
        && !path
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':'))
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Reads at most `cap` bytes of one entry. With `strict`, an entry longer than the cap is an
/// error; otherwise the rest is ignored. `Ok(None)` when the entry does not exist.
fn read_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    cap: u64,
    strict: bool,
) -> Result<Option<Vec<u8>>, SlicerError> {
    if archive.index_for_name(name).is_none() {
        return Ok(None);
    }
    let entry = archive
        .by_name(name)
        .map_err(|e| hostile(format!("`{name}` cannot be read: {e}")))?;
    let mut bytes = Vec::new();
    let read = entry.take(cap + 1).read_to_end(&mut bytes);
    if strict {
        read.map_err(|e| hostile(format!("`{name}` cannot be read: {e}")))?;
        if bytes.len() as u64 > cap {
            return Err(hostile(format!("`{name}` is larger than {cap} bytes")));
        }
    }
    // Not strict: a read that stops early (the cap, or a checksum the truncated read cannot
    // satisfy) still leaves the bytes read so far, which is all that is wanted.
    bytes.truncate(cap as usize);
    Ok(Some(bytes))
}

/// The child elements of `node` with the tag `tag`.
fn elements<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    tag: &str,
) -> Vec<roxmltree::Node<'a, 'input>> {
    node.children()
        .filter(|child| child.is_element() && child.tag_name().name() == tag)
        .collect()
}

/// The value of `<metadata key="..." value="..."/>` below `node`.
fn metadata(node: roxmltree::Node<'_, '_>, key: &str) -> Option<String> {
    elements(node, "metadata")
        .into_iter()
        .find(|item| item.attribute("key") == Some(key))
        .and_then(|item| item.attribute("value").map(str::to_owned))
}

fn parse_slice_info(bytes: &[u8], plate: u32, maps_key: Option<&str>, parsed: &mut Parsed) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        parsed.warn(
            "slice_info_unreadable",
            format!("`{SLICE_INFO}` is not UTF-8 text"),
        );
        return;
    };
    let options = roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: MAX_XML_NODES,
        ..roxmltree::ParsingOptions::default()
    };
    let document = match roxmltree::Document::parse_with_options(text, options) {
        Ok(document) => document,
        Err(error) => {
            parsed.warn(
                "slice_info_unreadable",
                format!(
                    "`{SLICE_INFO}` is not well-formed XML: {}",
                    untrusted_text(&error.to_string(), 160)
                ),
            );
            return;
        }
    };
    let plates = elements(document.root_element(), "plate");
    let wanted = plate.to_string();
    let chosen = plates
        .iter()
        .find(|node| metadata(**node, "index").as_deref().map(str::trim) == Some(wanted.as_str()))
        .or_else(|| {
            // One plate without an index can only be the plate that was sliced.
            (plates.len() == 1 && metadata(plates[0], "index").is_none()).then(|| &plates[0])
        });
    let Some(&node) = chosen else {
        parsed.warn(
            "slice_info_missing",
            format!("`{SLICE_INFO}` has no entry for plate {plate}"),
        );
        return;
    };

    parsed.time_s = metadata(node, "prediction").as_deref().and_then(number);

    for filament in elements(node, "filament") {
        let Some(id) = filament
            .attribute("id")
            .and_then(|id| id.trim().parse::<u32>().ok())
            .filter(|id| (1..=64).contains(id))
        else {
            parsed.warn("slice_info_unreadable", "a filament entry has no usable id");
            continue;
        };
        let usage = (
            filament.attribute("used_g").and_then(number),
            filament.attribute("used_m").and_then(number),
        );
        if parsed.filaments.insert(id, usage).is_some() {
            // Two entries for one filament: neither can be trusted.
            parsed.filaments.insert(id, (None, None));
            parsed.warn(
                "slice_info_unreadable",
                format!("filament {id} is listed twice; its use is unknown"),
            );
        }
    }

    if let Some(key) = maps_key {
        parsed.reported_map = metadata(node, key).and_then(|value| {
            let numbers: Option<Vec<u32>> = value
                .split([' ', ',', ';'])
                .filter(|token| !token.is_empty())
                .map(|token| token.parse::<u32>().ok())
                .collect();
            numbers.filter(|numbers| !numbers.is_empty() && numbers.len() <= 64)
        });
    }

    for warning in elements(node, "warning")
        .into_iter()
        .take(MAX_SLICER_WARNINGS)
    {
        let attribute = |name: &str| warning.attribute(name).unwrap_or("");
        let text = format!(
            "{} (level {}, code {})",
            attribute("msg"),
            attribute("level"),
            attribute("error_code")
        );
        parsed.warn("slicer_warning", untrusted_text(&text, 300));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used)]
    use std::io::{Cursor, Write};

    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::*;

    pub(crate) const SLICE_INFO_XML: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<config>
  <header>
    <header_item key="X-BBL-Client-Type" value="slicer"/>
    <header_item key="X-BBL-Client-Version" value="02.05.03.00"/>
  </header>
  <plate>
    <metadata key="index" value="1"/>
    <metadata key="some_future_key" value="whatever"/>
    <metadata key="prediction" value="5450"/>
    <metadata key="weight" value="29.41"/>
    <metadata key="filament_maps" value="2 1"/>
    <object identify_id="130" name="box" skipped="false"/>
    <filament id="1" tray_info_idx="GFA00" type="PLA" color="#FFFFFF" used_m="9.02" used_g="26.91" future="x"/>
    <filament id="2" type="PETG" color="#000000" used_m="0.84" used_g="2.50"/>
    <warning msg="bed_temperature_too_high_than_filament" level="1" error_code="1000C001"/>
    <something_new><nested/></something_new>
  </plate>
</config>
"##;

    const GCODE: &str = "; HEADER_BLOCK_START\n; BambuStudio 02.05.03.00\n; total layer number: 50\n; HEADER_BLOCK_END\nG1 X1\n";

    pub(crate) fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn sliced(slice_info: &str, gcode: &str) -> Vec<u8> {
        archive(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("Metadata/plate_1.gcode", gcode.as_bytes()),
            (SLICE_INFO, slice_info.as_bytes()),
        ])
    }

    fn beds() -> Vec<BedType> {
        crate::system::SystemProfiles::builtin().unwrap().bed_types
    }

    fn parse_bytes(bytes: Vec<u8>) -> Result<Parsed, SlicerError> {
        parse(
            Cursor::new(bytes),
            1,
            Some("filament_maps"),
            &beds(),
            &OutputLimits::default(),
        )
    }

    fn codes(parsed: &Parsed) -> Vec<&str> {
        parsed.warnings.iter().map(|w| w.code.as_str()).collect()
    }

    #[test]
    fn a_complete_file_is_read_and_unknown_fields_are_ignored() {
        let parsed = parse_bytes(sliced(SLICE_INFO_XML, GCODE)).unwrap();
        assert_eq!(parsed.time_s, Some(5450.0));
        assert_eq!(parsed.layers, Some(50));
        assert_eq!(parsed.filaments[&1], (Some(26.91), Some(9.02)));
        assert_eq!(parsed.filaments[&2], (Some(2.5), Some(0.84)));
        assert_eq!(parsed.reported_map, Some(vec![2, 1]));
        assert_eq!(codes(&parsed), ["slicer_warning"]);
        assert_eq!(
            parsed.warnings[0].message,
            "bed_temperature_too_high_than_filament (level 1, code 1000C001)"
        );
    }

    #[test]
    fn missing_fields_stay_unknown_and_are_reported() {
        let no_time = SLICE_INFO_XML.replace(r#"<metadata key="prediction" value="5450"/>"#, "");
        let parsed = parse_bytes(sliced(&no_time, GCODE)).unwrap();
        assert_eq!(parsed.time_s, None);
        assert_eq!(parsed.layers, Some(50));
        assert!(codes(&parsed).contains(&"time_unknown"));

        let bad_numbers = SLICE_INFO_XML
            .replace(r#"value="5450""#, r#"value="soon""#)
            .replace(r#"used_g="26.91""#, r#"used_g="-3""#)
            .replace(r#"used_m="0.84" "#, "")
            .replace(r#"value="2 1""#, r#"value="left right""#);
        let parsed = parse_bytes(sliced(&bad_numbers, "G1 X1\n")).unwrap();
        assert_eq!(parsed.time_s, None);
        assert_eq!(parsed.filaments[&1], (None, Some(9.02)));
        assert_eq!(parsed.filaments[&2], (Some(2.5), None));
        assert_eq!(parsed.reported_map, None);
        assert_eq!(parsed.layers, None);
        assert!(codes(&parsed).contains(&"layers_unknown"));

        let no_info = archive(&[("Metadata/plate_1.gcode", GCODE.as_bytes())]);
        let parsed = parse_bytes(no_info).unwrap();
        assert_eq!((parsed.time_s, parsed.layers), (None, Some(50)));
        assert!(parsed.filaments.is_empty());
        assert!(codes(&parsed).contains(&"slice_info_missing"));

        let other_plate =
            SLICE_INFO_XML.replace(r#"key="index" value="1""#, r#"key="index" value="2""#);
        let parsed = parse_bytes(sliced(&other_plate, GCODE)).unwrap();
        assert_eq!(parsed.time_s, None);
        assert!(codes(&parsed).contains(&"slice_info_missing"));

        let twice = SLICE_INFO_XML.replace(r#"<filament id="2""#, r#"<filament id="1""#);
        let parsed = parse_bytes(sliced(&twice, GCODE)).unwrap();
        assert_eq!(parsed.filaments[&1], (None, None));
    }

    #[test]
    fn malformed_or_malicious_xml_gives_no_numbers() {
        let bomb = r#"<?xml version="1.0"?>
<!DOCTYPE config [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;">]>
<config><plate><metadata key="index" value="1"/><metadata key="prediction" value="&b;"/></plate></config>"#;
        for xml in [bomb, "<config><plate>", "not xml at all", ""] {
            let parsed = parse_bytes(sliced(xml, GCODE)).unwrap();
            assert_eq!(parsed.time_s, None, "{xml}");
            assert!(parsed.filaments.is_empty());
            assert!(codes(&parsed).contains(&"slice_info_unreadable"), "{xml}");
        }
        let not_utf8 = archive(&[
            ("Metadata/plate_1.gcode", GCODE.as_bytes()),
            (SLICE_INFO, &[0xff, 0xfe, 0x00]),
        ]);
        assert!(codes(&parse_bytes(not_utf8).unwrap()).contains(&"slice_info_unreadable"));
    }

    #[test]
    fn a_file_without_gcode_for_the_plate_is_not_a_result() {
        let bytes = archive(&[(SLICE_INFO, SLICE_INFO_XML.as_bytes())]);
        assert_eq!(parse_bytes(bytes).unwrap_err().code(), "unusable_output");
    }

    #[test]
    fn things_that_are_not_zip_archives_are_rejected() {
        for bytes in [
            Vec::new(),
            b"PK".to_vec(),
            b"; plain gcode, not an archive\nG1 X1\n".repeat(10),
            vec![0u8; 100_000],
        ] {
            assert_eq!(parse_bytes(bytes).unwrap_err().code(), "hostile_output");
        }
        // A valid archive cut in half.
        let mut cut = sliced(SLICE_INFO_XML, GCODE);
        cut.truncate(cut.len() / 2);
        assert_eq!(parse_bytes(cut).unwrap_err().code(), "hostile_output");
    }

    #[test]
    fn path_traversal_names_are_rejected() {
        for name in [
            "../evil.gcode",
            "Metadata/../../evil",
            "/etc/passwd",
            "C:\\Windows\\evil",
            "c:/x",
            "a//b",
            "./a",
            "a/./b",
            "a\\..\\b",
            "nul\0byte",
            "line\nbreak",
        ] {
            assert!(!safe_entry_name(name), "{name:?}");
            let bytes = archive(&[
                ("Metadata/plate_1.gcode", GCODE.as_bytes()),
                (SLICE_INFO, SLICE_INFO_XML.as_bytes()),
                (name, b"x"),
            ]);
            let error = parse_bytes(bytes).unwrap_err();
            assert_eq!(error.code(), "hostile_output", "{name:?}");
            assert!(error.to_string().contains("unsafe name"), "{error}");
        }
        for name in [
            "Metadata/plate_1.gcode",
            "3D/3dmodel.model",
            "[Content_Types].xml",
            "Metadata/",
        ] {
            assert!(safe_entry_name(name), "{name}");
        }
    }

    #[test]
    fn too_many_entries_are_rejected_before_the_archive_is_parsed() {
        let mut entries: Vec<(String, &[u8])> =
            (0..40).map(|i| (format!("f{i}"), &b"x"[..])).collect();
        entries.push(("Metadata/plate_1.gcode".into(), GCODE.as_bytes()));
        let borrowed: Vec<(&str, &[u8])> = entries.iter().map(|(n, c)| (n.as_str(), *c)).collect();
        let bytes = archive(&borrowed);
        let limits = OutputLimits {
            max_zip_entries: 40,
            ..OutputLimits::default()
        };
        let error = parse(Cursor::new(bytes.clone()), 1, None, &beds(), &limits).unwrap_err();
        assert!(error.to_string().contains("declares 41 entries"), "{error}");
        let roomy = OutputLimits {
            max_zip_entries: 41,
            ..OutputLimits::default()
        };
        assert!(parse(Cursor::new(bytes), 1, None, &beds(), &roomy).is_ok());

        // A record that claims 65535 entries (the zip64 marker) without a zip64 locator.
        let mut lying = sliced(SLICE_INFO_XML, GCODE);
        let eocd = lying.len() - 22;
        lying[eocd + 10..eocd + 12].copy_from_slice(&[0xff, 0xff]);
        assert_eq!(parse_bytes(lying).unwrap_err().code(), "hostile_output");

        // A directory larger than allowed.
        let tight = OutputLimits {
            max_central_directory_bytes: 64,
            ..OutputLimits::default()
        };
        let error = parse(
            Cursor::new(sliced(SLICE_INFO_XML, GCODE)),
            1,
            None,
            &beds(),
            &tight,
        )
        .unwrap_err();
        assert!(error.to_string().contains("directory"), "{error}");
    }

    /// An end record with the given entry count and central directory offset.
    fn end_record(entries: u16, directory_offset: u32, comment: &[u8]) -> Vec<u8> {
        let mut record = vec![0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0];
        record.extend(entries.to_le_bytes());
        record.extend(entries.to_le_bytes());
        record.extend(46u32.to_le_bytes());
        record.extend(directory_offset.to_le_bytes());
        record.extend((comment.len() as u16).to_le_bytes());
        record.extend(comment);
        record
    }

    #[test]
    fn a_decoy_end_record_does_not_get_an_archive_past_the_limits() {
        let mut entries: Vec<(String, &[u8])> =
            (0..40).map(|i| (format!("f{i}"), &b"x"[..])).collect();
        entries.push(("Metadata/plate_1.gcode".into(), GCODE.as_bytes()));
        let borrowed: Vec<(&str, &[u8])> = entries.iter().map(|(n, c)| (n.as_str(), *c)).collect();
        let over_the_limit = archive(&borrowed);
        let limits = OutputLimits {
            max_zip_entries: 40,
            ..OutputLimits::default()
        };
        let refused_early = |bytes: Vec<u8>, what: &str| {
            let error = parse(Cursor::new(bytes), 1, None, &beds(), &limits).unwrap_err();
            assert_eq!(error.code(), "hostile_output", "{what}");
            let message = error.to_string();
            assert!(
                !message.contains("the archive has 41 entries"),
                "{what}: the library parsed the directory before the archive was refused"
            );
            message
        };
        // A decoy that claims one entry and leads nowhere: the library gives up on it and
        // uses the real record before it.
        let decoy = end_record(1, u32::MAX - 1, b"");

        // After the real record, as trailing bytes.
        let mut trailing = over_the_limit.clone();
        trailing.extend(&decoy);
        let message = refused_early(trailing, "trailing decoy");
        assert!(
            message.contains("more than one end-of-central"),
            "{message}"
        );

        // Inside the comment of the real record.
        let mut in_comment = over_the_limit.clone();
        let eocd = in_comment.len() - 22;
        in_comment[eocd + 20..eocd + 22].copy_from_slice(&(decoy.len() as u16).to_le_bytes());
        in_comment.extend(&decoy);
        let message = refused_early(in_comment, "decoy in the comment");
        assert!(
            message.contains("more than one end-of-central"),
            "{message}"
        );

        // A record that does not end the file, and one that claims other disks.
        let mut padded = over_the_limit.clone();
        padded.extend([0u8; 16]);
        let message = refused_early(padded, "trailing bytes");
        assert!(message.contains("does not end the file"), "{message}");
        let mut spanned = sliced(SLICE_INFO_XML, GCODE);
        let eocd = spanned.len() - 22;
        spanned[eocd + 4] = 1;
        let error = parse_bytes(spanned).unwrap_err();
        assert!(error.to_string().contains("several disks"), "{error}");

        // An honest comment is fine.
        let mut commented = sliced(SLICE_INFO_XML, GCODE);
        let eocd = commented.len() - 22;
        commented[eocd + 20..eocd + 22].copy_from_slice(&5u16.to_le_bytes());
        commented.extend(b"hello");
        assert!(parse_bytes(commented).is_ok());
    }

    /// The zip64 tail of an archive: a zip64 record, its locator and an end record that
    /// defers to them.
    fn zip64_tail(record_offset: u64, entries: u64, directory_bytes: u64) -> Vec<u8> {
        let mut tail = vec![0x50, 0x4b, 0x06, 0x06];
        tail.extend(44u64.to_le_bytes());
        tail.extend([45, 0, 45, 0]);
        tail.extend([0u8; 8]);
        tail.extend(entries.to_le_bytes());
        tail.extend(entries.to_le_bytes());
        tail.extend(directory_bytes.to_le_bytes());
        tail.extend(0u64.to_le_bytes());
        tail.extend([0x50, 0x4b, 0x06, 0x07, 0, 0, 0, 0]);
        tail.extend(record_offset.to_le_bytes());
        tail.extend(1u32.to_le_bytes());
        tail.extend(end_record(0xFFFF, 0xFFFF_FFFF, b""));
        tail
    }

    #[test]
    fn the_zip64_record_is_the_one_before_its_locator_or_the_archive_is_refused() {
        let limits = OutputLimits::default();
        let check = |bytes: Vec<u8>| check_directory(&mut Cursor::new(bytes), &limits);
        let body = vec![0u8; 100];

        let mut honest = vec![0u8; 300];
        honest.extend(zip64_tail(300, 3, 200));
        check(honest).unwrap();

        let mut huge = body.clone();
        huge.extend(zip64_tail(100, 50_000_000, 200));
        let error = check(huge).unwrap_err().to_string();
        assert!(error.contains("declares 50000000 entries"), "{error}");

        // The locator names a small record further up; a second record with the real,
        // huge numbers sits between it and the locator, where the library also looks.
        let mut decoy = body.clone();
        let small = zip64_tail(100, 1, 46);
        decoy.extend(&small[..56]);
        decoy.extend(zip64_tail(100, 50_000_000, 200));
        let error = check(decoy).unwrap_err().to_string();
        assert!(error.contains("not directly before its locator"), "{error}");

        // A record with room for more data after its fixed part, or a second signature
        // inside it.
        let mut long = body.clone();
        let mut tail = zip64_tail(100, 3, 46);
        tail[4..12].copy_from_slice(&45u64.to_le_bytes());
        long.extend(tail);
        assert!(check(long).unwrap_err().to_string().contains("malformed"));
        let mut nested = body.clone();
        let mut tail = zip64_tail(100, 3, 46);
        tail[48..52].copy_from_slice(&[0x50, 0x4b, 0x06, 0x06]);
        nested.extend(tail);
        assert!(check(nested).unwrap_err().to_string().contains("malformed"));

        let mut spanned = body;
        let mut tail = zip64_tail(100, 3, 46);
        tail[24..32].copy_from_slice(&2u64.to_le_bytes());
        spanned.extend(tail);
        assert!(
            check(spanned)
                .unwrap_err()
                .to_string()
                .contains("several disks")
        );
    }

    #[test]
    fn oversized_and_overcompressed_entries_are_rejected() {
        let big = vec![b'G'; 3 * 1024 * 1024];
        let bytes = archive(&[
            ("Metadata/plate_1.gcode", GCODE.as_bytes()),
            ("Metadata/big.bin", &big),
        ]);
        // Three megabytes of one letter compress far better than 200:1.
        let error = parse_bytes(bytes.clone()).unwrap_err();
        assert!(error.to_string().contains("compression ratio"), "{error}");

        let lenient_ratio = OutputLimits {
            max_compression_ratio: 100_000,
            ..OutputLimits::default()
        };
        assert!(parse(Cursor::new(bytes.clone()), 1, None, &beds(), &lenient_ratio).is_ok());
        let per_entry = OutputLimits {
            max_entry_bytes: 1024 * 1024,
            ..lenient_ratio
        };
        let error = parse(Cursor::new(bytes.clone()), 1, None, &beds(), &per_entry).unwrap_err();
        assert!(error.to_string().contains("limit per entry"), "{error}");
        let total = OutputLimits {
            max_total_bytes: 3 * 1024 * 1024,
            ..lenient_ratio
        };
        let error = parse(Cursor::new(bytes), 1, None, &beds(), &total).unwrap_err();
        assert!(error.to_string().contains("in total"), "{error}");
    }

    #[test]
    fn metadata_is_never_read_past_its_cap() {
        // A slice_info that is valid XML but larger than the cap is refused, not truncated.
        let padding = format!("<!-- {} -->", "x".repeat(4096));
        let large = SLICE_INFO_XML.replace("<config>", &format!("<config>{padding}"));
        let limits = OutputLimits {
            max_metadata_bytes: 1024,
            ..OutputLimits::default()
        };
        let error = parse(
            Cursor::new(sliced(&large, GCODE)),
            1,
            None,
            &beds(),
            &limits,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("larger than 1024 bytes"),
            "{error}"
        );

        // The layer line is only looked for in the capped header.
        let late = format!("{}; total layer number: 7\n", "; filler\n".repeat(200));
        let short_header = OutputLimits {
            max_gcode_header_bytes: 256,
            ..OutputLimits::default()
        };
        let parsed = parse(
            Cursor::new(sliced(SLICE_INFO_XML, &late)),
            1,
            None,
            &beds(),
            &short_header,
        )
        .unwrap();
        assert_eq!(parsed.layers, None);
        let parsed = parse_bytes(sliced(SLICE_INFO_XML, &late)).unwrap();
        assert_eq!(parsed.layers, Some(7));
    }

    #[test]
    fn duplicate_names_are_rejected() {
        // Build an archive with two different names of equal length, then make the second
        // name equal to the first in both the local header and the directory.
        let mut bytes = archive(&[
            ("Metadata/plate_1.gcode", GCODE.as_bytes()),
            ("Metadata/plate_2.gcode", b"; other\n"),
        ]);
        let needle = b"Metadata/plate_2.gcode";
        let mut replaced = 0;
        let mut i = 0;
        while i + needle.len() <= bytes.len() {
            if &bytes[i..i + needle.len()] == needle {
                bytes[i..i + needle.len()].copy_from_slice(b"Metadata/plate_1.gcode");
                replaced += 1;
            }
            i += 1;
        }
        assert_eq!(replaced, 2);
        let error = parse_bytes(bytes).unwrap_err();
        assert_eq!(error.code(), "hostile_output");
        assert!(error.to_string().contains("same name"), "{error}");
    }

    /// A whole file in the measured layout, written for this test.
    fn real_shaped(gcode: &str, plate_json: Option<&str>) -> Vec<u8> {
        let info = r##"<?xml version="1.0" encoding="UTF-8"?>
<config>
  <header>
    <header_item key="X-BBL-Client-Type" value="slicer"/>
    <header_item key="X-BBL-Client-Version" value="02.09.01.70"/>
  </header>
  <plate>
    <metadata key="index" value="1"/>
    <metadata key="prediction" value="3800"/>
    <metadata key="weight" value="4.74"/>
    <metadata key="filament_maps" value="2 1"/>
    <filament id="1" type="PLA" color="#00AA00" used_m="1.25" used_g="3.79"/>
    <filament id="2" type="PETG" color="#0000AA" used_m="0.30" used_g="0.95"/>
  </plate>
</config>
"##;
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("Metadata/plate_1.gcode", gcode.as_bytes()),
            (SLICE_INFO, info.as_bytes()),
        ];
        if let Some(json) = plate_json {
            entries.push(("Metadata/plate_1.json", json.as_bytes()));
        }
        archive(&entries)
    }

    #[test]
    fn the_gcode_is_believed_before_the_metadata() {
        let gcode = crate::output::gcode::tests::SLICED;
        let plate_json = r#"{"bbox_all": [0.0, 30.0, 61.0, 71.0], "bed_type": "x", "version": 2}"#;
        let parsed = parse_bytes(real_shaped(gcode, Some(plate_json))).unwrap();
        assert_eq!(parsed.slicer_version.as_deref(), Some("02.09.01.70"));
        assert_eq!((parsed.time_s, parsed.layers), (Some(3800.0), Some(3)));
        // Length from the header's millimetres, weight from its grams.
        assert_eq!(parsed.filaments[&1], (Some(3.79), Some(1.2505)));
        assert_eq!(parsed.filaments[&2], (Some(0.95), Some(0.30025)));
        assert_eq!(parsed.reported_map, Some(vec![2, 1]));
        assert_eq!(parsed.bed_type.as_deref(), Some("Textured PEI Plate"));
        // The commanded temperatures, not the settings (55,60 and 220,240).
        assert_eq!(parsed.bed_temperature_c, Some(60.0));
        assert_eq!(parsed.nozzle_temperature_c, Some(240.0));
        let extent = parsed.extent.clone().unwrap();
        assert_eq!(extent.source, "gcode");
        assert_eq!((extent.x, extent.y), ([0.25, 60.5], [30.25, 70.75]));
        assert_eq!(extent.z, Some([0.2, 0.6]));
        assert!(!parsed.density_missing);
        assert!(codes(&parsed).is_empty(), "{:?}", parsed.warnings);

        // Sources that disagree: the G-code wins and the result says so.
        let other_time = gcode.replace("1h 3m 20s", "2h 3m 20s");
        let parsed = parse_bytes(real_shaped(&other_time, None)).unwrap();
        assert_eq!(parsed.time_s, Some(7400.0));
        assert_eq!(codes(&parsed), ["summary_disagrees"]);
        let other_weight = gcode.replace("3.79,0.95", "5.79,0.95");
        let parsed = parse_bytes(real_shaped(&other_weight, None)).unwrap();
        assert_eq!(parsed.filaments[&1].0, Some(5.79));
        assert_eq!(codes(&parsed), ["summary_disagrees"]);
        // Header lists of different lengths are not believed; the metadata stands.
        let uneven = gcode.replace("; filament: 1,2", "; filament: 1");
        let parsed = parse_bytes(real_shaped(&uneven, None)).unwrap();
        assert_eq!(parsed.filaments[&1], (Some(3.79), Some(1.25)));
        // A density of 0 is what settings that were not flattened leave behind.
        let no_density = gcode.replace("filament_density: 1.26,1.31", "filament_density: 0,1.31");
        assert!(
            parse_bytes(real_shaped(&no_density, None))
                .unwrap()
                .density_missing
        );
    }

    #[test]
    fn a_file_without_moves_states_its_temperatures_in_the_settings_and_its_outline_in_the_plate_file()
     {
        let gcode = "; HEADER_BLOCK_START\n; total layer number: 5\n; max_z_height: 1.00\n\
                     ; HEADER_BLOCK_END\n; CONFIG_BLOCK_START\n; curr_bed_type = Cool Plate\n\
                     ; cool_plate_temp = 35\n; cool_plate_temp_initial_layer = 40\n\
                     ; textured_plate_temp = 55\n; nozzle_temperature = 220,230\n\
                     ; CONFIG_BLOCK_END\n";
        let plate_json = r#"{"bbox_all": [2.5, 3.5, 22.5, 33.5]}"#;
        let parsed = parse_bytes(real_shaped(gcode, Some(plate_json))).unwrap();
        assert_eq!(parsed.bed_type.as_deref(), Some("Cool Plate"));
        // The setting of the plate that was sliced for, not of another plate.
        assert_eq!(parsed.bed_temperature_c, Some(40.0));
        assert_eq!(parsed.nozzle_temperature_c, Some(230.0));
        let extent = parsed.extent.unwrap();
        assert_eq!(extent.source, "plate_metadata");
        assert_eq!(
            (extent.x, extent.y, extent.z),
            ([2.5, 22.5], [3.5, 33.5], Some([0.0, 1.0]))
        );
        // Time and filament use fall back to slice_info.config.
        assert_eq!(parsed.time_s, Some(3800.0));
        assert_eq!(parsed.filaments[&2], (Some(0.95), Some(0.3)));

        // A plate type this build does not know: no temperature is made up.
        let unknown = gcode.replace("Cool Plate", "Glass Plate");
        let parsed = parse_bytes(real_shaped(&unknown, None)).unwrap();
        assert_eq!(parsed.bed_type.as_deref(), Some("Glass Plate"));
        assert_eq!((parsed.bed_temperature_c, parsed.extent), (None, None));
        // Outlines that are not four sensible numbers are not an extent.
        for bad in [
            r#"{"bbox_all": [1, 2, 3]}"#,
            r#"{"bbox_all": [5, 2, 1, 4]}"#,
            r#"{"bbox_all": ["a", 2, 3, 4]}"#,
            r#"{"bbox_all": [1e300, 2, 1e301, 4]}"#,
            r#"{"other": 1}"#,
            "not json",
        ] {
            let parsed = parse_bytes(real_shaped(gcode, Some(bad))).unwrap();
            assert_eq!(parsed.extent, None, "{bad}");
        }
    }
}
