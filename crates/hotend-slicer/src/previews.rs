//! Plate previews made by Hotend, for a sliced file the slicer wrote without them.
//!
//! Why: Bambu Connect accepted a sliced file without `Metadata/plate_<n>.png` in its
//! import dialog and then imported nothing (2026-11-21), and the Linux Bambu Studio
//! cannot draw the previews headless. Its GLFW 3.3.7 is built for Wayland only ("Wayland:
//! Failed to connect to display", then "glfwInit return error"), so a virtual X display
//! does not help. A Wayland compositor inside the sandbox would need `AF_UNIX` sockets,
//! which the sandbox denies the slicer.
//!
//! What: when the setting asks for it (off by default), the plate's own renders that
//! Hotend's engine drew when it laid the plate out are scaled to the sizes Bambu Studio
//! writes, and put into the sliced file. This happens before the file is hashed, checked
//! and recorded, so the hash, the G-code check, the proof and the review are all of the
//! file with the previews. The result carries the warning [`WARNING_CODE`], whose text
//! begins with [`WARNING_TEXT`].
//!
//! Guarantees:
//! - nothing is added when the file already has `Metadata/plate_<n>.png`: previews the
//!   slicer drew are never replaced;
//! - every entry except the three index files below is copied as it is, compressed bytes
//!   included (so `plate_<n>.gcode` and its `.md5` are byte-identical);
//! - `Metadata/model_settings.config` gains the four lines Bambu Studio writes into the
//!   plate's block after its `gcode_file` line (`thumbnail_file`,
//!   `thumbnail_no_light_file`, `top_file`, `pick_file`), each only when missing;
//!   `[Content_Types].xml` gains the `png` type and `_rels/.rels` the thumbnail
//!   relationships Bambu Studio writes, each only when missing;
//! - the file is read as hostile input, with the limits of the output parser, and replaced
//!   only by a complete new archive (written beside it, then renamed).
//!
//! Names and sizes measured from a sliced file Bambu Studio 02.09.01.70 made on Windows
//! (2026-11-17): `plate_<n>.png`, `plate_no_light_<n>.png`, `top_<n>.png` and
//! `pick_<n>.png` are 512 x 512, `plate_<n>_small.png` 128 x 128, all 8-bit RGBA, stored
//! uncompressed directly after `[Content_Types].xml`. Hotend has no unlit render and no
//! object-id picture: `plate_no_light_<n>.png` is the iso view and `pick_<n>.png` the top
//! view.

use std::io::{BufReader, Cursor, Read, Seek, Write};
use std::path::Path;

use zip::ZipArchive;
use zip::write::{SimpleFileOptions, ZipWriter};

use crate::error::SlicerError;
use crate::job::PlatePreviews;
use crate::output::OutputLimits;
use crate::output::bambu::{check_directory, check_entries};

/// Code of the warning a slice with Hotend-made previews carries.
pub const WARNING_CODE: &str = hotend_core::HOTEND_MADE_PREVIEWS;
/// The words the warning starts with, shown in the review and the confirmation.
pub const WARNING_TEXT: &str = "plate previews made by Hotend";

/// Largest render read.
const MAX_RENDER_BYTES: u64 = 16 * 1024 * 1024;
/// Largest render side, in pixels.
const MAX_RENDER_SIDE: u32 = 8192;
/// Side of the large previews.
const LARGE: u32 = 512;
/// Side of the small preview.
const SMALL: u32 = 128;

/// The warning's full text.
pub fn warning_message() -> String {
    format!(
        "{WARNING_TEXT}: the slicer drew no plate preview, so the pictures in this file \
         that Bambu Connect and the printer show are Hotend's own renders of the plate \
         (made when the plate was laid out), not Bambu Studio's"
    )
}

/// The preview entries for plate `plate` and the render each is made from.
fn entries(plate: u32) -> [(String, View, u32); 5] {
    [
        (format!("Metadata/plate_{plate}.png"), View::Iso, LARGE),
        (
            format!("Metadata/plate_{plate}_small.png"),
            View::Iso,
            SMALL,
        ),
        (
            format!("Metadata/plate_no_light_{plate}.png"),
            View::Iso,
            LARGE,
        ),
        (format!("Metadata/top_{plate}.png"), View::Top, LARGE),
        (format!("Metadata/pick_{plate}.png"), View::Top, LARGE),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Iso,
    Top,
}

/// Puts previews made from `previews` into the sliced file at `path` for plate `plate`,
/// unless it already has `Metadata/plate_<plate>.png`. `Ok(true)` when they were added.
pub fn add(
    path: &Path,
    plate: u32,
    previews: &PlatePreviews,
    limits: &OutputLimits,
) -> Result<bool, SlicerError> {
    // The renders first: a missing or broken render changes nothing.
    let iso = Rgba::load(&previews.iso)?;
    let top = Rgba::load(&previews.top)?;
    let original = read_capped(path, limits.max_file_bytes)?;
    let Some(rebuilt) = rebuild(&original, plate, &iso, &top, limits)? else {
        return Ok(false);
    };
    let part = path.with_extension("previews.part");
    let written = (|| {
        let mut file = std::fs::File::create(&part)?;
        file.write_all(&rebuilt)?;
        file.sync_all()?;
        std::fs::rename(&part, path)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&part);
        return Err(SlicerError::io(
            "write the plate previews into",
            path,
            error,
        ));
    }
    Ok(true)
}

/// The archive `original` with the previews of plate `plate` added, or `None` when it
/// already has `Metadata/plate_<plate>.png`.
fn rebuild(
    original: &[u8],
    plate: u32,
    iso: &Rgba,
    top: &Rgba,
    limits: &OutputLimits,
) -> Result<Option<Vec<u8>>, SlicerError> {
    let mut reader = Cursor::new(original);
    check_directory(&mut reader, limits)?;
    reader
        .rewind()
        .map_err(|e| unusable(format!("cannot be read: {e}")))?;
    let mut archive = ZipArchive::new(reader)
        .map_err(|e| unusable(format!("not a readable zip archive: {e}")))?;
    check_entries(&mut archive, limits)?;
    let wanted = entries(plate);
    if archive.index_for_name(&wanted[0].0).is_some() {
        return Ok(None);
    }
    let mut pictures = Vec::new();
    for (name, view, side) in &wanted {
        if archive.index_for_name(name).is_none() {
            let source = if *view == View::Iso { iso } else { top };
            pictures.push((name.clone(), source.scaled(*side).encode()?));
        }
    }
    let gcode = format!("Metadata/plate_{plate}.gcode");
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let write_pictures = |writer: &mut ZipWriter<Cursor<Vec<u8>>>,
                          pictures: &mut Vec<(String, Vec<u8>)>|
     -> Result<(), SlicerError> {
        for (name, bytes) in pictures.drain(..) {
            writer.start_file(name, stored).map_err(zip_error)?;
            writer.write_all(&bytes).map_err(write_error)?;
        }
        Ok(())
    };
    if archive.index_for_name("[Content_Types].xml").is_none() {
        write_pictures(&mut writer, &mut pictures)?;
    }
    for index in 0..archive.len() {
        let name = archive
            .by_index_raw(index)
            .map_err(|e| unusable(format!("entry {index} cannot be read: {e}")))?
            .name()
            .map_err(|e| unusable(format!("entry {index} has no readable name: {e}")))?
            .into_owned();
        let edited = match name.as_str() {
            "Metadata/model_settings.config" => {
                let text = read_text(&mut archive, index, limits)?;
                model_settings(&text, plate, &gcode)
            }
            "[Content_Types].xml" => content_types(&read_text(&mut archive, index, limits)?),
            "_rels/.rels" => relationships(&read_text(&mut archive, index, limits)?, plate),
            _ => None,
        };
        match edited {
            Some(text) => {
                writer
                    .start_file(name.as_str(), deflated)
                    .map_err(zip_error)?;
                writer.write_all(text.as_bytes()).map_err(write_error)?;
            }
            None => {
                let entry = archive
                    .by_index_raw(index)
                    .map_err(|e| unusable(format!("`{name}` cannot be read: {e}")))?;
                writer.raw_copy_file(entry).map_err(zip_error)?;
            }
        }
        if name == "[Content_Types].xml" {
            write_pictures(&mut writer, &mut pictures)?;
        }
    }
    let bytes = writer.finish().map_err(zip_error)?.into_inner();
    Ok(Some(bytes))
}

/// `model_settings.config` with the preview lines in the plate's block, after its
/// `gcode_file` line; `None` when nothing is missing or there is no such line.
fn model_settings(text: &str, plate: u32, gcode: &str) -> Option<String> {
    let marker = format!("<metadata key=\"gcode_file\" value=\"{gcode}\"/>");
    let at = text.find(&marker)?;
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let indent = &text[line_start..at];
    if !indent.chars().all(|c| c == ' ' || c == '\t') {
        return None;
    }
    let line_end = at + marker.len();
    let mut added = String::new();
    for (key, file) in [
        ("thumbnail_file", format!("Metadata/plate_{plate}.png")),
        (
            "thumbnail_no_light_file",
            format!("Metadata/plate_no_light_{plate}.png"),
        ),
        ("top_file", format!("Metadata/top_{plate}.png")),
        ("pick_file", format!("Metadata/pick_{plate}.png")),
    ] {
        if !text.contains(&format!("value=\"{file}\"")) {
            added.push_str(&format!(
                "\n{indent}<metadata key=\"{key}\" value=\"{file}\"/>"
            ));
        }
    }
    if added.is_empty() {
        return None;
    }
    Some(format!("{}{added}{}", &text[..line_end], &text[line_end..]))
}

/// `[Content_Types].xml` with the `png` type; `None` when it has it or has no end tag.
fn content_types(text: &str) -> Option<String> {
    if text.contains("Extension=\"png\"") {
        return None;
    }
    let end = text.rfind("</Types>")?;
    Some(format!(
        "{} <Default Extension=\"png\" ContentType=\"image/png\"/>\n{}",
        &text[..end],
        &text[end..]
    ))
}

/// `_rels/.rels` with the thumbnail relationships Bambu Studio writes for plate `plate`;
/// `None` when they are there or it has no end tag.
fn relationships(text: &str, plate: u32) -> Option<String> {
    let large = format!("/Metadata/plate_{plate}.png");
    let small = format!("/Metadata/plate_{plate}_small.png");
    let mut added = String::new();
    let mut id = 0;
    let mut add = |target: &str, kind: &str| {
        loop {
            id += 1;
            if !text.contains(&format!("Id=\"rel-preview-{id}\"")) {
                break;
            }
        }
        added.push_str(&format!(
            " <Relationship Target=\"{target}\" Id=\"rel-preview-{id}\" Type=\"{kind}\"/>\n"
        ));
    };
    if !text.contains(&format!("Target=\"{large}\"")) {
        add(
            &large,
            "http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail",
        );
        add(
            &large,
            "http://schemas.bambulab.com/package/2021/cover-thumbnail-middle",
        );
    }
    if !text.contains(&format!("Target=\"{small}\"")) {
        add(
            &small,
            "http://schemas.bambulab.com/package/2021/cover-thumbnail-small",
        );
    }
    if added.is_empty() {
        return None;
    }
    let end = text.rfind("</Relationships>")?;
    Some(format!("{}{added}{}", &text[..end], &text[end..]))
}

fn read_text<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: usize,
    limits: &OutputLimits,
) -> Result<String, SlicerError> {
    let entry = archive
        .by_index(index)
        .map_err(|e| unusable(format!("entry {index} cannot be read: {e}")))?;
    let name = entry
        .name()
        .map_err(|e| unusable(format!("entry {index} has no readable name: {e}")))?
        .into_owned();
    let mut bytes = Vec::new();
    entry
        .take(limits.max_metadata_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| unusable(format!("`{name}` cannot be read: {e}")))?;
    if bytes.len() as u64 > limits.max_metadata_bytes {
        return Err(unusable(format!(
            "`{name}` is larger than the metadata limit"
        )));
    }
    String::from_utf8(bytes).map_err(|_| unusable(format!("`{name}` is not UTF-8 text")))
}

fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, SlicerError> {
    let file = std::fs::File::open(path).map_err(|e| SlicerError::io("open", path, e))?;
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| SlicerError::io("read", path, e))?;
    if bytes.len() as u64 > cap {
        return Err(unusable(format!(
            "`{}` is larger than {cap} bytes",
            path.display()
        )));
    }
    Ok(bytes)
}

fn unusable(message: impl Into<String>) -> SlicerError {
    SlicerError::UnusableOutput(message.into())
}

fn zip_error(error: zip::result::ZipError) -> SlicerError {
    unusable(format!("the plate previews cannot be added: {error}"))
}

fn write_error(error: std::io::Error) -> SlicerError {
    unusable(format!("the plate previews cannot be added: {error}"))
}

/// An 8-bit RGBA picture.
#[derive(Debug, Clone, PartialEq)]
struct Rgba {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Rgba {
    /// Reads a PNG render: any bit depth and colour type, at most
    /// [`MAX_RENDER_SIDE`] on a side.
    fn load(path: &Path) -> Result<Self, SlicerError> {
        let bad = |why: String| {
            SlicerError::UnusableOutput(format!(
                "the plate render `{}` cannot be used for the plate previews: {why}",
                path.display()
            ))
        };
        let bytes = read_capped(path, MAX_RENDER_BYTES)?;
        Self::decode(&bytes).map_err(bad)
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut decoder = png::Decoder::new(BufReader::new(Cursor::new(bytes)));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
        let (width, height) = (reader.info().width, reader.info().height);
        if width == 0 || height == 0 || width > MAX_RENDER_SIDE || height > MAX_RENDER_SIDE {
            return Err(format!("it is {width} x {height} pixels"));
        }
        let size = reader
            .output_buffer_size()
            .ok_or_else(|| "it is too large".to_owned())?;
        let mut buffer = vec![0; size];
        let frame = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
        let data = &buffer[..frame.buffer_size()];
        let channels = match frame.color_type {
            png::ColorType::Grayscale => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            png::ColorType::Indexed => return Err("its palette was not expanded".to_owned()),
        };
        if frame.bit_depth != png::BitDepth::Eight {
            return Err("its bit depth was not reduced to 8".to_owned());
        }
        let line = frame.line_size;
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height as usize {
            let row = &data[y * line..y * line + width as usize * channels];
            for pixel in row.chunks_exact(channels) {
                let rgba = match channels {
                    1 => [pixel[0], pixel[0], pixel[0], 255],
                    2 => [pixel[0], pixel[0], pixel[0], pixel[1]],
                    3 => [pixel[0], pixel[1], pixel[2], 255],
                    _ => [pixel[0], pixel[1], pixel[2], pixel[3]],
                };
                pixels.extend_from_slice(&rgba);
            }
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// The picture scaled to `side` x `side`: each new pixel is the average of the old
    /// pixels whose centres fall inside it (the nearest one when none does). A render
    /// that is not square is scaled to fit and centred on a transparent square.
    fn scaled(&self, side: u32) -> Self {
        let scale = f64::from(side) / f64::from(self.width.max(self.height));
        let (w, h) = (
            (f64::from(self.width) * scale).round().max(1.0),
            (f64::from(self.height) * scale).round().max(1.0),
        );
        let (x0, y0) = ((f64::from(side) - w) / 2.0, (f64::from(side) - h) / 2.0);
        let mut pixels = vec![0u8; side as usize * side as usize * 4];
        for y in 0..side {
            for x in 0..side {
                let (fx, fy) = (f64::from(x) - x0, f64::from(y) - y0);
                if fx < 0.0 || fy < 0.0 || fx >= w || fy >= h {
                    continue;
                }
                // The new pixel's footprint in old pixels.
                let sx = |v: f64| v / scale;
                let (left, right) = (sx(fx), sx(fx + 1.0));
                let (top, bottom) = (sx(fy), sx(fy + 1.0));
                let first = |v: f64| (v - 0.5).ceil().max(0.0) as u32;
                let last = |v: f64, max: u32| ((v - 0.5).ceil().max(0.0) as u32).min(max);
                let (ax, bx) = (first(left), last(right, self.width));
                let (ay, by) = (first(top), last(bottom, self.height));
                let mut sum = [0u64; 4];
                let mut count = 0u64;
                for oy in ay..by {
                    for ox in ax..bx {
                        let i = (oy as usize * self.width as usize + ox as usize) * 4;
                        for (total, value) in sum.iter_mut().zip(&self.pixels[i..i + 4]) {
                            *total += u64::from(*value);
                        }
                        count += 1;
                    }
                }
                let o = (y as usize * side as usize + x as usize) * 4;
                if count == 0 {
                    let ox = ((left + right) / 2.0)
                        .floor()
                        .clamp(0.0, f64::from(self.width - 1));
                    let oy = ((top + bottom) / 2.0)
                        .floor()
                        .clamp(0.0, f64::from(self.height - 1));
                    let i = (oy as usize * self.width as usize + ox as usize) * 4;
                    pixels[o..o + 4].copy_from_slice(&self.pixels[i..i + 4]);
                } else {
                    for (k, total) in sum.iter().enumerate() {
                        pixels[o + k] = ((total + count / 2) / count) as u8;
                    }
                }
            }
        }
        Self {
            width: side,
            height: side,
            pixels,
        }
    }

    fn encode(&self) -> Result<Vec<u8>, SlicerError> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder
                .write_header()
                .map_err(|e| unusable(format!("a plate preview cannot be encoded: {e}")))?;
            writer
                .write_image_data(&self.pixels)
                .map_err(|e| unusable(format!("a plate preview cannot be encoded: {e}")))?;
            writer
                .finish()
                .map_err(|e| unusable(format!("a plate preview cannot be encoded: {e}")))?;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// A render of `width` x `height` with colour type `color`, every pixel `value`.
    fn render(width: u32, height: u32, color: png::ColorType, value: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(color);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let data: Vec<u8> = (0..width * height).flat_map(|_| value.to_vec()).collect();
            writer.write_image_data(&data).unwrap();
        }
        out
    }

    fn header(png: &[u8]) -> (u32, u32, u8, u8) {
        let word = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
        (word(16), word(20), png[24], png[25])
    }

    const MODEL_SETTINGS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<config>\n  <plate>\n    <metadata key=\"plater_id\" value=\"1\"/>\n    <metadata key=\"gcode_file\" value=\"Metadata/plate_1.gcode\"/>\n  </plate>\n</config>\n";
    const CONTENT_TYPES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n <Default Extension=\"gcode\" ContentType=\"text/x.gcode\"/>\n</Types>\n";
    const RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n <Relationship Target=\"/3D/3dmodel.model\" Id=\"rel-1\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/>\n</Relationships>\n";

    /// A sliced file of the headless Linux shape (structure only).
    fn sliced(extra: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
            ("3D/3dmodel.model", b"<model/>"),
            (
                "Metadata/plate_1.gcode",
                b"; HEADER_BLOCK_START\nG1 X1 Y1\n",
            ),
            (
                "Metadata/plate_1.gcode.md5",
                b"0123456789ABCDEF0123456789ABCDEF",
            ),
            ("Metadata/model_settings.config", MODEL_SETTINGS.as_bytes()),
            (
                "Metadata/project_settings.config",
                b"{\"curr_bed_type\": \"Textured PEI Plate\"}",
            ),
            ("Metadata/slice_info.config", b"<config/>"),
            ("_rels/.rels", RELS.as_bytes()),
        ];
        entries.extend_from_slice(extra);
        for (name, content) in entries {
            writer
                .start_file(
                    name,
                    SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn renders() -> (Rgba, Rgba) {
        (
            Rgba::decode(&render(768, 768, png::ColorType::Rgb, &[10, 20, 30])).unwrap(),
            Rgba::decode(&render(768, 768, png::ColorType::Rgb, &[40, 50, 60])).unwrap(),
        )
    }

    fn names(bytes: &[u8]) -> Vec<String> {
        let archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        archive
            .file_names()
            .map(|name| name.unwrap().into_owned())
            .collect()
    }

    fn read(bytes: &[u8], name: &str) -> Vec<u8> {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut out = Vec::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    fn raw(bytes: &[u8], name: &str) -> (u32, u64, Vec<u8>) {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut entry = archive.by_name_raw(name.as_bytes()).unwrap();
        let (crc, size) = (entry.crc32(), entry.compressed_size());
        let mut out = Vec::new();
        entry.read_to_end(&mut out).unwrap();
        (crc, size, out)
    }

    #[test]
    fn previews_are_added_with_bambu_studios_names_sizes_and_index_lines() {
        let original = sliced(&[]);
        let (iso, top) = renders();
        let rebuilt = rebuild(&original, 1, &iso, &top, &OutputLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!(
            names(&rebuilt),
            [
                "[Content_Types].xml",
                "Metadata/plate_1.png",
                "Metadata/plate_1_small.png",
                "Metadata/plate_no_light_1.png",
                "Metadata/top_1.png",
                "Metadata/pick_1.png",
                "3D/3dmodel.model",
                "Metadata/plate_1.gcode",
                "Metadata/plate_1.gcode.md5",
                "Metadata/model_settings.config",
                "Metadata/project_settings.config",
                "Metadata/slice_info.config",
                "_rels/.rels",
            ]
        );
        for (name, side) in [
            ("Metadata/plate_1.png", 512),
            ("Metadata/plate_1_small.png", 128),
            ("Metadata/plate_no_light_1.png", 512),
            ("Metadata/top_1.png", 512),
            ("Metadata/pick_1.png", 512),
        ] {
            // 8-bit RGBA (colour type 6), as Bambu Studio writes them.
            assert_eq!(header(&read(&rebuilt, name)), (side, side, 8, 6), "{name}");
        }
        // The iso render becomes the plate preview, the top render the top view.
        let plate = Rgba::decode(&read(&rebuilt, "Metadata/plate_1.png")).unwrap();
        assert_eq!(&plate.pixels[..4], &[10, 20, 30, 255]);
        let topped = Rgba::decode(&read(&rebuilt, "Metadata/top_1.png")).unwrap();
        assert_eq!(&topped.pixels[..4], &[40, 50, 60, 255]);
        // The G-code, its MD5 and the settings the proof reads are copied as they are,
        // compressed bytes included.
        for name in [
            "Metadata/plate_1.gcode",
            "Metadata/plate_1.gcode.md5",
            "3D/3dmodel.model",
            "Metadata/project_settings.config",
            "Metadata/slice_info.config",
        ] {
            assert_eq!(raw(&rebuilt, name), raw(&original, name), "{name}");
        }
        let settings = String::from_utf8(read(&rebuilt, "Metadata/model_settings.config")).unwrap();
        assert_eq!(
            settings,
            MODEL_SETTINGS.replace(
                "    <metadata key=\"gcode_file\" value=\"Metadata/plate_1.gcode\"/>\n",
                "    <metadata key=\"gcode_file\" value=\"Metadata/plate_1.gcode\"/>\n    <metadata key=\"thumbnail_file\" value=\"Metadata/plate_1.png\"/>\n    <metadata key=\"thumbnail_no_light_file\" value=\"Metadata/plate_no_light_1.png\"/>\n    <metadata key=\"top_file\" value=\"Metadata/top_1.png\"/>\n    <metadata key=\"pick_file\" value=\"Metadata/pick_1.png\"/>\n"
            )
        );
        let types = String::from_utf8(read(&rebuilt, "[Content_Types].xml")).unwrap();
        assert!(types.contains("<Default Extension=\"png\" ContentType=\"image/png\"/>"));
        let rels = String::from_utf8(read(&rebuilt, "_rels/.rels")).unwrap();
        assert_eq!(rels.matches("Target=\"/Metadata/plate_1.png\"").count(), 2);
        assert_eq!(
            rels.matches("Target=\"/Metadata/plate_1_small.png\"")
                .count(),
            1
        );
        assert!(rels.contains("Id=\"rel-1\""));
    }

    #[test]
    fn index_entries_that_already_name_the_previews_are_left_alone() {
        // The headless Linux slice of 2026-11-21: the png type and the relationships were
        // there already, only the pictures and the model settings lines were missing.
        let types = CONTENT_TYPES.replace(
            "</Types>",
            " <Default Extension=\"png\" ContentType=\"image/png\"/>\n</Types>",
        );
        let rels = RELS.replace(
            "</Relationships>",
            " <Relationship Target=\"/Metadata/plate_1.png\" Id=\"rel-2\" Type=\"t\"/>\n <Relationship Target=\"/Metadata/plate_1_small.png\" Id=\"rel-5\" Type=\"s\"/>\n</Relationships>",
        );
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in [
            ("[Content_Types].xml", types.as_bytes()),
            ("Metadata/plate_1.gcode", b"G1 X1\n".as_slice()),
            ("Metadata/model_settings.config", MODEL_SETTINGS.as_bytes()),
            ("_rels/.rels", rels.as_bytes()),
        ] {
            writer
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content).unwrap();
        }
        let original = writer.finish().unwrap().into_inner();
        let (iso, top) = renders();
        let rebuilt = rebuild(&original, 1, &iso, &top, &OutputLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!(
            raw(&rebuilt, "[Content_Types].xml"),
            raw(&original, "[Content_Types].xml")
        );
        assert_eq!(raw(&rebuilt, "_rels/.rels"), raw(&original, "_rels/.rels"));
        assert_ne!(
            read(&rebuilt, "Metadata/model_settings.config"),
            read(&original, "Metadata/model_settings.config")
        );
    }

    #[test]
    fn a_file_with_the_slicers_own_preview_is_not_touched() {
        let original = sliced(&[("Metadata/plate_1.png", b"the slicer's own")]);
        let (iso, top) = renders();
        assert!(
            rebuild(&original, 1, &iso, &top, &OutputLimits::default())
                .unwrap()
                .is_none()
        );
        // On disk too: the file stays byte for byte.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plate_1.gcode.3mf");
        std::fs::write(&path, &original).unwrap();
        let iso_path = dir.path().join("iso.png");
        let top_path = dir.path().join("top.png");
        std::fs::write(&iso_path, render(64, 64, png::ColorType::Rgb, &[1, 2, 3])).unwrap();
        std::fs::write(&top_path, render(64, 64, png::ColorType::Rgb, &[1, 2, 3])).unwrap();
        let previews = PlatePreviews {
            iso: iso_path,
            top: top_path,
        };
        assert!(!add(&path, 1, &previews, &OutputLimits::default()).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn a_preview_of_another_kind_already_there_is_kept_and_not_doubled() {
        let original = sliced(&[("Metadata/top_1.png", b"kept")]);
        let (iso, top) = renders();
        let rebuilt = rebuild(&original, 1, &iso, &top, &OutputLimits::default())
            .unwrap()
            .unwrap();
        let all = names(&rebuilt);
        assert_eq!(all.iter().filter(|n| *n == "Metadata/top_1.png").count(), 1);
        assert_eq!(read(&rebuilt, "Metadata/top_1.png"), b"kept");
        assert!(all.contains(&"Metadata/plate_1.png".to_owned()));
    }

    #[test]
    fn add_rewrites_the_file_and_a_broken_render_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plate_2.gcode.3mf");
        let original = sliced(&[]);
        std::fs::write(&path, &original).unwrap();
        let iso_path = dir.path().join("iso.png");
        let top_path = dir.path().join("top.png");
        std::fs::write(&iso_path, b"not a png").unwrap();
        std::fs::write(
            &top_path,
            render(32, 16, png::ColorType::Rgba, &[9, 9, 9, 9]),
        )
        .unwrap();
        let previews = PlatePreviews {
            iso: iso_path.clone(),
            top: top_path,
        };
        let refused = add(&path, 1, &previews, &OutputLimits::default()).unwrap_err();
        assert_eq!(refused.code(), "unusable_output");
        assert!(refused.to_string().contains("plate render"), "{refused}");
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::write(&iso_path, render(16, 32, png::ColorType::Grayscale, &[200])).unwrap();
        assert!(add(&path, 1, &previews, &OutputLimits::default()).unwrap());
        let rebuilt = std::fs::read(&path).unwrap();
        // A render that is not square is fitted and centred on a transparent square.
        let plate = Rgba::decode(&read(&rebuilt, "Metadata/plate_1.png")).unwrap();
        assert_eq!((plate.width, plate.height), (512, 512));
        assert_eq!(&plate.pixels[..4], &[0, 0, 0, 0]);
        let centre = (256 * 512 + 256) * 4;
        assert_eq!(&plate.pixels[centre..centre + 4], &[200, 200, 200, 255]);
        assert!(!dir.path().join("plate_2.gcode.previews.part").exists());
        // Added once; a second run finds the slicer's (here: the first run's) preview.
        assert!(!add(&path, 1, &previews, &OutputLimits::default()).unwrap());
    }

    #[test]
    fn the_warning_starts_with_the_agreed_words() {
        assert!(warning_message().starts_with("plate previews made by Hotend"));
    }
}
