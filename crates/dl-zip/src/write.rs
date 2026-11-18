//! Writing archives.

use std::borrow::Cow;
use std::io::Write;

use crate::{
    DosDateTime, Error, Level, SIG_CENTRAL, SIG_EOCD, SIG_EOCD64, SIG_LOCAL, SIG_LOCATOR64, path,
};

/// How an entry's data is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// No compression (method 0).
    Stored,
    /// DEFLATE (method 8) at the given level. When the compressed data would not be
    /// smaller, the entry is stored instead.
    Deflate(Level),
}

/// An entry's modification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modified {
    /// Written as is into the header (usually local time).
    Dos(DosDateTime),
    /// Seconds since the Unix epoch: the header gets the UTC date-time, and an extended
    /// timestamp extra field (0x5455) keeps the exact value when it is in 0..=u32::MAX.
    Unix(i64),
}

/// Options for [`ZipWriter::add_file`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Compression method.
    pub method: Method,
    /// Modification time.
    pub modified: Modified,
}

impl Default for Options {
    /// Deflate at [`Level::Default`], modified 1980-01-01 00:00:00.
    fn default() -> Self {
        Options {
            method: Method::Deflate(Level::Default),
            modified: Modified::Dos(DosDateTime::default()),
        }
    }
}

/// Writes an archive to a stream, one complete entry at a time.
///
/// Every entry is compressed in memory, so its sizes and CRC-32 are known before its
/// local header is written: no data descriptors and no seeking. ZIP64 records are
/// written only when needed (more than 65 534 entries, or a size or offset of
/// 0xFFFFFFFF or more). Call [`finish`](Self::finish) to write the central directory;
/// after an error the archive is incomplete.
#[derive(Debug)]
pub struct ZipWriter<W: Write> {
    w: W,
    offset: u64,
    central: Vec<u8>,
    count: u64,
    /// Tests use this to write ZIP64 structures for small archives.
    pub(crate) force_zip64: bool,
}

const MADE_BY_UNIX: u16 = 3 << 8;
const MODE_FILE: u32 = 0o100_644 << 16;
const MODE_DIR: u32 = (0o040_755 << 16) | 0x10;

struct Header<'a> {
    name: &'a str,
    method: u16,
    crc: u32,
    size: u64,
    data: &'a [u8],
    modified: Modified,
    external: u32,
    is_dir: bool,
}

impl<W: Write> ZipWriter<W> {
    /// Starts an archive at the current position of `w`.
    pub fn new(w: W) -> Self {
        ZipWriter {
            w,
            offset: 0,
            central: Vec::new(),
            count: 0,
            force_zip64: false,
        }
    }

    /// Adds a file. Backslashes in `name` become `/`.
    ///
    /// # Errors
    /// [`Error::InvalidName`] for an empty, absolute (`/x`, `C:x`) or NUL-containing
    /// name, a `..` component, a trailing `/` or more than 65 535 bytes;
    /// [`Error::Io`].
    pub fn add_file(&mut self, name: &str, data: &[u8], options: Options) -> Result<(), Error> {
        let name = check_name(name)?;
        if name.ends_with('/') {
            return Err(Error::InvalidName {
                name: name.into_owned(),
                reason: "a file name must not end with '/'",
            });
        }
        let crc = dl_hash::crc32(data);
        let (method, body) = match options.method {
            Method::Stored => (0, Cow::Borrowed(data)),
            Method::Deflate(level) => {
                let c = dl_flate::deflate_raw::compress(data, level);
                if c.len() < data.len() {
                    (8, Cow::Owned(c))
                } else {
                    (0, Cow::Borrowed(data))
                }
            }
        };
        self.write_entry(&Header {
            name: &name,
            method,
            crc,
            size: data.len() as u64,
            data: &body,
            modified: options.modified,
            external: MODE_FILE,
            is_dir: false,
        })
    }

    /// Adds a directory entry; a `/` is appended to `name` when missing. Its time is
    /// 1980-01-01 00:00:00.
    ///
    /// # Errors
    /// As for [`add_file`](Self::add_file).
    pub fn add_directory(&mut self, name: &str) -> Result<(), Error> {
        let mut name = check_name(name)?.into_owned();
        if !name.ends_with('/') {
            name.push('/');
            if name.len() > usize::from(u16::MAX) {
                return Err(Error::InvalidName {
                    name,
                    reason: "longer than 65 535 bytes",
                });
            }
        }
        self.write_entry(&Header {
            name: &name,
            method: 0,
            crc: 0,
            size: 0,
            data: &[],
            modified: Modified::Dos(DosDateTime::default()),
            external: MODE_DIR,
            is_dir: true,
        })
    }

    fn write_entry(&mut self, h: &Header<'_>) -> Result<(), Error> {
        let csize = h.data.len() as u64;
        let (dos, unix) = match h.modified {
            Modified::Dos(d) => (d, None),
            Modified::Unix(t) => (DosDateTime::from_unix(t), u32::try_from(t).ok()),
        };
        let sizes64 = self.force_zip64 || h.size >= 0xFFFF_FFFF || csize >= 0xFFFF_FFFF;
        let offset64 = self.force_zip64 || self.offset >= 0xFFFF_FFFF;
        let needed: u16 = if sizes64 || offset64 {
            45
        } else if h.method == 8 || h.is_dir {
            20
        } else {
            10
        };
        let flags: u16 = if h.name.is_ascii() { 0 } else { 1 << 11 };
        let (size32, csize32) = if sizes64 {
            (u32::MAX, u32::MAX)
        } else {
            // Both are below 0xFFFFFFFF here.
            (h.size as u32, csize as u32)
        };

        let mut local_extra = Vec::new();
        if sizes64 {
            put16(&mut local_extra, 0x0001);
            put16(&mut local_extra, 16);
            put64(&mut local_extra, h.size);
            put64(&mut local_extra, csize);
        }
        let mut central_extra = Vec::new();
        if sizes64 || offset64 {
            let fields = [(sizes64, h.size), (sizes64, csize), (offset64, self.offset)];
            put16(&mut central_extra, 0x0001);
            put16(
                &mut central_extra,
                8 * fields.iter().filter(|f| f.0).count() as u16,
            );
            for (on, v) in fields {
                if on {
                    put64(&mut central_extra, v);
                }
            }
        }
        if let Some(t) = unix {
            put16(&mut local_extra, 0x5455);
            put16(&mut local_extra, 5);
            local_extra.push(1);
            put32(&mut local_extra, t);
            put16(&mut central_extra, 0x5455);
            put16(&mut central_extra, 5);
            central_extra.push(1);
            put32(&mut central_extra, t);
        }

        // Names are at most 65 535 bytes (check_name); extras are at most 37 bytes.
        let name_len = h.name.len() as u16;
        let mut local = Vec::with_capacity(30 + h.name.len() + local_extra.len());
        put32(&mut local, SIG_LOCAL);
        put16(&mut local, needed);
        put16(&mut local, flags);
        put16(&mut local, h.method);
        put16(&mut local, dos.raw_time());
        put16(&mut local, dos.raw_date());
        put32(&mut local, h.crc);
        put32(&mut local, csize32);
        put32(&mut local, size32);
        put16(&mut local, name_len);
        put16(&mut local, local_extra.len() as u16);
        local.extend_from_slice(h.name.as_bytes());
        local.extend_from_slice(&local_extra);
        self.w.write_all(&local)?;
        self.w.write_all(h.data)?;

        let c = &mut self.central;
        put32(c, SIG_CENTRAL);
        put16(c, MADE_BY_UNIX | needed.max(20));
        put16(c, needed);
        put16(c, flags);
        put16(c, h.method);
        put16(c, dos.raw_time());
        put16(c, dos.raw_date());
        put32(c, h.crc);
        put32(c, csize32);
        put32(c, size32);
        put16(c, name_len);
        put16(c, central_extra.len() as u16);
        put16(c, 0); // comment length
        put16(c, 0); // disk number
        put16(c, 0); // internal attributes
        put32(c, h.external);
        put32(
            c,
            if offset64 {
                u32::MAX
            } else {
                self.offset as u32
            },
        );
        c.extend_from_slice(h.name.as_bytes());
        c.extend_from_slice(&central_extra);

        self.offset += local.len() as u64 + csize;
        self.count += 1;
        Ok(())
    }

    /// Writes the central directory and the end records, flushes and returns the writer.
    ///
    /// # Errors
    /// [`Error::Io`].
    pub fn finish(mut self) -> Result<W, Error> {
        let cd_start = self.offset;
        let cd_size = self.central.len() as u64;
        self.w.write_all(&self.central)?;
        let zip64 = self.force_zip64
            || self.count >= 0xFFFF
            || cd_size >= 0xFFFF_FFFF
            || cd_start >= 0xFFFF_FFFF;
        let mut end = Vec::with_capacity(98);
        if zip64 {
            let record = cd_start + cd_size;
            put32(&mut end, SIG_EOCD64);
            put64(&mut end, 44); // size of the rest of the record
            put16(&mut end, MADE_BY_UNIX | 45);
            put16(&mut end, 45);
            put32(&mut end, 0); // this disk
            put32(&mut end, 0); // disk with the central directory
            put64(&mut end, self.count);
            put64(&mut end, self.count);
            put64(&mut end, cd_size);
            put64(&mut end, cd_start);
            put32(&mut end, SIG_LOCATOR64);
            put32(&mut end, 0);
            put64(&mut end, record);
            put32(&mut end, 1); // total disks
        }
        put32(&mut end, SIG_EOCD);
        put16(&mut end, 0);
        put16(&mut end, 0);
        // With ZIP64 every field is a marker (as Go writes), so every reader looks for
        // the ZIP64 record.
        let n = if zip64 { u16::MAX } else { self.count as u16 };
        put16(&mut end, n);
        put16(&mut end, n);
        put32(&mut end, if zip64 { u32::MAX } else { cd_size as u32 });
        put32(&mut end, if zip64 { u32::MAX } else { cd_start as u32 });
        put16(&mut end, 0); // comment length
        self.w.write_all(&end)?;
        self.w.flush()?;
        Ok(self.w)
    }
}

/// Converts `\` to `/` and refuses names that could not be extracted safely.
fn check_name(name: &str) -> Result<Cow<'_, str>, Error> {
    let bad = |reason| Error::InvalidName {
        name: name.to_owned(),
        reason,
    };
    if name.is_empty() {
        return Err(bad("empty"));
    }
    if name.len() > usize::from(u16::MAX) {
        return Err(bad("longer than 65 535 bytes"));
    }
    match path::components(name, false) {
        None => return Err(bad("absolute, contains '..', NUL or control characters")),
        Some(p) if p.is_empty() => return Err(bad("no path component")),
        Some(_) => {}
    }
    Ok(if name.contains('\\') {
        Cow::Owned(name.replace('\\', "/"))
    } else {
        Cow::Borrowed(name)
    })
}

fn put16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn put64(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_le_bytes());
}
