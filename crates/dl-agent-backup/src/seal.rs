//! At-rest protection for the module's state files, the same as einvoice `config.enc`:
//! DPAPI in machine scope on Windows around a 4-byte big-endian length prefix; off Windows
//! (development only) the framed bytes are stored as they are in owner-only (0600) files.

use std::path::Path;

use dl_json::Value;

use crate::Error;

fn wrap(p: &[u8]) -> Vec<u8> {
    let mut out = (p.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(p);
    out
}

fn unwrap(b: &[u8]) -> Result<Vec<u8>, String> {
    let (len, rest) = b.split_at_checked(4).ok_or("sealed data too short")?;
    let n = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
    if n != rest.len() {
        return Err("sealed data length prefix mismatch".into());
    }
    Ok(rest.to_vec())
}

/// Seals `p` for this machine.
#[cfg(windows)]
pub fn seal(p: &[u8]) -> Result<Vec<u8>, Error> {
    dl_winsvc::dpapi::protect(&wrap(p), dl_winsvc::dpapi::Scope::LocalMachine)
        .map_err(|e| Error::Io(format!("dpapi protect: {e}")))
}

/// Opens what [`seal`] produced on this machine.
#[cfg(windows)]
pub fn open(blob: &[u8]) -> Result<Vec<u8>, Error> {
    if blob.is_empty() {
        return Err(Error::Io("sealed file is empty".into()));
    }
    let plain = dl_winsvc::dpapi::unprotect(blob, dl_winsvc::dpapi::Scope::LocalMachine)
        .map_err(|e| Error::Io(format!("dpapi unprotect: {e}")))?;
    unwrap(&plain).map_err(Error::Io)
}

/// Seals `p` for this machine (framing only off Windows).
#[cfg(not(windows))]
pub fn seal(p: &[u8]) -> Result<Vec<u8>, Error> {
    Ok(wrap(p))
}

/// Opens what [`seal`] produced.
#[cfg(not(windows))]
pub fn open(blob: &[u8]) -> Result<Vec<u8>, Error> {
    unwrap(blob).map_err(Error::Io)
}

/// Writes through a temporary file and renames it over `path`; owner-only on Unix.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    let ctx = |e: std::io::Error| Error::io(&format!("write {}", path.display()), &e);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(ctx)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).map_err(ctx)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(ctx)?;
        }
        f.write_all(data).map_err(ctx)?;
        f.sync_all().map_err(ctx)?;
    }
    std::fs::rename(&tmp, path).map_err(ctx)
}

/// Seals `doc` as JSON into `path`.
pub fn write_json(path: &Path, doc: &Value) -> Result<(), Error> {
    write_atomic(path, &seal(dl_json::to_string(doc).as_bytes())?)
}

/// Reads a sealed JSON file; `None` when it does not exist.
pub fn read_json(path: &Path) -> Result<Option<Value>, Error> {
    let blob = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(&format!("read {}", path.display()), &e)),
    };
    let plain = open(&blob)?;
    dl_json::parse_slice(&plain)
        .map(Some)
        .map_err(|e| Error::Io(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_round_trips_and_rejects_truncation() {
        let blob = seal(b"hello").unwrap();
        assert_eq!(open(&blob).unwrap(), b"hello");
        assert!(open(&blob[..blob.len() - 1]).is_err());
        assert!(open(&[0, 0]).is_err());
    }

    #[test]
    fn json_file_round_trips_and_missing_is_none() {
        let dir = std::env::temp_dir().join(format!("dlbk-seal-{}", dl_uuid::Uuid::new_v4()));
        let path = dir.join("x.enc");
        assert_eq!(read_json(&path).unwrap(), None);
        write_json(&path, &dl_json::json!({"a": 1})).unwrap();
        assert_eq!(read_json(&path).unwrap(), Some(dl_json::json!({"a": 1})));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
