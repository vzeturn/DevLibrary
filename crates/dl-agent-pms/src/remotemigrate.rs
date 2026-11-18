//! A cloud-requested MM_PMS migration (the `migrate` frame): pull the bundle over the
//! agent's authenticated HTTP route, verify it against this agent's own pinned keys (the
//! cloud is not trusted for that), check it is the version the cloud asked for, run it
//! and report progress and one result. At most one run at a time. Frames and logs carry
//! file names, versions, durations and codes only.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use dl_http::{Method, Request, Url};
use dl_json::Value;

use crate::attrs;
use crate::bundle::{self, Bundle, Keyring};
use crate::conn::{MigrationHandler, ProgressSink};
use crate::ctx::Ctx;
use crate::enroll::TokenProvider;
use crate::json;
use crate::log::Logger;
use crate::migrate::{self, DriftFinding, Event, MErr, Plan, Report};
use crate::pat;
use crate::wire::{self, Migrate, MigrateError, MigrateProgress, MigrateResult};

/// A downloaded, not yet verified bundle.
#[derive(Debug, Clone, Default)]
pub struct Raw {
    /// manifest.json bytes.
    pub manifest: Vec<u8>,
    /// manifest.sig bytes.
    pub signature: Vec<u8>,
    /// File name -> bytes.
    pub files: BTreeMap<String, Vec<u8>>,
}

/// Downloads a bundle by id.
pub trait Fetcher: Send + Sync {
    /// The raw bundle.
    fn fetch(&self, ctx: &Ctx, bundle_id: &str) -> Result<Raw, String>;
}

/// `GET <cloud>/agent/v1/bundles/<id>` with the agent's bearer token. The body is
/// untrusted until [`bundle::verify`] accepts it.
pub struct HttpFetcher {
    /// Tenant base URL.
    pub cloud_url: String,
    /// HTTP client.
    pub http: Arc<dl_http::Client>,
    /// Tokens.
    pub tokens: Arc<dyn TokenProvider>,
    /// Body cap.
    pub max_bytes: usize,
}

enum GetErr {
    Unauthorized,
    Other(String),
}

impl HttpFetcher {
    fn get(&self, ctx: &Ctx, id: &str) -> Result<Raw, GetErr> {
        let token = self
            .tokens
            .token(ctx)
            .map_err(|e| GetErr::Other(format!("get token: {e}")))?;
        let url = format!(
            "{}{}/{}",
            self.cloud_url,
            wire::AGENT_BUNDLE_PATH,
            dl_http::encode_path_segment(id)
        );
        let url = Url::parse(&url).map_err(|e| GetErr::Other(e.to_string()))?;
        let bad = |e: dl_http::Error| GetErr::Other(e.to_string());
        let mut req = Request::new(Method::Get, url)
            .with_header("Authorization", &format!("Bearer {token}"))
            .map_err(bad)?
            .with_header("Accept", "application/json")
            .map_err(bad)?;
        if let Some(left) = ctx.remaining() {
            req = req.with_timeout(left);
        } else {
            // Bundles can be large; give the download more than the default 30 s.
            req = req.with_timeout(std::time::Duration::from_secs(300));
        }
        let resp = self
            .http
            .request(&req)
            .map_err(|e| GetErr::Other(format!("fetch bundle: {e}")))?;
        match resp.status.as_u16() {
            200 => {}
            401 => return Err(GetErr::Unauthorized),
            s => return Err(GetErr::Other(format!("bundle route answered HTTP {s}"))),
        }
        if resp.body.len() > self.max_bytes {
            return Err(GetErr::Other(format!(
                "bundle larger than {} bytes",
                self.max_bytes
            )));
        }
        decode(&resp.body).map_err(GetErr::Other)
    }
}

impl Fetcher for HttpFetcher {
    /// A refused token is renewed once.
    fn fetch(&self, ctx: &Ctx, bundle_id: &str) -> Result<Raw, String> {
        if !pat::is_bundle_id(bundle_id) {
            return Err("bad bundle id".into());
        }
        match self.get(ctx, bundle_id) {
            Ok(r) => Ok(r),
            Err(GetErr::Unauthorized) => {
                self.tokens.invalidate();
                match self.get(ctx, bundle_id) {
                    Ok(r) => Ok(r),
                    Err(GetErr::Unauthorized) => Err("bundle route answered HTTP 401".into()),
                    Err(GetErr::Other(e)) => Err(e),
                }
            }
            Err(GetErr::Other(e)) => Err(e),
        }
    }
}

/// The route's JSON body: `{manifest: base64, signature: text, files: [{name, body}]}`.
pub fn decode(body: &[u8]) -> Result<Raw, String> {
    let shape = || "bundle body is not the expected JSON".to_owned();
    let o = json::parse_object(body).ok_or_else(shape)?;
    let s = |o: &dl_json::Map, k: &str| match json::field_ci(o, k) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(shape()),
    };
    let b64 = |s: &str| {
        let clean: String = s.chars().filter(|c| *c != '\r' && *c != '\n').collect();
        dl_encoding::base64::STANDARD.decode(&clean)
    };
    let manifest = b64(&s(&o, "manifest")?).map_err(|_| "manifest is not base64".to_owned())?;
    let mut out = Raw {
        manifest,
        signature: s(&o, "signature")?.into_bytes(),
        files: BTreeMap::new(),
    };
    let files = match json::field_ci(&o, "files") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Err(shape()),
    };
    for f in &files {
        let empty = dl_json::Map::new();
        let fo = match f {
            Value::Object(m) => m,
            Value::Null => &empty,
            _ => return Err(shape()),
        };
        let name = s(fo, "name")?;
        let data = b64(&s(fo, "body")?).map_err(|_| "a file body is not base64".to_owned())?;
        if out.files.insert(name, data).is_some() {
            return Err("duplicate file name".into());
        }
    }
    Ok(out)
}

/// What a runner offers (the real one is [`migrate::Runner`]).
pub trait Runner: Send {
    /// Plans without writing.
    fn dry_run(&self, ctx: &Ctx, b: &Bundle) -> (Plan, Result<(), MErr>);
    /// Applies.
    fn apply(&self, ctx: &Ctx, b: &Bundle) -> (Report, Result<(), MErr>);
    /// Reports drift.
    fn check_drift(&self, ctx: &Ctx, b: &Bundle) -> Result<Vec<DriftFinding>, MErr>;
}

impl Runner for migrate::Runner {
    fn dry_run(&self, ctx: &Ctx, b: &Bundle) -> (Plan, Result<(), MErr>) {
        migrate::Runner::dry_run(self, ctx, b)
    }
    fn apply(&self, ctx: &Ctx, b: &Bundle) -> (Report, Result<(), MErr>) {
        migrate::Runner::apply(self, ctx, b)
    }
    fn check_drift(&self, ctx: &Ctx, b: &Bundle) -> Result<Vec<DriftFinding>, MErr> {
        migrate::Runner::check_drift(self, ctx, b)
    }
}

/// Builds a runner for one run: the operator's re-run approval and the progress sink.
pub type RunnerFactory = Box<
    dyn Fn(i64, Box<dyn Fn(Event) + Send + Sync>) -> Result<Box<dyn Runner>, String> + Send + Sync,
>;

/// The agent's [`MigrationHandler`].
pub struct Handler {
    fetcher: Option<Box<dyn Fetcher>>,
    keys: Keyring,
    new_runner: Option<RunnerFactory>,
    log: Logger,
    busy: AtomicBool,
}

/// A result for a run that did not start.
pub fn refused(req: &Migrate, code: &str) -> MigrateResult {
    MigrateResult {
        id: req.id.clone(),
        status: wire::MIGRATE_REFUSED.into(),
        bundle_id: req.bundle_id.clone(),
        error: Some(MigrateError {
            code: code.into(),
            ..MigrateError::default()
        }),
        ..MigrateResult::default()
    }
}

/// Sets status and a code-only error (the message never leaves the agent).
fn with_error(mut res: MigrateResult, status: &str, err: &MErr) -> MigrateResult {
    res.status = status.into();
    res.error = Some(MigrateError {
        code: err.code.into(),
        seq: (err.seq > 0).then_some(err.seq),
        sql_number: (err.sql_number != 0).then_some(err.sql_number),
        line: (err.sql_number != 0).then_some(err.line),
    });
    res
}

fn progress_frame(id: &str, e: &Event) -> MigrateProgress {
    MigrateProgress {
        id: id.into(),
        phase: e.phase.clone(),
        seq: e.seq,
        name: (!e.name.is_empty()).then(|| e.name.clone()),
        duration_ms: e.duration_ms,
    }
}

impl Handler {
    /// A handler; without a fetcher, keys or runner factory every request is refused
    /// MIGRATE_UNSUPPORTED.
    pub fn new(
        fetcher: Option<Box<dyn Fetcher>>,
        keys: Keyring,
        new_runner: Option<RunnerFactory>,
        log: Logger,
    ) -> Handler {
        Handler {
            fetcher,
            keys,
            new_runner,
            log,
            busy: AtomicBool::new(false),
        }
    }

    fn run(&self, ctx: &Ctx, req: &Migrate, progress: &ProgressSink) -> MigrateResult {
        let (Some(fetcher), false, Some(new_runner)) =
            (&self.fetcher, self.keys.is_empty(), &self.new_runner)
        else {
            return refused(req, wire::MIGRATE_UNSUPPORTED);
        };
        let raw = match fetcher.fetch(ctx, &req.bundle_id) {
            Ok(r) => r,
            Err(e) => {
                self.log.warn(
                    "agent.migrate.fetch_failed",
                    attrs!["run_id" => &req.id, "bundle_id" => &req.bundle_id, "reason" => e],
                );
                return refused(req, wire::BUNDLE_FETCH);
            }
        };
        let b = match bundle::verify(&raw.manifest, &raw.signature, &raw.files, &self.keys) {
            Ok(b) => b,
            Err(e) => {
                self.log.warn(
                    "agent.migrate.unverified",
                    attrs!["run_id" => &req.id, "bundle_id" => &req.bundle_id, "code" => e.code],
                );
                return refused(req, &format!("BUNDLE_{}", e.code.to_uppercase()));
            }
        };
        if b.manifest.bundle_id != req.bundle_id || b.manifest.to_version != req.target_version {
            return refused(req, wire::TARGET_MISMATCH);
        }
        progress(MigrateProgress {
            id: req.id.clone(),
            phase: "verified".into(),
            seq: 0,
            name: None,
            duration_ms: 0,
        });
        let sink = progress.clone();
        let id = req.id.clone();
        let runner = match new_runner(
            req.rerun_partial,
            Box::new(move |e| sink(progress_frame(&id, &e))),
        ) {
            Ok(r) => r,
            Err(e) => {
                self.log.error(
                    "agent.migrate.runner",
                    attrs!["run_id" => &req.id, "reason" => e],
                );
                return refused(req, wire::MIGRATE_UNSUPPORTED);
            }
        };
        let mut res = MigrateResult {
            id: req.id.clone(),
            bundle_id: req.bundle_id.clone(),
            ..MigrateResult::default()
        };
        if req.check_drift {
            match runner.check_drift(ctx, &b) {
                Ok(d) => {
                    res.drift = d
                        .into_iter()
                        .take(500)
                        .map(|f| wire::DriftFinding {
                            kind: f.kind,
                            object: f.object,
                            code: f.code,
                        })
                        .collect();
                }
                Err(e) => return with_error(res, wire::MIGRATE_REFUSED, &e),
            }
        }
        if req.dry_run {
            let (plan, r) = runner.dry_run(ctx, &b);
            let err = r.err();
            res.head = plan.head;
            res.adopted = plan.adopt as i64;
            res.pending = plan.pending.len() as i64;
            if let Some(e) = err {
                return with_error(res, wire::MIGRATE_REFUSED, &e);
            }
            if !plan.backup_refusal.is_empty() {
                // Apply would refuse: say so now instead of reporting a clean plan.
                return with_error(
                    res,
                    wire::MIGRATE_REFUSED,
                    &MErr::new(migrate::CODE_BACKUP_REQUIRED, ""),
                );
            }
            if !plan.missing_permissions.is_empty() {
                res.status = wire::MIGRATE_REFUSED.into();
                res.error = Some(MigrateError {
                    code: "PERMISSION_DENIED".into(),
                    ..MigrateError::default()
                });
                return res;
            }
            res.status = wire::MIGRATE_DRY_RUN.into();
            return res;
        }
        let (rep, r) = runner.apply(ctx, &b);
        res.head = rep.head;
        res.adopted = rep.adopted as i64;
        res.applied = rep.applied.len() as i64;
        if let Err(e) = r {
            res.pending = (b.manifest.to_version - rep.head.max(b.manifest.from_version)).max(0);
            let status = if e.code == migrate::CODE_MIGRATION_FAILED || !rep.applied.is_empty() {
                wire::MIGRATE_FAILED
            } else {
                wire::MIGRATE_REFUSED
            };
            return with_error(res, status, &e);
        }
        res.status = wire::MIGRATE_APPLIED.into();
        res
    }
}

impl MigrationHandler for Handler {
    fn handle(&self, ctx: &Ctx, req: &Migrate, progress: ProgressSink) -> MigrateResult {
        let progress = &progress;
        if self
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            self.log.warn(
                "agent.migrate.busy",
                attrs!["run_id" => &req.id, "bundle_id" => &req.bundle_id],
            );
            return refused(req, wire::MIGRATE_BUSY);
        }
        let started = Instant::now();
        let mut res = self.run(ctx, req, progress);
        self.busy.store(false, Ordering::SeqCst);
        res.duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let code = res.error.as_ref().map_or(String::new(), |e| e.code.clone());
        self.log.info(
            "agent.migrate.done",
            attrs![
                "run_id" => &req.id,
                "bundle_id" => &req.bundle_id,
                "dry_run" => req.dry_run,
                "status" => &res.status,
                "code" => code,
                "head" => res.head,
                "applied" => res.applied,
                "drift" => res.drift.len(),
                "ms" => res.duration_ms
            ],
        );
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_route_body() {
        let body = br#"{"manifest":"e30=","signature":"{\"alg\":\"ed25519\"}","files":[{"name":"001_a.sql","body":"U0VMRUNUIDE="}]}"#;
        let r = decode(body).unwrap();
        assert_eq!(r.manifest, b"{}");
        assert_eq!(r.files["001_a.sql"], b"SELECT 1");
        assert!(decode(br#"{"manifest":"!!"}"#).is_err());
        assert!(
            decode(br#"{"manifest":"","files":[{"name":"a","body":""},{"name":"a","body":""}]}"#)
                .is_err()
        );
        assert!(decode(b"[]").is_err());
    }

    #[test]
    fn errors_carry_codes_only() {
        let req = Migrate {
            id: "x".into(),
            bundle_id: "b".into(),
            target_version: 1,
            dry_run: false,
            rerun_partial: 0,
            check_drift: false,
        };
        let mut e = MErr::new(migrate::CODE_MIGRATION_FAILED, "secret text");
        e.seq = 4;
        e.sql_number = 2627;
        e.line = 7;
        let r = with_error(refused(&req, "X"), wire::MIGRATE_FAILED, &e);
        assert_eq!(
            r.error,
            Some(MigrateError {
                code: migrate::CODE_MIGRATION_FAILED.into(),
                seq: Some(4),
                sql_number: Some(2627),
                line: Some(7),
            })
        );
    }
}
