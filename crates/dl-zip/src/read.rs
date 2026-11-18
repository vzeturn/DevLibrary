//! Reading archives.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::{
    DosDateTime, Error, SIG_CENTRAL, SIG_EOCD, SIG_EOCD64, SIG_LOCAL, SIG_LOCATOR64, cp437, path,
};

/// The end of central directory record (22 bytes) plus the longest comment.
const EOCD_SEARCH: u64 = 22 + 65_535;
const FLAG_ENCRYPTED: u16 = 1;
const FLAG_UTF8: u16 = 1 << 11;

// Callers check the bounds first.
fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from(u32_at(b, i)) | (u64::from(u32_at(b, i + 4)) << 32)
}

fn malformed(what: &'static str, offset: u64) -> Error {
    Error::Malformed { what, offset }
}

/// One entry of the central directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    name: String,
    raw_name: Vec<u8>,
    version_made_by: u16,
    flags: u16,
    method: u16,
    modified: DosDateTime,
    modified_unix: Option<i64>,
    crc32: u32,
    compressed_size: u64,
    size: u64,
    external_attributes: u32,
    header_offset: u64,
}

impl Entry {
    /// The name, decoded from UTF-8 when flag bit 11 is set, else from an Info-ZIP
    /// Unicode Path extra field when one matches, else from CP437. Directories end with
    /// `/`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The name bytes as stored in the central directory.
    pub fn raw_name(&self) -> &[u8] {
        &self.raw_name
    }

    /// Whether this is a directory: the name ends with `/` (or `\`), or the MS-DOS
    /// directory attribute or a Unix directory mode is set.
    pub fn is_dir(&self) -> bool {
        self.name.ends_with(['/', '\\'])
            || self.external_attributes & 0x10 != 0
            || self.unix_mode().is_some_and(|m| m & 0o170_000 == 0o040_000)
    }

    /// Uncompressed size from the central directory.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Compressed size from the central directory.
    pub fn compressed_size(&self) -> u64 {
        self.compressed_size
    }

    /// CRC-32 of the uncompressed data, from the central directory.
    pub fn crc32(&self) -> u32 {
        self.crc32
    }

    /// Compression method code: 0 = stored, 8 = deflate (the only ones that can be read).
    pub fn method(&self) -> u16 {
        self.method
    }

    /// General purpose bit flags (bit 0: encrypted, bit 3: data descriptor, bit 11: UTF-8).
    pub fn flags(&self) -> u16 {
        self.flags
    }

    /// Whether the entry is encrypted (flag bit 0); such entries cannot be read.
    pub fn is_encrypted(&self) -> bool {
        self.flags & FLAG_ENCRYPTED != 0
    }

    /// Last modification time from the header (usually local time of the writer).
    pub fn modified(&self) -> DosDateTime {
        self.modified
    }

    /// Last modification time in seconds since the Unix epoch (UTC) from an extended
    /// timestamp extra field (0x5455), when present.
    pub fn modified_unix(&self) -> Option<i64> {
        self.modified_unix
    }

    /// External file attributes: MS-DOS attributes in the low byte, the Unix mode in the
    /// high 16 bits when the entry was made on Unix.
    pub fn external_attributes(&self) -> u32 {
        self.external_attributes
    }

    /// The Unix mode (`st_mode`, e.g. `0o100755`) when the entry was made on Unix or
    /// macOS and has one.
    pub fn unix_mode(&self) -> Option<u32> {
        let host = self.version_made_by >> 8;
        let mode = self.external_attributes >> 16;
        ((host == 3 || host == 19) && mode != 0).then_some(mode)
    }

    /// The name as a relative path that stays inside a target directory, or `None` when
    /// it is unsafe (absolute, drive letter, `..`, NUL or control characters; on Windows
    /// also `:`, trailing dots or spaces and device names like `CON`) or empty. Both `/`
    /// and `\` separate components.
    pub fn enclosed_name(&self) -> Option<PathBuf> {
        let parts = path::components(&self.name, cfg!(windows))?;
        (!parts.is_empty()).then(|| parts.iter().collect())
    }
}

/// Limits for [`ZipArchive::extract_to`], the guard against zip bombs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractLimits {
    /// Most entries the archive may have.
    pub max_entries: usize,
    /// Largest uncompressed size of one entry (entries are decompressed in memory).
    pub max_entry_size: usize,
    /// Largest sum of the uncompressed sizes.
    pub max_total_size: u64,
}

impl Default for ExtractLimits {
    /// 100 000 entries, 1 GiB per entry, 4 GiB in total.
    fn default() -> Self {
        ExtractLimits {
            max_entries: 100_000,
            max_entry_size: 1 << 30,
            max_total_size: 4 << 30,
        }
    }
}

/// A ZIP archive opened for reading.
#[derive(Debug)]
pub struct ZipArchive<R> {
    reader: R,
    len: u64,
    entries: Vec<Entry>,
    comment: Vec<u8>,
}

struct Zip64End {
    pos: u64,
    count: u64,
    cd_size: u64,
    cd_offset: u64,
}

impl<R: Read + Seek> ZipArchive<R> {
    /// Reads the central directory.
    ///
    /// Data before the archive (a self-extractor stub) is allowed.
    ///
    /// # Errors
    /// [`Error::NotZip`] without an end of central directory record, [`Error::MultiDisk`],
    /// [`Error::Malformed`] for inconsistent or truncated structures, [`Error::Io`].
    pub fn new(mut reader: R) -> Result<Self, Error> {
        let len = reader.seek(SeekFrom::End(0))?;
        let tail_len = len.min(EOCD_SEARCH);
        let tail_start = len - tail_len;
        let tail = read_at(&mut reader, len, tail_start, tail_len, "truncated archive")?;
        let rel = find_eocd(&tail).ok_or(Error::NotZip)?;
        let eocd_pos = tail_start + rel as u64;
        // find_eocd checked that the record and its comment are inside `tail`.
        let e = &tail[rel..];
        let disk = u16_at(e, 4);
        let cd_disk = u16_at(e, 6);
        let n_disk = u16_at(e, 8);
        let n_total = u16_at(e, 10);
        let cd_size32 = u32_at(e, 12);
        let cd_offset32 = u32_at(e, 16);
        let comment = e[22..22 + usize::from(u16_at(e, 20))].to_vec();
        let markers = disk == u16::MAX
            || cd_disk == u16::MAX
            || n_disk == u16::MAX
            || n_total == u16::MAX
            || cd_size32 == u32::MAX
            || cd_offset32 == u32::MAX;

        let (count, cd_size, cd_offset, dir_end, wrapped) =
            match find_zip64(&mut reader, len, eocd_pos)? {
                Some(z) => (z.count, z.cd_size, z.cd_offset, z.pos, false),
                None if markers => {
                    return Err(malformed(
                        "ZIP64 end of central directory record not found",
                        eocd_pos,
                    ));
                }
                None => {
                    if disk != 0 || cd_disk != 0 || n_disk != n_total {
                        return Err(Error::MultiDisk);
                    }
                    let n = u64::from(n_total);
                    (
                        n,
                        u64::from(cd_size32),
                        u64::from(cd_offset32),
                        eocd_pos,
                        true,
                    )
                }
            };

        let cd_end_offset = dir_end.checked_sub(cd_size).ok_or(malformed(
            "central directory larger than the space before its end record",
            eocd_pos,
        ))?;
        let mut base = cd_end_offset.checked_sub(cd_offset).ok_or(malformed(
            "central directory offset past its end record",
            eocd_pos,
        ))?;
        // Data in front of the archive moves everything by `base`; the stated offsets
        // are used when no central directory is found at the moved position.
        if base > 0 && count > 0 && !sig_at(&mut reader, len, cd_offset + base, SIG_CENTRAL)? {
            base = 0;
        }
        let cd_start = cd_offset + base;
        let cd = read_at(
            &mut reader,
            len,
            cd_start,
            cd_size,
            "central directory past the end of the archive",
        )?;
        if count > cd_size / 46 {
            return Err(malformed(
                "more entries than the central directory can hold",
                eocd_pos,
            ));
        }
        // count <= cd.len() / 46, so it fits in usize.
        let mut entries = Vec::with_capacity(count as usize);
        let mut p = 0;
        while cd.len() - p >= 4 && u32_at(&cd, p) == SIG_CENTRAL {
            let (entry, next) = parse_central(&cd, p, cd_start, base)?;
            entries.push(entry);
            p = next;
        }
        let found = entries.len() as u64;
        // Some old writers store the entry count modulo 65 536 without ZIP64 records.
        let ok = if wrapped {
            found >= count && (found - count).is_multiple_of(65_536)
        } else {
            found == count
        };
        if !ok {
            return Err(malformed(
                "number of central directory entries differs from the end record",
                cd_start,
            ));
        }
        Ok(ZipArchive {
            reader,
            len,
            entries,
            comment,
        })
    }

    /// The entries in central directory order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The archive comment (raw bytes).
    pub fn comment(&self) -> &[u8] {
        &self.comment
    }

    /// Index of the first entry with this exact name.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.name == name)
    }

    /// Reads and decompresses entry `index`, checking its size and CRC-32. Entries whose
    /// declared size is above `limit` are refused before anything is decompressed.
    ///
    /// # Errors
    /// [`Error::TooLarge`], [`Error::Encrypted`], [`Error::UnsupportedMethod`],
    /// [`Error::ChecksumMismatch`], [`Error::SizeMismatch`], [`Error::Deflate`],
    /// [`Error::Malformed`], [`Error::IndexOutOfRange`], [`Error::Io`].
    pub fn read(&mut self, index: usize, limit: usize) -> Result<Vec<u8>, Error> {
        let len = self.entries.len();
        let e = self
            .entries
            .get(index)
            .ok_or(Error::IndexOutOfRange { index, len })?;
        if e.is_encrypted() {
            return Err(Error::Encrypted { index });
        }
        if e.method != 0 && e.method != 8 {
            return Err(Error::UnsupportedMethod {
                index,
                method: e.method,
            });
        }
        if e.size > limit as u64 {
            return Err(Error::TooLarge {
                index,
                size: e.size,
                limit: limit as u64,
            });
        }
        let (size, csize, method, crc, at) = (
            e.size,
            e.compressed_size,
            e.method,
            e.crc32,
            e.header_offset,
        );
        let lh = read_at(
            &mut self.reader,
            self.len,
            at,
            30,
            "local file header past the end of the archive",
        )?;
        if u32_at(&lh, 0) != SIG_LOCAL {
            return Err(malformed("missing local file header", at));
        }
        // at + 30 <= self.len, and the two lengths add at most 131 070.
        let data_start = (at + 30)
            .checked_add(u64::from(u16_at(&lh, 26)) + u64::from(u16_at(&lh, 28)))
            .ok_or(malformed(
                "local file header past the end of the archive",
                at,
            ))?;
        let max_compressed = if method == 0 {
            size
        } else {
            // DEFLATE never needs much more than the data itself (5 bytes per stored
            // block); this bounds the buffer for the compressed bytes.
            size.saturating_add(size / 8).saturating_add(65_536)
        };
        if csize > max_compressed {
            return Err(malformed(
                if method == 0 {
                    "stored entry with different compressed and uncompressed sizes"
                } else {
                    "compressed size too large for the declared size"
                },
                at,
            ));
        }
        let comp = read_at(
            &mut self.reader,
            self.len,
            data_start,
            csize,
            "entry data past the end of the archive",
        )?;
        // size <= limit, a usize.
        let size_usize = size as usize;
        let data = if method == 0 {
            comp
        } else if csize == 0 && size == 0 {
            Vec::new()
        } else {
            dl_flate::deflate_raw::decompress(&comp, size_usize).map_err(|source| {
                match source.kind() {
                    dl_flate::ErrorKind::TooLarge { .. } => Error::SizeMismatch {
                        index,
                        declared: size,
                    },
                    _ => Error::Deflate { index, source },
                }
            })?
        };
        if data.len() != size_usize {
            return Err(Error::SizeMismatch {
                index,
                declared: size,
            });
        }
        let actual = dl_hash::crc32(&data);
        if actual != crc {
            return Err(Error::ChecksumMismatch {
                index,
                expected: crc,
                actual,
            });
        }
        Ok(data)
    }

    /// Like [`read`](Self::read), for the first entry with this exact name.
    ///
    /// # Errors
    /// [`Error::NotFound`] and the errors of [`read`](Self::read).
    pub fn read_by_name(&mut self, name: &str, limit: usize) -> Result<Vec<u8>, Error> {
        let index = self
            .index_of(name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?;
        self.read(index, limit)
    }

    /// Extracts every entry below `dir`, creating directories as needed and overwriting
    /// existing files.
    ///
    /// All names and the limits are checked before anything is written: one unsafe name
    /// (see [`Entry::enclosed_name`]) refuses the whole archive. Each entry is
    /// decompressed in memory and checked (size, CRC-32) before its file is written; if
    /// one fails, the files already written stay. Symbolic links are written as regular
    /// files holding the link target.
    ///
    /// # Errors
    /// [`Error::UnsafeName`], [`Error::TooManyEntries`], [`Error::TooLarge`],
    /// [`Error::TotalTooLarge`], the errors of [`read`](Self::read), [`Error::Io`].
    pub fn extract_to(
        &mut self,
        dir: impl AsRef<Path>,
        limits: &ExtractLimits,
    ) -> Result<(), Error> {
        let dir = dir.as_ref();
        if self.entries.len() > limits.max_entries {
            return Err(Error::TooManyEntries {
                count: self.entries.len(),
                limit: limits.max_entries,
            });
        }
        let mut plan: Vec<(PathBuf, bool)> = Vec::with_capacity(self.entries.len());
        let mut total = 0u64;
        for (index, e) in self.entries.iter().enumerate() {
            let unsafe_name = || Error::UnsafeName {
                index,
                name: e.name.clone(),
            };
            let parts = path::components(&e.name, cfg!(windows)).ok_or_else(unsafe_name)?;
            let is_dir = e.is_dir();
            if parts.is_empty() && !is_dir {
                return Err(unsafe_name());
            }
            if !is_dir {
                if e.size > limits.max_entry_size as u64 {
                    return Err(Error::TooLarge {
                        index,
                        size: e.size,
                        limit: limits.max_entry_size as u64,
                    });
                }
                total = total.saturating_add(e.size);
                if total > limits.max_total_size {
                    return Err(Error::TotalTooLarge {
                        limit: limits.max_total_size,
                    });
                }
            }
            plan.push((parts.iter().collect(), is_dir));
        }
        std::fs::create_dir_all(dir)?;
        for (index, (rel, is_dir)) in plan.into_iter().enumerate() {
            let target = dir.join(rel);
            if is_dir {
                std::fs::create_dir_all(&target)?;
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let data = self.read(index, limits.max_entry_size)?;
            std::fs::write(&target, data)?;
        }
        Ok(())
    }

    /// Returns the underlying reader.
    pub fn into_inner(self) -> R {
        self.reader
    }
}

/// Reads `len` bytes at `pos`, refusing ranges past `total` before allocating.
fn read_at<R: Read + Seek>(
    r: &mut R,
    total: u64,
    pos: u64,
    len: u64,
    what: &'static str,
) -> Result<Vec<u8>, Error> {
    if pos.checked_add(len).is_none_or(|end| end > total) {
        return Err(malformed(what, pos));
    }
    let n = usize::try_from(len)
        .map_err(|_| malformed("structure too large for this platform", pos))?;
    r.seek(SeekFrom::Start(pos))?;
    let mut buf = vec![0; n];
    r.read_exact(&mut buf).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            malformed(what, pos)
        } else {
            Error::Io(e)
        }
    })?;
    Ok(buf)
}

fn sig_at<R: Read + Seek>(r: &mut R, total: u64, pos: u64, sig: u32) -> Result<bool, Error> {
    match read_at(r, total, pos, 4, "") {
        Ok(b) => Ok(u32_at(&b, 0) == sig),
        Err(Error::Malformed { .. }) => Ok(false),
        Err(e) => Err(e),
    }
}

/// The last end of central directory signature whose comment fits in `tail`.
fn find_eocd(tail: &[u8]) -> Option<usize> {
    let last = tail.len().checked_sub(22)?;
    (0..=last).rev().find(|&i| {
        u32_at(tail, i) == SIG_EOCD && i + 22 + usize::from(u16_at(tail, i + 20)) <= tail.len()
    })
}

fn find_zip64<R: Read + Seek>(
    r: &mut R,
    len: u64,
    eocd_pos: u64,
) -> Result<Option<Zip64End>, Error> {
    let Some(loc_pos) = eocd_pos.checked_sub(20) else {
        return Ok(None);
    };
    let loc = read_at(r, len, loc_pos, 20, "truncated ZIP64 locator")?;
    if u32_at(&loc, 0) != SIG_LOCATOR64 {
        return Ok(None);
    }
    if u32_at(&loc, 4) != 0 || u32_at(&loc, 16) > 1 {
        return Err(Error::MultiDisk);
    }
    // The stated offset, or right before the locator (data in front of the archive).
    for pos in [Some(u64_at(&loc, 8)), loc_pos.checked_sub(56)]
        .into_iter()
        .flatten()
    {
        if !sig_at(r, len, pos, SIG_EOCD64)? {
            continue;
        }
        let rec = read_at(
            r,
            len,
            pos,
            56,
            "truncated ZIP64 end of central directory record",
        )?;
        if u32_at(&rec, 16) != 0 || u32_at(&rec, 20) != 0 || u64_at(&rec, 24) != u64_at(&rec, 32) {
            return Err(Error::MultiDisk);
        }
        return Ok(Some(Zip64End {
            pos,
            count: u64_at(&rec, 32),
            cd_size: u64_at(&rec, 40),
            cd_offset: u64_at(&rec, 48),
        }));
    }
    Ok(None)
}

/// Parses the central directory header at `cd[p..]`; returns it and the next position.
fn parse_central(cd: &[u8], p: usize, cd_start: u64, base: u64) -> Result<(Entry, usize), Error> {
    let at = cd_start + p as u64;
    let truncated = || malformed("truncated central directory entry", at);
    let h = cd.get(p..p + 46).ok_or_else(truncated)?;
    let version_made_by = u16_at(h, 4);
    let flags = u16_at(h, 8);
    let method = u16_at(h, 10);
    let modified = DosDateTime::from_raw(u16_at(h, 14), u16_at(h, 12));
    let crc32 = u32_at(h, 16);
    let csize32 = u32_at(h, 20);
    let size32 = u32_at(h, 24);
    let name_len = usize::from(u16_at(h, 28));
    let extra_len = usize::from(u16_at(h, 30));
    let comment_len = usize::from(u16_at(h, 32));
    let external_attributes = u32_at(h, 38);
    let offset32 = u32_at(h, 42);
    let name_start = p + 46;
    let extra_start = name_start + name_len;
    let comment_start = extra_start + extra_len;
    let end = comment_start + comment_len;
    if end > cd.len() {
        return Err(truncated());
    }
    let raw_name = cd[name_start..extra_start].to_vec();

    let mut size = u64::from(size32);
    let mut compressed_size = u64::from(csize32);
    let mut offset = u64::from(offset32);
    let mut unicode_name = None;
    let mut modified_unix = None;
    let mut x = &cd[extra_start..comment_start];
    while x.len() >= 4 {
        let id = u16_at(x, 0);
        let n = usize::from(u16_at(x, 2));
        let Some(data) = x.get(4..4 + n) else {
            break; // a truncated trailing field (padding) is ignored, as Go and .NET do
        };
        match id {
            0x0001 => {
                let mut q = 0;
                for (marked, field) in [
                    (size32 == u32::MAX, &mut size),
                    (csize32 == u32::MAX, &mut compressed_size),
                    (offset32 == u32::MAX, &mut offset),
                ] {
                    if marked {
                        let v = data
                            .get(q..q + 8)
                            .ok_or(malformed("ZIP64 extra field too short", at))?;
                        *field = u64_at(v, 0);
                        q += 8;
                    }
                }
            }
            0x7075 if n >= 5 && data[0] == 1 && u32_at(data, 1) == dl_hash::crc32(&raw_name) => {
                unicode_name = std::str::from_utf8(&data[5..]).ok().map(str::to_owned);
            }
            0x5455 if n >= 5 && data[0] & 1 != 0 => {
                modified_unix = Some(i64::from(u32_at(data, 1)));
            }
            _ => {}
        }
        x = &x[4 + n..];
    }
    let name = if flags & FLAG_UTF8 != 0 {
        String::from_utf8_lossy(&raw_name).into_owned()
    } else if let Some(u) = unicode_name {
        u
    } else {
        cp437::decode(&raw_name)
    };
    let header_offset = offset
        .checked_add(base)
        .ok_or(malformed("local header offset out of range", at))?;
    Ok((
        Entry {
            name,
            raw_name,
            version_made_by,
            flags,
            method,
            modified,
            modified_unix,
            crc32,
            compressed_size,
            size,
            external_attributes,
            header_offset,
        },
        end,
    ))
}
