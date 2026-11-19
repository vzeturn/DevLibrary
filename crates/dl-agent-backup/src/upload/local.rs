//! A local or mapped folder (`provider: LocalFolder`, token `remoteFolderPath` or `path`).
//! The cloud path is the copy's full path.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use dl_json::Value;
use dl_tds::CancelToken;

use super::{Progress, Uploader, base_name, text};
use crate::Error;

const CHUNK: usize = 4 * 1024 * 1024;

/// Copies backups into a folder.
#[derive(Debug, Clone)]
pub struct Local {
    folder: PathBuf,
}

impl Local {
    /// The folder named by the tokens.
    pub fn from_tokens(tokens: &Value) -> Result<Local, Error> {
        let folder = [text(tokens, "remoteFolderPath"), text(tokens, "path")]
            .into_iter()
            .find(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::Upload("local folder destination needs remoteFolderPath".into())
            })?;
        Ok(Local {
            folder: PathBuf::from(folder),
        })
    }

    /// A folder destination for tests and settings.
    pub fn new(folder: impl Into<PathBuf>) -> Local {
        Local {
            folder: folder.into(),
        }
    }

    fn inside(&self, cloud_path: &str) -> Result<PathBuf, Error> {
        let p = PathBuf::from(cloud_path);
        if !p.starts_with(&self.folder) || p.components().any(|c| c.as_os_str() == "..") {
            return Err(Error::Upload(format!(
                "{cloud_path} is outside the destination folder"
            )));
        }
        Ok(p)
    }
}

fn copy(
    from: &Path,
    to: &Path,
    cancel: &CancelToken,
    progress: Progress<'_>,
) -> Result<u64, Error> {
    let mut src =
        fs::File::open(from).map_err(|e| Error::Upload(format!("open {}: {e}", from.display())))?;
    let total = src.metadata().map(|m| m.len()).unwrap_or(0);
    let tmp = to.with_extension("partial");
    let mut dst = fs::File::create(&tmp)
        .map_err(|e| Error::Upload(format!("create {}: {e}", tmp.display())))?;
    let mut buf = vec![0u8; CHUNK];
    let mut sent = 0u64;
    let res = (|| {
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let n = super::read_full(&mut src, &mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut dst, &buf[..n])
                .map_err(|e| Error::Upload(format!("write {}: {e}", tmp.display())))?;
            sent += n as u64;
            progress(sent, total);
        }
        dst.sync_all()
            .map_err(|e| Error::Upload(format!("flush {}: {e}", tmp.display())))?;
        fs::rename(&tmp, to).map_err(|e| Error::Upload(format!("rename to {}: {e}", to.display())))
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp); // best effort: the error that matters is returned below
    }
    res.map(|()| sent)
}

impl Uploader for Local {
    fn upload(
        &self,
        file: &Path,
        cancel: &CancelToken,
        progress: Progress<'_>,
    ) -> Result<String, Error> {
        let name = base_name(file)?;
        fs::create_dir_all(&self.folder)
            .map_err(|e| Error::Upload(format!("create {}: {e}", self.folder.display())))?;
        let dest = self.folder.join(name);
        copy(file, &dest, cancel, progress)?;
        Ok(dest.to_string_lossy().into_owned())
    }

    fn download(&self, cloud_path: &str, dest: &Path, cancel: &CancelToken) -> Result<u64, Error> {
        let from = self.inside(cloud_path)?;
        copy(&from, dest, cancel, &mut |_, _| {})
    }

    fn delete(&self, cloud_path: &str) -> Result<(), Error> {
        let p = self.inside(cloud_path)?;
        match fs::remove_file(&p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Upload(format!("delete {}: {e}", p.display()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "dl-backup-local-{name}-{}",
            dl_uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn upload_download_delete_round_trip() {
        let root = temp("rt");
        let src = root.join("db_1.bak");
        fs::write(&src, vec![7u8; CHUNK + 10]).unwrap();
        let up = Local::new(root.join("dest"));
        let mut calls = Vec::new();
        let cloud = up
            .upload(&src, &CancelToken::new(), &mut |s, t| calls.push((s, t)))
            .unwrap();
        assert_eq!(PathBuf::from(&cloud), root.join("dest").join("db_1.bak"));
        assert_eq!(
            calls.last(),
            Some(&((CHUNK + 10) as u64, (CHUNK + 10) as u64))
        );
        let back = root.join("back.bak");
        assert_eq!(
            up.download(&cloud, &back, &CancelToken::new()).unwrap(),
            (CHUNK + 10) as u64
        );
        assert_eq!(fs::read(&back).unwrap(), fs::read(&src).unwrap());
        up.delete(&cloud).unwrap();
        up.delete(&cloud).unwrap();
        assert!(
            up.delete(&root.join("elsewhere.bak").to_string_lossy())
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancel_and_missing_folder() {
        let root = temp("cancel");
        let src = root.join("x.bak");
        fs::write(&src, b"abc").unwrap();
        let t = CancelToken::new();
        t.cancel();
        let err = Local::new(root.join("d"))
            .upload(&src, &t, &mut |_, _| {})
            .unwrap_err();
        assert_eq!(err, Error::Cancelled);
        assert!(Local::from_tokens(&dl_json::json!({})).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
