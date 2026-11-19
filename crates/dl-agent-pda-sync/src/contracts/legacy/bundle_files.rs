//! Reading a verified legacy bundle (`Legacy/LegacyBundleFiles.cs`, ADR 0085/0089).

use std::io::Read;
use std::path::{Path, PathBuf};

use dl_hash::{Digest, Sha256};

use super::bundle::{self, LegacyBundleFile, LegacyBundleManifest};

/// Largest decompressed table file read into memory (2 GiB).
pub const MAX_TABLE_BYTES: usize = 1 << 31;

/// A bundle that is unreadable or failed verification; the message names the file and
/// the problem (`LegacyBundleException`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyBundleError {
    /// The message.
    pub message: String,
}

impl LegacyBundleError {
    fn new(message: impl Into<String>) -> LegacyBundleError {
        LegacyBundleError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for LegacyBundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LegacyBundleError {}

/// A bundle whose manifest and every file hash were checked.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedLegacyBundle {
    /// Bundle directory.
    pub dir: PathBuf,
    /// The manifest.
    pub manifest: LegacyBundleManifest,
    /// SHA-256 of `manifest.json` (the bundle's identity).
    pub manifest_sha256: String,
}

impl VerifiedLegacyBundle {
    /// The file of `table`.
    pub fn file(&self, table: &str) -> Option<&LegacyBundleFile> {
        self.manifest.files.iter().find(|f| f.table == table)
    }
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut sha = Sha256::default();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
    }
    Ok(dl_encoding::hex::encode(sha.finalize()))
}

/// Checks the manifest format and version, that every file sits at its canonical path,
/// appears once and matches its SHA-256; all problems are reported together.
pub fn read_verified(dir: &Path) -> Result<VerifiedLegacyBundle, LegacyBundleError> {
    let path = dir.join(bundle::MANIFEST_FILE);
    let shown = path.display();
    if !path.is_file() {
        return Err(LegacyBundleError::new(format!(
            "{shown} not found: --in is not a legacy bundle directory."
        )));
    }
    let bytes = std::fs::read(&path)
        .map_err(|e| LegacyBundleError::new(format!("{shown} could not be read: {e}")))?;
    let not_readable =
        || LegacyBundleError::new(format!("{shown} is not a readable legacy manifest."));
    let text = std::str::from_utf8(&bytes).map_err(|_| not_readable())?;
    let m = LegacyBundleManifest::from_json(text).map_err(|_| not_readable())?;
    let m = match m {
        Some(m) if m.format == bundle::FORMAT => m,
        _ => {
            return Err(LegacyBundleError::new(format!(
                "{shown} is not a {} manifest.",
                bundle::FORMAT
            )));
        }
    };
    if m.format_version != bundle::FORMAT_VERSION {
        return Err(LegacyBundleError::new(format!(
            "Bundle format version {}; this tool reads version {}. Use the matching release.",
            m.format_version,
            bundle::FORMAT_VERSION
        )));
    }
    let mut problems = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for f in &m.files {
        if !bundle::is_valid_table_name(&f.table)
            || bundle::path_for(&f.table).as_deref() != Some(f.path.as_str())
        {
            problems.push(format!("{}: unexpected path '{}'", f.table, f.path));
            continue;
        }
        if seen.contains(&f.table.as_str()) {
            problems.push(format!("{}: listed twice", f.table));
        } else {
            seen.push(&f.table);
        }
        let full = dir.join(&f.path);
        if !full.is_file() {
            problems.push(format!("{}: {} is missing", f.table, f.path));
        } else {
            let ok = sha256_file(&full).is_ok_and(|h| h.eq_ignore_ascii_case(&f.sha256));
            if !ok {
                problems.push(format!(
                    "{}: SHA-256 mismatch (file changed after export)",
                    f.table
                ));
            }
        }
    }
    if !problems.is_empty() {
        return Err(LegacyBundleError::new(format!(
            "Bundle failed verification: {}",
            problems.join("; ")
        )));
    }
    Ok(VerifiedLegacyBundle {
        dir: dir.to_path_buf(),
        manifest: m,
        manifest_sha256: dl_encoding::hex::encode(Sha256::digest(&bytes)),
    })
}

/// The rows of one verified file, one JSON object per non-empty line; after the last
/// row the count is checked against the manifest (an error item when it differs).
pub fn read_rows(
    bundle: &VerifiedLegacyBundle,
    file: &LegacyBundleFile,
) -> Result<RowReader, LegacyBundleError> {
    let full = bundle.dir.join(&file.path);
    let gz = std::fs::read(&full)
        .map_err(|e| LegacyBundleError::new(format!("{} could not be read: {e}", file.path)))?;
    let raw = dl_flate::gzip::decompress(&gz, MAX_TABLE_BYTES).map_err(|e| {
        LegacyBundleError::new(format!("{} is not a valid gzip file: {e}", file.path))
    })?;
    let mut text = String::from_utf8_lossy(&raw).into_owned();
    if text.starts_with('\u{feff}') {
        text.remove(0);
    }
    Ok(RowReader {
        text,
        pos: 0,
        rows: 0,
        expected: file.rows,
        path: file.path.clone(),
        done: false,
    })
}

/// Iterator over the rows of a bundle file (see [`read_rows`]).
#[derive(Debug)]
pub struct RowReader {
    text: String,
    pos: usize,
    rows: i64,
    expected: i64,
    path: String,
    done: bool,
}

impl RowReader {
    /// The next line (`StreamReader.ReadLine`: `\n`, `\r` and `\r\n` end a line).
    fn next_line(&mut self) -> Option<(usize, usize)> {
        if self.pos >= self.text.len() {
            return None;
        }
        let b = self.text.as_bytes();
        let start = self.pos;
        let mut i = start;
        while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
            i += 1;
        }
        let end = i;
        if i < b.len() {
            i += if b[i] == b'\r' && b.get(i + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
        }
        self.pos = i;
        Some((start, end))
    }
}

impl Iterator for RowReader {
    type Item = Result<dl_json::Map, LegacyBundleError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            let Some((s, e)) = self.next_line() else {
                self.done = true;
                if self.rows != self.expected {
                    return Some(Err(LegacyBundleError::new(format!(
                        "{} has {} rows; the manifest says {}.",
                        self.path, self.rows, self.expected
                    ))));
                }
                return None;
            };
            if s == e {
                continue;
            }
            self.rows += 1;
            let n = self.rows;
            let parsed = crate::util::stj::parse(&self.text[s..e]);
            return Some(match parsed {
                Err(_) => {
                    self.done = true;
                    Err(LegacyBundleError::new(format!(
                        "{} line {n} is not valid JSON.",
                        self.path
                    )))
                }
                Ok(dl_json::Value::Object(m)) => Ok(m),
                Ok(_) => {
                    self.done = true;
                    Err(LegacyBundleError::new(format!(
                        "{} line {n} is not a JSON object.",
                        self.path
                    )))
                }
            });
        }
    }
}
