//! Backup files at rest: the copy sent to a destination is sealed under the tenant's
//! backup data key, so the destination (a bucket, a Drive folder, a share) never holds a
//! readable database and one tenant's key never opens another tenant's backup.
//!
//! Layout (`DLBKENC1`), all integers big-endian:
//!
//! ```text
//! magic "DLBKENC1" (8) | key id length (1) | key id (1..=64, ASCII) | salt (32) | chunk size (4)
//! chunk 0 | chunk 1 | ... | final chunk
//! ```
//!
//! The file key is HKDF-SHA256(ikm = the 32-byte data key, salt = the file's random salt,
//! info = `dl-agent-backup/file/v1` ‖ the whole header), so every file has its own key and
//! any change to the header (key id, salt, chunk size) makes every chunk fail to open. Each
//! chunk is AES-256-GCM of up to `chunk size` plaintext bytes under that key, nonce
//! `0000 ‖ u64 index`, AAD one byte (1 on the final chunk, else 0). Every chunk but the last
//! holds exactly `chunk size` bytes; the final chunk holds fewer (possibly none), so a file
//! cut at a chunk boundary, reordered chunks or appended data are all refused.
//!
//! A file without the magic is a backup from before encryption (plaintext `.bak`) and is
//! restored as it is: old backups stay restorable.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use dl_tds::CancelToken;

use super::{gcm, hkdf_sha256};
use crate::Error;

/// The first bytes of a sealed backup file.
pub const MAGIC: &[u8; 8] = b"DLBKENC1";
/// Plaintext bytes per chunk the agent writes.
pub const CHUNK: usize = 1 << 20;
/// The largest chunk size a reader accepts (bounds the memory a hostile header can ask for).
const MAX_CHUNK: usize = 16 << 20;
const SALT_LEN: usize = 32;
const TAG_LEN: usize = 16;
const INFO: &[u8] = b"dl-agent-backup/file/v1";

/// A tenant's backup data key and its id. `Debug` never shows the key.
#[derive(Clone, PartialEq, Eq)]
pub struct DataKey {
    /// The hub's id for the key (recorded in every file and report).
    pub id: String,
    /// The 32 key bytes.
    pub key: [u8; 32],
}

impl std::fmt::Debug for DataKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataKey")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl DataKey {
    /// A key from the hub's answer: `id` and the base64url (no padding) key bytes.
    pub fn from_hub(id: &str, key_b64: &str) -> Result<DataKey, Error> {
        check_key_id(id)?;
        let raw = dl_encoding::base64::URL_SAFE_NO_PAD
            .decode(key_b64.trim_end_matches('='))
            .map_err(|e| Error::Credential(format!("backup key {id}: decode: {e}")))?;
        let key: [u8; 32] = raw.as_slice().try_into().map_err(|_| {
            Error::Credential(format!("backup key {id}: want 32 bytes, got {}", raw.len()))
        })?;
        Ok(DataKey {
            id: id.to_owned(),
            key,
        })
    }
}

/// Where the backup data keys come from: the hub in production ([`crate::core::Core`]).
pub trait BackupKeys: Send + Sync {
    /// The tenant's current key; `None` when the hub has no per-tenant keys (an older hub),
    /// in which case the backup is uploaded as before, unencrypted.
    fn current(&self) -> Result<Option<DataKey>, Error>;
    /// The key a sealed file names. A key of another tenant is refused by the hub and comes
    /// back as [`Error::Credential`].
    fn by_id(&self, id: &str) -> Result<DataKey, Error>;
}

/// No keys: backups stay unencrypted and a sealed file cannot be opened.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoKeys;

impl BackupKeys for NoKeys {
    fn current(&self) -> Result<Option<DataKey>, Error> {
        Ok(None)
    }
    fn by_id(&self, id: &str) -> Result<DataKey, Error> {
        Err(Error::Credential(format!(
            "the backup is encrypted under key {id}, but no backup keys are available"
        )))
    }
}

/// Key ids are short printable tokens: `[A-Za-z0-9._:-]{1,64}`.
pub fn check_key_id(id: &str) -> Result<(), Error> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(Error::Credential(format!("invalid backup key id {id:?}")))
    }
}

/// The key id of a sealed file, `None` for a plaintext (legacy) backup.
pub fn key_id_of(path: &Path) -> Result<Option<String>, Error> {
    let mut f = File::open(path).map_err(|e| Error::io(&format!("open {}", path.display()), &e))?;
    let mut magic = [0u8; 8];
    let n = read_full(&mut f, &mut magic)
        .map_err(|e| Error::io(&format!("read {}", path.display()), &e))?;
    if n < MAGIC.len() || &magic != MAGIC {
        return Ok(None);
    }
    read_header(&mut f).map(|h| Some(h.key_id))
}

/// Seals `src` into `dst` under `key`; returns the bytes written. `dst` is removed on failure.
pub fn seal_file(
    src: &Path,
    dst: &Path,
    key: &DataKey,
    cancel: &CancelToken,
) -> Result<u64, Error> {
    let salt: [u8; SALT_LEN] = dl_rand::secure_bytes()
        .map_err(|e| Error::Credential(format!("backup encryption: random salt: {e}")))?;
    let res = seal_with(src, dst, key, &salt, CHUNK, cancel);
    if res.is_err() {
        remove_partial(dst);
    }
    res
}

fn seal_with(
    src: &Path,
    dst: &Path,
    key: &DataKey,
    salt: &[u8; SALT_LEN],
    chunk: usize,
    cancel: &CancelToken,
) -> Result<u64, Error> {
    check_key_id(&key.id)?;
    let mut input =
        File::open(src).map_err(|e| Error::io(&format!("open {}", src.display()), &e))?;
    let mut out =
        File::create(dst).map_err(|e| Error::io(&format!("create {}", dst.display()), &e))?;
    let header = header_bytes(&key.id, salt, chunk);
    out.write_all(&header)
        .map_err(|e| Error::io(&format!("write {}", dst.display()), &e))?;
    let file_key = file_key(&key.key, salt, &header);
    let mut buf = vec![0u8; chunk];
    let mut written = header.len() as u64;
    for index in 0u64.. {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let n = read_full(&mut input, &mut buf)
            .map_err(|e| Error::io(&format!("read {}", src.display()), &e))?;
        let last = n < chunk;
        let sealed = gcm::seal(&file_key, &nonce(index), &[u8::from(last)], &buf[..n])
            .map_err(|e| Error::Credential(format!("backup encryption: {e}")))?;
        out.write_all(&sealed)
            .map_err(|e| Error::io(&format!("write {}", dst.display()), &e))?;
        written += sealed.len() as u64;
        if last {
            break;
        }
    }
    out.sync_all()
        .map_err(|e| Error::io(&format!("flush {}", dst.display()), &e))?;
    Ok(written)
}

/// Opens the sealed `src` into `dst` with the key it names, looked up in `keys`. The
/// plaintext goes to `<dst>.part` first and is renamed only once every chunk has been
/// authenticated: a wrong key, an altered or a cut file leaves no plaintext behind.
pub fn open_file(
    src: &Path,
    dst: &Path,
    keys: &dyn BackupKeys,
    cancel: &CancelToken,
) -> Result<String, Error> {
    let part = part_path(dst);
    let res = open_into(src, &part, keys, cancel).and_then(|id| {
        std::fs::rename(&part, dst)
            .map_err(|e| Error::io(&format!("rename {}", part.display()), &e))?;
        Ok(id)
    });
    if res.is_err() {
        remove_partial(&part);
    }
    res
}

fn open_into(
    src: &Path,
    part: &Path,
    keys: &dyn BackupKeys,
    cancel: &CancelToken,
) -> Result<String, Error> {
    let mut input =
        File::open(src).map_err(|e| Error::io(&format!("open {}", src.display()), &e))?;
    let mut magic = [0u8; 8];
    let n = read_full(&mut input, &mut magic)
        .map_err(|e| Error::io(&format!("read {}", src.display()), &e))?;
    if n < MAGIC.len() || &magic != MAGIC {
        return Err(Error::Credential(format!(
            "{} is not an encrypted backup",
            src.display()
        )));
    }
    let h = read_header(&mut input)?;
    let key = keys.by_id(&h.key_id)?;
    if key.id != h.key_id {
        return Err(Error::Credential(format!(
            "asked for backup key {}, got {}",
            h.key_id, key.id
        )));
    }
    let header = header_bytes(&h.key_id, &h.salt, h.chunk);
    let file_key = file_key(&key.key, &h.salt, &header);
    let mut out =
        File::create(part).map_err(|e| Error::io(&format!("create {}", part.display()), &e))?;
    let mut buf = vec![0u8; h.chunk + TAG_LEN];
    for index in 0u64.. {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let n = read_full(&mut input, &mut buf)
            .map_err(|e| Error::io(&format!("read {}", src.display()), &e))?;
        if n < TAG_LEN {
            return Err(Error::Credential(format!(
                "decrypt backup with key {}: the file is truncated",
                h.key_id
            )));
        }
        let last = n < buf.len();
        let plain =
            gcm::open(&file_key, &nonce(index), &[u8::from(last)], &buf[..n]).map_err(|_| {
                Error::Credential(format!(
                    "decrypt backup with key {}: authentication failed (wrong key, or the file \
                     was altered or cut)",
                    h.key_id
                ))
            })?;
        out.write_all(&plain)
            .map_err(|e| Error::io(&format!("write {}", part.display()), &e))?;
        if last {
            break;
        }
    }
    out.sync_all()
        .map_err(|e| Error::io(&format!("flush {}", part.display()), &e))?;
    Ok(h.key_id)
}

struct Header {
    key_id: String,
    salt: [u8; SALT_LEN],
    chunk: usize,
}

/// Reads the header after the magic.
fn read_header(f: &mut File) -> Result<Header, Error> {
    let bad = |what: &str| Error::Credential(format!("encrypted backup header: {what}"));
    let mut len = [0u8; 1];
    f.read_exact(&mut len).map_err(|_| bad("truncated"))?;
    let mut id = vec![0u8; usize::from(len[0])];
    f.read_exact(&mut id).map_err(|_| bad("truncated"))?;
    let key_id = String::from_utf8(id).map_err(|_| bad("key id is not text"))?;
    check_key_id(&key_id)?;
    let mut salt = [0u8; SALT_LEN];
    f.read_exact(&mut salt).map_err(|_| bad("truncated"))?;
    let mut size = [0u8; 4];
    f.read_exact(&mut size).map_err(|_| bad("truncated"))?;
    let chunk = u32::from_be_bytes(size) as usize;
    if chunk == 0 || chunk > MAX_CHUNK {
        return Err(bad(&format!("chunk size {chunk} out of range")));
    }
    Ok(Header {
        key_id,
        salt,
        chunk,
    })
}

fn header_bytes(key_id: &str, salt: &[u8; SALT_LEN], chunk: usize) -> Vec<u8> {
    let mut h = Vec::with_capacity(8 + 1 + key_id.len() + SALT_LEN + 4);
    h.extend_from_slice(MAGIC);
    // check_key_id bounds the id to 64 bytes.
    h.push(key_id.len() as u8);
    h.extend_from_slice(key_id.as_bytes());
    h.extend_from_slice(salt);
    h.extend_from_slice(&(chunk as u32).to_be_bytes());
    h
}

fn file_key(key: &[u8; 32], salt: &[u8], header: &[u8]) -> [u8; 32] {
    let mut info = Vec::with_capacity(INFO.len() + header.len());
    info.extend_from_slice(INFO);
    info.extend_from_slice(header);
    let mut out = [0u8; 32];
    hkdf_sha256(key, salt, &info, &mut out);
    out
}

fn nonce(index: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&index.to_be_bytes());
    n
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// `<dst>.part`.
pub fn part_path(dst: &Path) -> PathBuf {
    let mut s = dst.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Removes a partial output; the caller's error is what matters, a leftover is logged.
fn remove_partial(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => crate::log::warn(
            "could not remove a partial backup file",
            &[
                ("path", &path.display().to_string()),
                ("error", &e.to_string()),
            ],
        ),
    }
}

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod tests;
