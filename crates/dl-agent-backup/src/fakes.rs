//! Hand-written fakes for the SQL and destination seams, shared by the pipeline, restore,
//! executor and sync tests.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dl_tds::CancelToken;

use crate::Error;
use crate::sql::{BackupFile, Connector, ServerInfo, SqlConn, Target};
use crate::upload::{Destination, Kind, Progress, Real, Uploader, Uploaders};

/// What the fake server does and what it was asked.
#[derive(Debug, Clone)]
pub struct SqlState {
    pub connect_error: Option<Error>,
    pub info: ServerInfo,
    pub backup_dir: Option<String>,
    pub recovery: HashMap<String, String>,
    pub run_error: Option<Error>,
    /// BACKUP writes `content` to `@path` (as a local SQL Server would).
    pub write_file: bool,
    pub content: Vec<u8>,
    /// BACKUP / RESTORE block until the cancel token fires.
    pub hang: bool,
    pub files: Vec<BackupFile>,
    pub data_dirs: (String, String),
    pub databases: Vec<String>,
    pub connects: Vec<Target>,
    pub statements: Vec<(String, Vec<(String, String)>)>,
    /// Databases the login may not `BACKUP`.
    pub backup_denied: Vec<String>,
    /// Databases whose BACKUP permission was asked for.
    pub perm_checked: Vec<String>,
}

impl Default for SqlState {
    fn default() -> SqlState {
        SqlState {
            connect_error: None,
            info: ServerInfo {
                engine_edition: 3,
                edition: "Developer Edition (64-bit)".into(),
                major: 16,
                minor: 0,
                transport: "as-configured".into(),
            },
            backup_dir: None,
            recovery: HashMap::new(),
            run_error: None,
            write_file: true,
            content: b"fake backup bytes".to_vec(),
            hang: false,
            files: vec![
                BackupFile {
                    logical: "Sales".into(),
                    kind: "D".into(),
                },
                BackupFile {
                    logical: "Sales_log".into(),
                    kind: "L".into(),
                },
            ],
            data_dirs: ("/var/opt/mssql/data".into(), "/var/opt/mssql/log".into()),
            databases: vec![
                "master".into(),
                "model".into(),
                "msdb".into(),
                "tempdb".into(),
                "Sales".into(),
            ],
            connects: Vec::new(),
            statements: Vec::new(),
            backup_denied: Vec::new(),
            perm_checked: Vec::new(),
        }
    }
}

/// The fake connector; clones share one state.
#[derive(Debug, Clone, Default)]
pub struct FakeSql(pub Arc<Mutex<SqlState>>);

impl FakeSql {
    pub fn new(state: SqlState) -> FakeSql {
        FakeSql(Arc::new(Mutex::new(state)))
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, SqlState> {
        self.0.lock().unwrap()
    }
}

impl Connector for FakeSql {
    fn connect(&self, target: &Target) -> Result<Box<dyn SqlConn>, Error> {
        let mut s = self.state();
        s.connects.push(target.clone());
        if let Some(e) = &s.connect_error {
            return Err(e.clone());
        }
        let info = s.info.clone();
        Ok(Box::new(FakeConn {
            shared: self.0.clone(),
            info,
            cancel: None,
        }))
    }
}

struct FakeConn {
    shared: Arc<Mutex<SqlState>>,
    info: ServerInfo,
    cancel: Option<CancelToken>,
}

impl SqlConn for FakeConn {
    fn info(&self) -> &ServerInfo {
        &self.info
    }

    fn backup_dir(&mut self) -> Result<Option<String>, Error> {
        Ok(self.shared.lock().unwrap().backup_dir.clone())
    }

    fn can_backup(&mut self, database: &str) -> Result<Option<bool>, Error> {
        let mut s = self.shared.lock().unwrap();
        s.perm_checked.push(database.to_owned());
        Ok(Some(!s.backup_denied.iter().any(|d| d == database)))
    }

    fn recovery_model(&mut self, database: &str) -> Result<Option<String>, Error> {
        Ok(self.shared.lock().unwrap().recovery.get(database).cloned())
    }

    fn run(&mut self, statement: &str, params: &[(String, String)]) -> Result<(), Error> {
        let (hang, err, write, content) = {
            let mut s = self.shared.lock().unwrap();
            s.statements.push((statement.to_owned(), params.to_vec()));
            (s.hang, s.run_error.clone(), s.write_file, s.content.clone())
        };
        if hang {
            loop {
                if self.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
                    return Err(Error::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if let Some(e) = err {
            return Err(e);
        }
        if write && statement.starts_with("BACKUP") {
            let path = params
                .iter()
                .find(|(k, _)| k == "path")
                .map(|(_, v)| v.clone())
                .unwrap();
            std::fs::write(path, content).unwrap();
        }
        Ok(())
    }

    fn file_list(&mut self, _path: &str) -> Result<Vec<BackupFile>, Error> {
        Ok(self.shared.lock().unwrap().files.clone())
    }

    fn data_dirs(&mut self) -> Result<(String, String), Error> {
        Ok(self.shared.lock().unwrap().data_dirs.clone())
    }

    fn list_databases(&mut self) -> Result<Vec<String>, Error> {
        Ok(self.shared.lock().unwrap().databases.clone())
    }

    fn set_cancel(&mut self, token: Option<CancelToken>) {
        self.cancel = token;
    }
}

/// What the fake destination does and what it was asked.
#[derive(Debug, Clone, Default)]
pub struct DestState {
    pub open_error: Option<Error>,
    pub upload_error: Option<Error>,
    pub download_error: Option<Error>,
    pub delete_error: Option<Error>,
    pub uploaded: Vec<String>,
    pub downloaded: Vec<(String, String)>,
    pub deleted: Vec<String>,
    pub content: Vec<u8>,
}

/// The fake destinations; providers dl-agent does not have yet fail like [`Real`].
#[derive(Debug, Clone, Default)]
pub struct FakeDest(pub Arc<Mutex<DestState>>);

impl FakeDest {
    pub fn state(&self) -> std::sync::MutexGuard<'_, DestState> {
        self.0.lock().unwrap()
    }
}

impl Uploaders for FakeDest {
    fn open(&self, dest: &Destination) -> Result<Box<dyn Uploader>, Error> {
        if matches!(Kind::of(&dest.provider), Kind::Unknown) {
            return Real.open(dest);
        }
        if let Some(e) = &self.state().open_error {
            return Err(e.clone());
        }
        Ok(Box::new(FakeUploader {
            shared: self.0.clone(),
            provider: dest.provider.clone(),
        }))
    }
}

struct FakeUploader {
    shared: Arc<Mutex<DestState>>,
    provider: String,
}

impl Uploader for FakeUploader {
    fn upload(
        &self,
        file: &Path,
        _cancel: &CancelToken,
        progress: Progress<'_>,
    ) -> Result<String, Error> {
        let mut s = self.shared.lock().unwrap();
        if let Some(e) = &s.upload_error {
            return Err(e.clone());
        }
        let len = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
        progress(len, len);
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        s.uploaded.push(name.clone());
        Ok(format!("fake://{}/{name}", self.provider))
    }

    fn download(&self, cloud_path: &str, dest: &Path, _cancel: &CancelToken) -> Result<u64, Error> {
        let mut s = self.shared.lock().unwrap();
        if let Some(e) = &s.download_error {
            return Err(e.clone());
        }
        std::fs::write(dest, &s.content).unwrap();
        s.downloaded
            .push((cloud_path.to_owned(), dest.display().to_string()));
        Ok(s.content.len() as u64)
    }

    fn delete(&self, cloud_path: &str) -> Result<(), Error> {
        let mut s = self.shared.lock().unwrap();
        if let Some(e) = &s.delete_error {
            return Err(e.clone());
        }
        s.deleted.push(cloud_path.to_owned());
        Ok(())
    }
}
