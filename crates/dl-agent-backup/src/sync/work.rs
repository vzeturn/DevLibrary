//! The parts of a poll besides schedules: Go `runRestores`, `runCommands`, `runDiscovery`
//! and `runRetention`.

use std::time::Instant;

use dl_json::{Value, json};

use super::{HUB_USERNAME, SyncLoop, deliver};
use crate::Error;
use crate::backup::Steps;
use crate::backup::restore::{RestoreParams, Source, restore_from_record};
use crate::core::lock;
use crate::discovery::{filter_system, list_databases};
use crate::hub::types::{CommandConfig, ConfigResponse, RestoreRequestConfig};
use crate::hub::{command_report, restore_report};
use crate::log;
use crate::report::scrub;
use crate::retention::select_expired;
use crate::sql::Target;

impl SyncLoop {
    /// Runs each restore request once and reports it.
    pub(super) fn restores(&mut self, cfg: &ConfigResponse) -> Result<(), Error> {
        for req in &cfg.restore_requests {
            let key = format!("restore:{}", req.id);
            if self.handled.contains(&req.id) || self.core.in_outbox(&key) {
                continue;
            }
            let res = self.restore(cfg, req);
            match &res {
                Err(Error::Cancelled) => return Ok(()),
                Err(e @ Error::Auth(_)) => return Err(e.clone()),
                Err(e) => {
                    self.core.reports.error(&req.id, e);
                    log::warn(
                        "restore failed",
                        &[("request", &req.id), ("error", &e.to_string())],
                    );
                }
                Ok(()) => log::info("restore complete", &[("request", &req.id)]),
            }
            self.handled.insert(req.id.clone());
            let error = res.err().map(|e| scrub(&e.to_string())).unwrap_or_default();
            deliver(
                &self.core,
                &key,
                restore_report(&req.id, error.is_empty(), &error),
            );
        }
        Ok(())
    }

    /// Go `runOneRestore`.
    fn restore(&self, cfg: &ConfigResponse, req: &RestoreRequestConfig) -> Result<(), Error> {
        let core = &self.core;
        let record = lock(&core.history)
            .find(&req.backup_history_id)
            .ok_or_else(|| {
                Error::Io(format!(
                    "no local record of backup history {}",
                    req.backup_history_id
                ))
            })?;
        let sched = cfg
            .schedules
            .iter()
            .find(|s| s.id == record.schedule_id)
            .ok_or_else(|| {
                Error::Schedule(format!(
                    "schedule {} that produced backup {} no longer exists",
                    record.schedule_id, req.backup_history_id
                ))
            })?;
        let destination = self.destination(sched)?;
        let (port, auth_type) = cfg
            .servers
            .iter()
            .find(|s| s.host == req.target_server_host)
            .map_or((1433, "sql".to_owned()), |s| (s.port, s.auth_type.clone()));
        let password = core.open_credential(
            &req.target_password_encrypted,
            req.credential_error,
            "password of the restore target server",
        )?;
        let params = RestoreParams {
            target: Target {
                host: req.target_server_host.clone(),
                port,
                auth_type,
                username: HUB_USERNAME.into(),
                password,
            },
            target_database: req.target_database.clone(),
            with_replace: true,
            backup_file: String::new(),
        };
        let src = Source {
            local_file_path: record.local_file_path,
            cloud_file_path: record.cloud_file_path,
            tag: req.id.clone(),
            destination,
        };
        let mut steps = Steps::new();
        restore_from_record(
            core.sql.as_ref(),
            core.uploaders.as_ref(),
            &src,
            params,
            &core.settings.backup_dir,
            &core.cancel,
            &mut steps,
        )
    }

    /// Runs each hub command once and reports it.
    pub(super) fn commands(&mut self, cfg: &ConfigResponse) -> Result<(), Error> {
        for cmd in &cfg.commands {
            let key = format!("command:{}", cmd.id);
            if self.handled.contains(&cmd.id) || self.core.in_outbox(&key) {
                continue;
            }
            let (ok, result) = match self.command(cmd) {
                Ok(v) => (true, v),
                Err(Error::Cancelled) => return Ok(()),
                Err(e @ Error::Auth(_)) => return Err(e),
                Err(e) => {
                    if matches!(e, Error::Credential(_)) {
                        self.core.reports.error(&cmd.id, &e);
                    }
                    (false, json!({"error": scrub(&e.to_string())}))
                }
            };
            self.handled.insert(cmd.id.clone());
            deliver(&self.core, &key, command_report(&cmd.id, ok, result));
        }
        Ok(())
    }

    /// Go `runOneCommand`.
    fn command(&self, cmd: &CommandConfig) -> Result<Value, Error> {
        let kind = cmd.kind.as_str();
        if kind != "TestConnection" && kind != "DiscoverDatabases" {
            return Err(Error::Schedule(format!("unknown command type {kind:?}")));
        }
        let what = format!("password of server {}", cmd.host);
        let password =
            self.core
                .open_credential(&cmd.encrypted_password, cmd.credential_error, &what)?;
        let target = Target {
            host: cmd.host.clone(),
            port: cmd.port,
            auth_type: cmd.auth_type.clone(),
            username: HUB_USERNAME.into(),
            password,
        };
        let t0 = Instant::now();
        let names = list_databases(self.core.sql.as_ref(), &target)?;
        if kind == "TestConnection" {
            let ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
            return Ok(json!({"latencyMs": ms}));
        }
        let dbs: Vec<Value> = filter_system(names).into_iter().map(Value::from).collect();
        Ok(json!({"databases": dbs}))
    }

    /// Go `runDiscovery`: each server's user databases, sent to the hub.
    pub(super) fn discovery(&self, cfg: &ConfigResponse) -> Result<(), Error> {
        let core = &self.core;
        for server in &cfg.servers {
            let what = format!("password of server {}", server.host);
            let res = core
                .open_credential(&server.encrypted_password, server.credential_error, &what)
                .and_then(|password| {
                    let target = Target {
                        host: server.host.clone(),
                        port: server.port,
                        auth_type: server.auth_type.clone(),
                        username: HUB_USERNAME.into(),
                        password,
                    };
                    list_databases(core.sql.as_ref(), &target)
                })
                .and_then(|names| core.hub.discover(&server.id, &filter_system(names)));
            match res {
                Ok(()) => {}
                Err(e @ Error::Auth(_)) => return Err(e),
                Err(Error::Cancelled) => return Ok(()),
                Err(e) => {
                    core.reports.error(&server.id, &e);
                }
            }
        }
        Ok(())
    }

    /// Go `runRetention`: removes the backups past a schedule's age or count, at the
    /// destination and on disk, then forgets them.
    pub(super) fn retention(&self, cfg: &ConfigResponse, now_ms: u64) -> Result<(), Error> {
        let core = &self.core;
        for sched in &cfg.schedules {
            if sched.retain_days <= 0 && sched.retain_count <= 0 {
                continue;
            }
            let records = lock(&core.history).for_schedule(&sched.id);
            let expired = select_expired(&records, sched.retain_days, sched.retain_count, now_ms);
            if expired.is_empty() {
                continue;
            }
            let destination = if expired.iter().any(|r| !r.cloud_file_path.is_empty()) {
                match self.destination(sched) {
                    Ok(d) => d,
                    Err(e @ Error::Auth(_)) => return Err(e),
                    Err(e) => {
                        core.reports.error(&sched.id, &e);
                        continue;
                    }
                }
            } else {
                None
            };
            for rec in expired {
                if !rec.cloud_file_path.is_empty() {
                    let deleted = match destination.as_ref() {
                        Some(d) => core
                            .uploaders
                            .open(d)
                            .and_then(|u| u.delete(&rec.cloud_file_path)),
                        None => Err(Error::Upload(format!(
                            "cannot delete expired backup {}: the schedule has no destination credentials",
                            rec.cloud_file_path
                        ))),
                    };
                    if let Err(e) = deleted {
                        // Kept, so the next poll tries again (Go).
                        core.reports.error(&sched.id, &e);
                        continue;
                    }
                }
                if !rec.local_file_path.is_empty() {
                    match std::fs::remove_file(&rec.local_file_path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            let err = Error::io(
                                &format!("remove expired backup {}", rec.local_file_path),
                                &e,
                            );
                            core.reports.error(&sched.id, &err);
                        }
                    }
                }
                if let Err(e) = lock(&core.history).forget(&rec.hub_history_id) {
                    core.reports.error(&sched.id, &e);
                }
            }
        }
        Ok(())
    }
}
