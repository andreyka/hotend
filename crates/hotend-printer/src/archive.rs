//! A hardened reader for the `.gcode.3mf` archive (a zip), which is hostile input.
//!
//! Guarantees of [`Archive::open`]:
//! - the entry count and the size of the central directory are checked against the limits
//!   by reading the end record by hand, before the zip library allocates anything for them;
//!   an archive with more than one end record, with disk spanning or with a zip64 record
//!   anywhere but directly before its locator is refused, so the record that was checked is
//!   the only one the library can use;
//! - every entry's name is a plain relative path of printable ASCII (no `..`, `\`, `:`,
//!   leading `/`, and no part that ends in a space or a dot); no two names are the same
//!   when upper and lower case are not told apart, and none is both a file and a folder
//!   on the way to another; no entry is encrypted or a symbolic link, and each is stored
//!   or deflated;
//! - the archive has one reading. Its entries tile the file from the first byte to the
//!   directory with no gap and no overlap; the header in front of each entry says what
//!   the directory says (name, method, flags, checksum, sizes), and so does the data
//!   descriptor behind it when it has one; the directory ends where the end record
//!   begins; there is no archive or entry comment. A reader that walks the file from
//!   its start, one that trusts the headers and one that trusts the directory all find
//!   the same entries with the same content;
//! - every entry's declared size, the total declared size and the declared compression
//!   ratio of entries over 1 MiB are within the limits;
//! - nothing is ever extracted to disk; [`Archive::read_capped`] and [`Archive::reader`]
//!   never yield more bytes than their cap, and an entry that holds more or fewer bytes
//!   than it declares is an error, not a short read.
//!
//! `hotend-slicer` has the same checks in `output::bambu` (crate-private). That crate
//! depends on `hotend-core`, which will depend on this crate, so the reader could not be
//! taken from there; `hotend-slicer` should use this module instead of its own copy.

use std::io::{Read, Seek, SeekFrom};

use zip::ZipArchive;

/// Size and count limits for a sliced archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    /// Largest archive file.
    pub max_file_bytes: u64,
    /// Most entries.
    pub max_entries: u64,
    /// Largest central directory.
    pub max_central_directory_bytes: u64,
    /// Largest declared uncompressed size of one entry.
    pub max_entry_bytes: u64,
    /// Largest declared uncompressed size of all entries together.
    pub max_total_bytes: u64,
    /// Highest declared compression ratio of an entry larger than 1 MiB.
    pub max_compression_ratio: u64,
    /// Most bytes read from a metadata entry.
    pub max_metadata_bytes: u64,
    /// Most bytes of G-code that are linted.
    pub max_gcode_bytes: u64,
}

impl Default for ArchiveLimits {
    /// The limits for a sliced file. The one real slice is a 0.4 MiB archive with 1 MiB
    /// of G-code in 21 entries; these leave room for a full plate and stop the check
    /// long before it could run for minutes. The check keeps the whole archive in
    /// memory while a job is open, so `max_file_bytes` is also a memory limit. The number
    /// of G-code lines has its own limit in the printer profile (`max_lines`).
    fn default() -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            max_file_bytes: 128 * MIB,
            max_entries: 256,
            max_central_directory_bytes: MIB,
            max_entry_bytes: 128 * MIB,
            max_total_bytes: 512 * MIB,
            max_compression_ratio: 200,
            max_metadata_bytes: 4 * MIB,
            max_gcode_bytes: 128 * MIB,
        }
    }
}

/// Why an archive was refused.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// The archive breaks a limit or a structural rule. The sliced file must not be used.
    #[error("the sliced file is not a safe archive: {0}")]
    Hostile(String),
    /// Reading failed.
    #[error("the sliced file cannot be read: {0}")]
    Io(#[from] std::io::Error),
}

fn hostile(message: impl Into<String>) -> ArchiveError {
    ArchiveError::Hostile(message.into())
}

/// Text from the archive for an error message: printable ASCII only, cut short.
fn shown(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(80)
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

/// A checked archive. Entries are read through capped readers only.
pub struct Archive<R> {
    archive: ZipArchive<R>,
}

/// Reads one entry and holds it to its cap and to the size the directory declares.
pub struct EntryReader<R> {
    inner: R,
    name: String,
    declared: u64,
    cap: u64,
    read: u64,
}

impl<R: Read> Read for EntryReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let invalid =
            |message: String| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
        if self.read > self.cap {
            // One byte past the cap was yielded, so the caller can tell; no more.
            return Ok(0);
        }
        let room = (self.cap.saturating_add(1) - self.read).min(buffer.len() as u64) as usize;
        let read = self.inner.read(&mut buffer[..room])?;
        self.read += read as u64;
        if self.read > self.declared {
            return Err(invalid(format!(
                "`{}` holds more bytes than the {} it declares",
                self.name, self.declared
            )));
        }
        if read == 0 && room > 0 && self.read != self.declared {
            return Err(invalid(format!(
                "`{}` holds {} bytes but declares {}",
                self.name, self.read, self.declared
            )));
        }
        Ok(read)
    }
}

impl<R: Read + Seek> Archive<R> {
    /// Checks the archive in `reader` against `limits` and opens it.
    pub fn open(mut reader: R, limits: &ArchiveLimits) -> Result<Self, ArchiveError> {
        check_directory(&mut reader, limits)?;
        reader.seek(SeekFrom::Start(0))?;
        let mut archive = ZipArchive::new(reader)
            .map_err(|e| hostile(format!("not a readable zip archive: {e}")))?;
        let entries = check_entries(&mut archive, limits)?;
        if archive.offset() != 0 {
            return Err(hostile("bytes stand before the first entry of the archive"));
        }
        if !archive.comment().is_empty() {
            return Err(hostile("the archive has a comment"));
        }
        let directory_start = archive.central_directory_start();
        // The library has told where it found the entries; now the file itself is held
        // to that, byte for byte, and opened again.
        let mut reader = archive.into_inner();
        check_layout(&mut reader, entries, directory_start)?;
        reader.seek(SeekFrom::Start(0))?;
        let archive = ZipArchive::new(reader)
            .map_err(|e| hostile(format!("not a readable zip archive: {e}")))?;
        Ok(Self { archive })
    }

    /// Whether the archive has an entry of this exact name.
    pub fn has(&self, name: &str) -> bool {
        self.archive.index_for_name(name).is_some()
    }

    /// The names of all entries.
    pub fn names(&self) -> Vec<String> {
        self.archive
            .file_names()
            .filter_map(|name| name.ok().map(|name| name.into_owned()))
            .collect()
    }

    /// Reads a whole entry of at most `cap` bytes. `Ok(None)` when it does not exist; an
    /// entry that turns out longer than `cap` is an error.
    pub fn read_capped(&mut self, name: &str, cap: u64) -> Result<Option<Vec<u8>>, ArchiveError> {
        if !self.has(name) {
            return Ok(None);
        }
        let mut reader = self
            .reader(name, cap)?
            .ok_or_else(|| hostile(format!("`{name}` cannot be read")))?;
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|e| hostile(format!("`{name}` cannot be read: {e}")))?;
        if bytes.len() as u64 > cap {
            return Err(hostile(format!("`{name}` is larger than {cap} bytes")));
        }
        Ok(Some(bytes))
    }

    /// A streaming reader over one entry that yields at most `cap + 1` bytes, so the
    /// caller can tell an entry that is too long from one that fits. An entry that ends
    /// before or after the size it declares is a read error. `Ok(None)` when the entry
    /// does not exist.
    pub fn reader(&mut self, name: &str, cap: u64) -> Result<Option<impl Read + '_>, ArchiveError> {
        if !self.has(name) {
            return Ok(None);
        }
        let entry = self
            .archive
            .by_name(name)
            .map_err(|e| hostile(format!("`{name}` cannot be read: {e}")))?;
        let declared = entry.size();
        Ok(Some(EntryReader {
            inner: entry,
            name: shown(name.as_bytes()),
            declared,
            cap,
            read: 0,
        }))
    }
}

/// Reads the end-of-central-directory record by hand and refuses an archive that declares
/// too many entries or too large a directory, before any library allocates for them.
///
/// The zip library is forgiving: when the last end record does not lead to a directory it
/// tries an earlier one, and it looks for a zip64 record between the place the locator
/// names and the locator. A check of "the" end record is only worth something if there is
/// exactly one record the library could use, so the archive must have the plain layout
/// every writer produces.
fn check_directory<R: Read + Seek>(
    reader: &mut R,
    limits: &ArchiveLimits,
) -> Result<(), ArchiveError> {
    const EOCD: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    const LOCATOR: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
    const EOCD64: [u8; 4] = [0x50, 0x4b, 0x06, 0x06];
    const EOCD_LEN: usize = 22;
    const LOCATOR_LEN: usize = 20;
    const EOCD64_LEN: usize = 56;
    // The library searches the last 64 KiB and a little more; this covers it with room.
    const SEARCHED: u64 = EOCD_LEN as u64 + 65_536 + 64;
    let len = reader.seek(SeekFrom::End(0))?;
    if len < EOCD_LEN as u64 {
        return Err(hostile("too short to be a zip archive"));
    }
    if len > limits.max_file_bytes {
        return Err(hostile(format!(
            "the file is {len} bytes; the limit is {}",
            limits.max_file_bytes
        )));
    }
    let tail_len = len.min(SEARCHED);
    reader.seek(SeekFrom::Start(len - tail_len))?;
    let mut tail = vec![0u8; tail_len as usize];
    reader.read_exact(&mut tail)?;

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
    let mut directory_offset = u32_at(position + 16);
    // Where the directory must end: at the end record, or at the zip64 record.
    let mut directory_end = len - tail_len + position as u64;
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
        reader.seek(SeekFrom::Start(offset))?;
        let mut record = [0u8; EOCD64_LEN];
        reader.read_exact(&mut record)?;
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
        directory_offset = u64_at(48);
        directory_end = offset;
    }
    if directory_offset.checked_add(directory_bytes) != Some(directory_end) {
        return Err(hostile(
            "the archive's directory does not end where its end record begins",
        ));
    }
    if entries > limits.max_entries {
        return Err(hostile(format!(
            "the archive declares {entries} entries; the limit is {}",
            limits.max_entries
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

/// What the directory says about one entry.
struct Listed {
    name: Vec<u8>,
    header_start: u64,
    flags: u16,
    method: u16,
    crc32: u32,
    compressed: u64,
    size: u64,
}

/// Checks every entry's name and declared sizes, and returns what the directory lists.
fn check_entries<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    limits: &ArchiveLimits,
) -> Result<Vec<Listed>, ArchiveError> {
    if archive.len() as u64 > limits.max_entries {
        return Err(hostile(format!(
            "the archive has {} entries; the limit is {}",
            archive.len(),
            limits.max_entries
        )));
    }
    if archive.has_duplicate_names() {
        return Err(hostile("the archive has two entries with the same name"));
    }
    let mut total: u64 = 0;
    let mut listed = Vec::with_capacity(archive.len());
    // Names as a reader that does not tell upper from lower case sees them, without the
    // slash that marks a folder.
    let mut folded: Vec<String> = Vec::with_capacity(archive.len());
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
                    shown(entry.name_raw())
                ))
            })?;
        if entry.is_symlink() || entry.encrypted() {
            return Err(hostile(format!(
                "`{name}` is a symbolic link or is encrypted"
            )));
        }
        if !entry.comment_raw().is_empty() {
            return Err(hostile(format!("`{name}` has a comment")));
        }
        let method = match entry.compression() {
            zip::CompressionMethod::Stored => 0,
            zip::CompressionMethod::Deflated => 8,
            _ => {
                return Err(hostile(format!("`{name}` is neither stored nor deflated")));
            }
        };
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
        folded.push(name.strip_suffix('/').unwrap_or(name).to_ascii_lowercase());
        listed.push(Listed {
            name: entry.name_raw().to_vec(),
            header_start: entry.header_start(),
            flags: entry.flags().as_u16(),
            method,
            crc32: entry.crc32(),
            compressed: entry.compressed_size(),
            size,
        });
    }
    // Another reader may fold case, or unpack to a drive that does. Then two names that
    // differ only in case are one file, and a file named like a folder on the way to
    // another entry cannot exist beside it.
    let mut sorted = folded.clone();
    sorted.sort();
    if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(hostile(
            "two entries of the archive have names that differ only in upper and lower case, or name a file and a folder alike",
        ));
    }
    let file_on_the_way = archive.file_names().any(|name| {
        let name = name
            .map(|name| name.to_ascii_lowercase())
            .unwrap_or_default();
        !name.ends_with('/')
            && sorted.iter().any(|other| {
                other.len() > name.len() + 1
                    && other.starts_with(&name)
                    && other.as_bytes()[name.len()] == b'/'
            })
    });
    if file_on_the_way {
        return Err(hostile(
            "an entry of the archive is a file and also a folder on the way to another entry",
        ));
    }
    let overlapping = archive
        .has_overlapping_files()
        .map_err(|e| hostile(format!("the archive's layout cannot be checked: {e}")))?;
    if overlapping {
        return Err(hostile("two entries of the archive share the same bytes"));
    }
    Ok(listed)
}

/// Holds the bytes of the archive to what its directory lists. The entries must tile the
/// file from its first byte to the directory, and the header in front of each entry, and
/// the data descriptor behind it when it has one, must say what the directory says.
///
/// The directory is what this reader uses. Another reader may walk the file from its
/// start, or trust the header in front of an entry; with this check all of them see the
/// same entries, so nothing can be shown to the check and something else to a printer.
fn check_layout<R: Read + Seek>(
    reader: &mut R,
    mut listed: Vec<Listed>,
    directory_start: u64,
) -> Result<(), ArchiveError> {
    const LOCAL: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];
    const DESCRIPTOR: [u8; 4] = [0x50, 0x4b, 0x07, 0x08];
    const DESCRIPTOR_FLAG: u16 = 1 << 3;
    listed.sort_by_key(|entry| entry.header_start);
    let u16_of = |bytes: &[u8]| u64::from(u16::from_le_bytes([bytes[0], bytes[1]]));
    let u32_of =
        |bytes: &[u8]| u64::from(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
    let mut expected: u64 = 0;
    for entry in &listed {
        let name = shown(&entry.name);
        if entry.header_start != expected {
            return Err(hostile(format!(
                "the bytes from {expected} to {} of the archive belong to no entry of its directory, or to two",
                entry.header_start
            )));
        }
        reader.seek(SeekFrom::Start(entry.header_start))?;
        let mut header = [0u8; 30];
        reader
            .read_exact(&mut header)
            .map_err(|_| hostile(format!("the header of `{name}` is cut off")))?;
        let name_len = u16_of(&header[26..28]);
        let extra_len = u16_of(&header[28..30]);
        let mut local_name = vec![0u8; name_len as usize];
        reader
            .read_exact(&mut local_name)
            .map_err(|_| hostile(format!("the header of `{name}` is cut off")))?;
        let described = entry.flags & DESCRIPTOR_FLAG != 0;
        let (crc, compressed, size) = (
            u32_of(&header[14..18]),
            u32_of(&header[18..22]),
            u32_of(&header[22..26]),
        );
        // With a data descriptor the header may leave checksum and sizes at zero.
        let same = |local: u64, directory: u64| local == directory || (described && local == 0);
        let agrees = header[..4] == LOCAL
            && local_name == entry.name
            && u16_of(&header[6..8]) == u64::from(entry.flags)
            && u16_of(&header[8..10]) == u64::from(entry.method)
            && same(crc, u64::from(entry.crc32))
            && same(compressed, entry.compressed)
            && same(size, entry.size);
        if !agrees {
            return Err(hostile(format!(
                "the header in front of `{name}` does not say what the archive's directory says about it"
            )));
        }
        if extra_len != 0 {
            // No entry of a real slice has one, and it is room for bytes no reader shows.
            return Err(hostile(format!(
                "the header in front of `{name}` carries an extra field"
            )));
        }
        expected = entry
            .header_start
            .checked_add(30 + name_len + entry.compressed)
            .ok_or_else(|| hostile("an entry lies beyond the end of the file"))?;
        if described {
            reader.seek(SeekFrom::Start(expected))?;
            let mut descriptor = [0u8; 16];
            reader
                .read_exact(&mut descriptor)
                .map_err(|_| hostile(format!("the data descriptor of `{name}` is cut off")))?;
            let agrees = descriptor[..4] == DESCRIPTOR
                && u32_of(&descriptor[4..8]) == u64::from(entry.crc32)
                && u32_of(&descriptor[8..12]) == entry.compressed
                && u32_of(&descriptor[12..16]) == entry.size;
            if !agrees {
                return Err(hostile(format!(
                    "the data descriptor behind `{name}` does not say what the archive's directory says about it"
                )));
            }
            expected += 16;
        }
    }
    if expected != directory_start {
        return Err(hostile(format!(
            "the bytes from {expected} to {directory_start} of the archive belong to no entry of its directory"
        )));
    }
    Ok(())
}

/// A name is safe when it is a plain relative path of printable ASCII with `/`
/// separators: no empty, `.` or `..` component, none that ends in a space or a dot (some
/// systems drop those, so two names would become one), no backslash, colon or leading
/// slash. A single trailing slash (a folder entry) is allowed.
fn safe_entry_name(name: &str) -> bool {
    let path = name.strip_suffix('/').unwrap_or(name);
    !path.is_empty()
        && path.len() <= 512
        && path
            .chars()
            .all(|c| (c.is_ascii_graphic() || c == ' ') && !matches!(c, '\\' | ':'))
        && path.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.ends_with([' ', '.'])
        })
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used)]
    use std::io::{Cursor, Write};

    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::*;

    /// A zip archive with these entries, deflated.
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

    fn open(
        bytes: Vec<u8>,
        limits: &ArchiveLimits,
    ) -> Result<Archive<Cursor<Vec<u8>>>, ArchiveError> {
        Archive::open(Cursor::new(bytes), limits)
    }

    fn refused(bytes: Vec<u8>, limits: &ArchiveLimits) -> String {
        match open(bytes, limits) {
            Err(ArchiveError::Hostile(message)) => message,
            Err(other) => panic!("expected a refusal, got {other}"),
            Ok(_) => panic!("the archive was accepted"),
        }
    }

    #[test]
    fn a_plain_archive_is_read_through_capped_readers() {
        let bytes = archive(&[
            ("Metadata/plate_1.gcode", b"G1 X1\nG1 X2\n"),
            ("a.txt", b"hello"),
        ]);
        let mut opened = open(bytes, &ArchiveLimits::default()).unwrap();
        assert!(opened.has("a.txt") && !opened.has("A.TXT"));
        assert_eq!(opened.names().len(), 2);
        assert_eq!(opened.read_capped("a.txt", 5).unwrap().unwrap(), b"hello");
        assert!(opened.read_capped("a.txt", 4).is_err());
        assert!(opened.read_capped("missing", 10).unwrap().is_none());
        let mut text = String::new();
        opened
            .reader("Metadata/plate_1.gcode", 5)
            .unwrap()
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "G1 X1\n", "cap + 1 bytes at most");
    }

    #[test]
    fn entry_count_size_and_ratio_limits_hold() {
        let many: Vec<(String, &[u8])> = (0..20).map(|i| (format!("f{i}"), &b"x"[..])).collect();
        let many: Vec<(&str, &[u8])> = many.iter().map(|(n, c)| (n.as_str(), *c)).collect();
        let limits = ArchiveLimits {
            max_entries: 19,
            ..ArchiveLimits::default()
        };
        assert!(refused(archive(&many), &limits).contains("20 entries"));

        let big = vec![0u8; 3 * 1024 * 1024];
        let bomb = archive(&[("zeros", &big)]);
        assert!(refused(bomb.clone(), &ArchiveLimits::default()).contains("compression ratio"));
        let limits = ArchiveLimits {
            max_entry_bytes: 1024,
            max_compression_ratio: 1 << 40,
            ..ArchiveLimits::default()
        };
        assert!(refused(bomb.clone(), &limits).contains("limit per entry"));
        let limits = ArchiveLimits {
            max_total_bytes: 1024,
            max_compression_ratio: 1 << 40,
            ..ArchiveLimits::default()
        };
        assert!(refused(bomb.clone(), &limits).contains("in total"));
        let limits = ArchiveLimits {
            max_file_bytes: 100,
            ..ArchiveLimits::default()
        };
        assert!(refused(bomb, &limits).contains("the limit is 100"));
        let limits = ArchiveLimits {
            max_central_directory_bytes: 10,
            ..ArchiveLimits::default()
        };
        assert!(refused(archive(&many), &limits).contains("directory"));
    }

    #[test]
    fn unsafe_names_are_refused() {
        for name in ["../evil", "/abs", "a\\b", "c:x", "a//b", "a/./b", "a\u{7}b"] {
            let message = refused(archive(&[(name, b"x")]), &ArchiveLimits::default());
            assert!(message.contains("unsafe name"), "{name}: {message}");
        }
        assert!(safe_entry_name("Metadata/plate_1.gcode"));
        assert!(safe_entry_name("Metadata/"));
        assert!(!safe_entry_name(""));
        assert!(!safe_entry_name(&"a".repeat(513)));
    }

    #[test]
    fn duplicate_names_and_extra_end_records_are_refused() {
        // Two entries of the same name, by renaming one in both headers.
        let mut bytes = archive(&[("aa", b"1"), ("ab", b"2")]);
        let mut patched = 0;
        for i in 0..bytes.len() - 1 {
            if &bytes[i..i + 2] == b"ab" {
                bytes[i + 1] = b'a';
                patched += 1;
            }
        }
        assert_eq!(patched, 2);
        assert!(refused(bytes, &ArchiveLimits::default()).contains("same name"));

        // A second end record hidden in the archive comment.
        let plain = archive(&[("a", b"1")]);
        let mut twice = plain.clone();
        let end = twice.len() - 22;
        let record = twice[end..].to_vec();
        twice[end + 20] = 22; // comment length
        twice.extend_from_slice(&record);
        assert!(refused(twice, &ArchiveLimits::default()).contains("more than one"));

        // Trailing bytes after the end record.
        let mut trailing = plain.clone();
        trailing.extend_from_slice(b"junk");
        assert!(refused(trailing, &ArchiveLimits::default()).contains("does not end the file"));

        // An end record that declares 65535 entries without a zip64 locator.
        let mut lying = plain;
        let end = lying.len() - 22;
        lying[end + 8..end + 12].copy_from_slice(&[0xFF; 4]);
        assert!(refused(lying, &ArchiveLimits::default()).contains("zip64"));

        for garbage in [&b""[..], b"PK", b"not a zip archive at all, just text....."] {
            assert!(open(garbage.to_vec(), &ArchiveLimits::default()).is_err());
        }
    }
}
