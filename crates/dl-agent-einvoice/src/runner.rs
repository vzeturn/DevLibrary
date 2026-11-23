//! One collect / push / write-back cycle: a port of `runner.go`. The loop around it lives
//! in [`crate::module`], which runs it on the module's collector thread.

use std::time::{Duration, Instant};

use crate::agent_config::AgentConfig;
use crate::log::{self, V};
use crate::outbox::Outbox;
use crate::platform::{ApiError, Heartbeat, Platform};
use crate::sql::{CollectKey, Sql, WritebackApply};

/// Errors of one cycle, and whether the platform said this install was replaced.
#[derive(Debug, Default, Clone)]
pub struct Errors {
    /// Messages, in the order they happened.
    pub list: Vec<String>,
    /// The platform answered `device_replaced`.
    pub replaced: bool,
}

impl Errors {
    fn api(&mut self, e: ApiError) {
        if e == ApiError::DeviceReplaced {
            self.replaced = true;
        }
        self.list.push(e.to_string());
    }
}

/// `CycleResult`, flattened, plus the failures sorted by kind for problem reports (the
/// Go fields keep their meaning, so the heartbeat's `last_error` is unchanged).
#[derive(Debug, Default, Clone)]
pub struct CycleResult {
    /// Invoices read from SQL.
    pub read: i64,
    /// Invoices added to the outbox.
    pub enqueued: i64,
    /// Invoices the platform accepted.
    pub pushed: i64,
    /// Invoices the platform answered with an error.
    pub errored: i64,
    /// Items left queued after a failed push.
    pub remaining: i64,
    /// Write-back commands received.
    pub polled: i64,
    /// Write-back commands applied.
    pub applied: i64,
    /// Write-back commands already applied before.
    pub skipped: i64,
    /// Write-back commands acknowledged.
    pub acked: i64,
    /// `ReadErrors`.
    pub read_errors: Errors,
    /// `PushErrors`.
    pub push_errors: Errors,
    /// `WritebackErrors`.
    pub writeback_errors: Errors,
    /// SQL failures: select, drift check, trn-types, probe, mark collected, write-back.
    pub sql_errors: Vec<String>,
    /// Bills that could not be fetched or turned into a push item.
    pub item_errors: Vec<String>,
    /// Local outbox failures: enqueue, remove, mark applied.
    pub outbox_errors: Vec<String>,
    /// The platform push failure that kept the batch queued.
    pub push_failed: Option<String>,
    /// The write-back poll or ack failure.
    pub poll_failed: Option<String>,
    /// Time spent reading SQL (select + build).
    pub select_ms: i64,
    /// Time spent pushing to the hub.
    pub push_ms: i64,
    /// Time spent polling and applying write-back commands.
    pub writeback_ms: i64,
}

impl CycleResult {
    /// `Replaced`.
    pub fn replaced(&self) -> bool {
        self.read_errors.replaced || self.push_errors.replaced || self.writeback_errors.replaced
    }

    /// `firstErrorMsg`.
    pub fn first_error(&self) -> Option<&str> {
        [&self.read_errors, &self.push_errors, &self.writeback_errors]
            .into_iter()
            .find_map(|e| e.list.first().map(String::as_str))
    }

    /// Adds `w`'s write-back outcome (counts, errors, time) to this result.
    fn absorb_writeback(&mut self, w: CycleResult) {
        self.polled += w.polled;
        self.applied += w.applied;
        self.skipped += w.skipped;
        self.acked += w.acked;
        self.writeback_errors.list.extend(w.writeback_errors.list);
        self.writeback_errors.replaced |= w.writeback_errors.replaced;
        self.sql_errors.extend(w.sql_errors);
        self.outbox_errors.extend(w.outbox_errors);
        if w.poll_failed.is_some() {
            self.poll_failed = w.poll_failed;
        }
        self.writeback_ms += w.writeback_ms;
    }
}

/// `sourcesFor`.
pub fn sources_for(collect_sources: &str) -> Vec<&'static str> {
    match collect_sources {
        "FO" => vec!["FO"],
        "POS" => vec!["POS"],
        "" | "FO,POS" => vec!["FO", "POS"],
        other => {
            log::warn(
                "read: unknown collect_sources value, collecting both",
                &[("collect_sources", V::S(other))],
            );
            vec!["FO", "POS"]
        }
    }
}

/// The machine name sent with heartbeats.
pub fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_owned())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".into())
}

/// One collected item ready for the outbox.
struct Found {
    source_ref: String,
    item_json: String,
    bill_id: String,
    bill_type: String,
}

/// The agent runner.
pub struct Runner {
    /// The SMILE_FO / SMILE_POS connections.
    pub sql: Sql,
    /// The platform client.
    pub cli: Platform,
    /// The local queue.
    pub outbox: Outbox,
    /// The sealed config, refreshed from the hub every cycle.
    pub cfg: AgentConfig,
    device_name: String,
    config_stale_streak: u32,
    /// Whether every collected database has the agent's write-back procedure (checked
    /// until it succeeds once per run).
    writeback_ensured: bool,
    /// Write-back runs on its own worker thread (RT-0785): the cycle only reports it.
    writeback_worker: bool,
    /// What the write-back worker did since the last cycle.
    writeback_pending: CycleResult,
}

const CONFIG_STALE_ESCALATE_AFTER: u32 = 5;

impl Runner {
    /// `NewRunner`.
    pub fn new(sql: Sql, cli: Platform, outbox: Outbox, cfg: AgentConfig) -> Runner {
        Runner {
            sql,
            cli,
            outbox,
            cfg,
            device_name: device_name(),
            config_stale_streak: 0,
            writeback_ensured: false,
            writeback_worker: false,
            writeback_pending: CycleResult::default(),
        }
    }

    /// `FetchCollected` for one key: SQL rows → collected invoice.
    pub fn fetch_collected(
        &self,
        key: &CollectKey,
        timeout: Duration,
    ) -> Result<crate::domain::CollectedInvoice, String> {
        crate::fetch::collected(&self.sql, &self.cfg.rules(), key, timeout)
    }

    /// Collects one source; returns the items and the SQL and item failures.
    fn read_source(&self, source: &str, deadline: Instant) -> (Vec<Found>, ReadFailures) {
        let mut failed = ReadFailures::default();
        let timeout = deadline.saturating_duration_since(Instant::now());
        let keys = match self.sql.select_pending(
            source,
            self.cfg.batch_size,
            self.cfg.collect_from,
            timeout,
        ) {
            Ok(p) => {
                if let Some(e) = p.edit_error {
                    // Edit detection failed; the never-sent keys are still valid.
                    log::error(
                        "read: select pending collect failed",
                        &[("source", V::S(source)), ("error", V::S(&e))],
                    );
                    failed.sql.push(e);
                }
                p.keys
            }
            Err(e) => {
                log::error(
                    "read: select pending collect failed",
                    &[("source", V::S(source)), ("error", V::S(&e))],
                );
                failed.sql.push(e);
                return (Vec::new(), failed);
            }
        };
        let mut out = Vec::new();
        for ck in keys {
            let timeout = deadline.saturating_duration_since(Instant::now());
            if timeout.is_zero() {
                break;
            }
            let inv = match self.fetch_collected(&ck, timeout) {
                Ok(inv) => inv,
                Err(e) => {
                    log::error(
                        "read: fetch collected failed",
                        &[("key", V::S(&ck.source_ref())), ("error", V::S(&e))],
                    );
                    failed.items.push(format!("{}: {e}", ck.source_ref()));
                    continue;
                }
            };
            match crate::push_item(inv) {
                Ok(item) => out.push(Found {
                    source_ref: item.source_ref.clone(),
                    item_json: item.to_json(),
                    bill_id: ck.id.clone(),
                    bill_type: ck.source.clone(),
                }),
                Err(e) => {
                    log::error(
                        "read: canonicalize collected failed",
                        &[("key", V::S(&ck.source_ref())), ("error", V::S(&e))],
                    );
                    failed.items.push(format!("{}: {e}", ck.source_ref()));
                }
            }
        }
        (out, failed)
    }

    /// Installs the write-back procedure where a site enrolled before it was embedded lacks
    /// it. Returns whether every source was checked.
    fn ensure_writeback(&self, sources: &[&str], timeout: Duration, res: &mut CycleResult) -> bool {
        let mut all = true;
        for source in sources {
            match self.sql.ensure_writeback(source, timeout) {
                Ok(true) => log::info(
                    "read: installed the missing write-back procedure",
                    &[("source", V::S(source))],
                ),
                Ok(false) => {}
                Err(e) => {
                    all = false;
                    log::error(
                        "read: write-back procedure check failed",
                        &[("source", V::S(source)), ("error", V::S(&e))],
                    );
                    res.sql_errors.push(e);
                }
            }
        }
        all
    }

    /// `ReadCycle`.
    pub fn read_cycle(&mut self, deadline: Instant, res: &mut CycleResult) {
        let sources = sources_for(&self.cfg.collect_sources);
        let timeout = deadline.saturating_duration_since(Instant::now());
        if !self.writeback_ensured {
            self.writeback_ensured = self.ensure_writeback(&sources, timeout, res);
        }
        let mut fo_ok = sources.contains(&"FO");
        if fo_ok
            && let Err(e) = self
                .sql
                .verify_fo_charge_code_sync(&self.cfg.charge_codes(), timeout)
        {
            fo_ok = false;
            log::error(
                "read: FO charge-code drift check failed, skipping FO collection",
                &[("error", V::S(&e))],
            );
            res.sql_errors.push(e);
        }
        if fo_ok {
            match self.sql.list_trn_types(timeout) {
                Err(e) => {
                    log::error("read: list trn-types failed", &[("error", V::S(&e))]);
                    res.sql_errors.push(e);
                }
                Ok(types) if !types.is_empty() => {
                    if let Err(e) = self.cli.publish_trn_types(&types, deadline) {
                        log::error(
                            "read: publish trn-types failed",
                            &[("error", V::S(&e.to_string()))],
                        );
                    }
                }
                Ok(_) => {}
            }
            match self.sql.probe_fo_schema(timeout) {
                Err(e) => {
                    log::error("read: FO schema probe failed", &[("error", V::S(&e))]);
                    res.sql_errors.push(e);
                }
                Ok(p) => log::info(
                    "FO schema probe",
                    &[
                        ("transaction_group", V::B(p.has_transaction_group)),
                        ("folio_balance_code", V::B(p.has_folio_balance_code)),
                        ("is_hk_post", V::B(p.has_is_hk_post)),
                        ("tax_def", V::B(p.has_tax_def)),
                        ("tax_def_rate", V::B(p.has_tax_def_rate)),
                        ("folio_balance_codes", V::S(&p.distinct_folio_balance_codes)),
                        ("group1_count", V::I(p.group1_count)),
                    ],
                ),
            }
        }
        let wanted: Vec<&str> = sources
            .into_iter()
            .filter(|s| *s != "FO" || fo_ok)
            .collect();
        // FO and POS are read in parallel, as the Go agent does with an errgroup; each
        // source has its own connection.
        let this = &*self;
        let per_source: Vec<(Vec<Found>, ReadFailures)> = std::thread::scope(|s| {
            let handles: Vec<_> = wanted
                .iter()
                .map(|src| (src, s.spawn(move || this.read_source(src, deadline))))
                .collect();
            handles
                .into_iter()
                .map(|(src, h)| {
                    h.join().unwrap_or_else(|_| {
                        let msg = format!("read: {src} reader panicked");
                        log::error(&msg, &[]);
                        let mut f = ReadFailures::default();
                        f.items.push(msg);
                        (Vec::new(), f)
                    })
                })
                .collect()
        });
        let mut found = Vec::new();
        for (items, failed) in per_source {
            found.extend(items);
            res.sql_errors.extend(failed.sql);
            res.item_errors.extend(failed.items);
        }
        let mut seen: Vec<String> = Vec::new();
        for f in found {
            res.read += 1;
            if seen.contains(&f.source_ref) {
                continue;
            }
            seen.push(f.source_ref.clone());
            match self
                .outbox
                .enqueue(&f.source_ref, f.item_json, &f.bill_id, &f.bill_type)
            {
                Ok(()) => res.enqueued += 1,
                Err(e) => {
                    log::error(
                        "read: enqueue failed",
                        &[("source_ref", V::S(&f.source_ref)), ("error", V::S(&e))],
                    );
                    res.outbox_errors.push(e.clone());
                    res.read_errors.list.push(e);
                }
            }
        }
    }

    /// `PushCycle`.
    pub fn push_cycle(&mut self, deadline: Instant, res: &mut CycleResult) {
        let entries = self.outbox.drain(self.cfg.batch_size);
        if entries.is_empty() {
            return;
        }
        let items: Vec<String> = entries.iter().map(|e| e.item_json.clone()).collect();
        let results = match self.cli.push_raw(&items, deadline) {
            Ok(r) => r,
            Err(e) => {
                log::error(
                    "push: platform push failed, keeping items queued",
                    &[
                        ("count", V::I(entries.len() as i64)),
                        ("error", V::S(&e.to_string())),
                    ],
                );
                res.remaining = entries.len() as i64;
                if e != ApiError::DeviceReplaced {
                    res.push_failed = Some(e.to_string());
                }
                res.push_errors.api(e);
                return;
            }
        };
        for pr in results {
            let Some(entry) = entries.iter().find(|e| e.source_ref == pr.source_ref) else {
                continue;
            };
            let key = CollectKey {
                source: entry.bill_type.clone(),
                id: entry.bill_id.clone(),
            };
            let timeout = deadline.saturating_duration_since(Instant::now());
            let failed = pr.outcome == "error";
            let mark = if failed {
                self.sql.mark_collected(&key, false, &pr.message, timeout)
            } else {
                self.sql.mark_collected(&key, true, "", timeout)
            };
            if let Err(e) = mark {
                let msg = if failed {
                    "push: mark processed(false) failed"
                } else {
                    "push: mark processed(true) failed"
                };
                log::error(
                    msg,
                    &[("source_ref", V::S(&pr.source_ref)), ("error", V::S(&e))],
                );
                res.sql_errors.push(e.clone());
                res.push_errors.list.push(e);
                continue;
            }
            if failed {
                res.errored += 1;
            } else {
                res.pushed += 1;
            }
            if let Err(e) = self.outbox.remove(&pr.source_ref) {
                log::error(
                    "push: remove from outbox failed",
                    &[("source_ref", V::S(&pr.source_ref)), ("error", V::S(&e))],
                );
                res.outbox_errors.push(e.clone());
                res.push_errors.list.push(e);
            }
        }
    }

    /// `WritebackCycle`.
    pub fn writeback_cycle(&mut self, deadline: Instant, res: &mut CycleResult) {
        let cmds = match self.cli.poll(deadline) {
            Ok(c) => c,
            Err(e) => {
                if e != ApiError::DeviceReplaced {
                    res.poll_failed = Some(e.to_string());
                }
                res.writeback_errors.api(e);
                return;
            }
        };
        self.apply_commands(cmds, deadline, res);
    }

    /// Applies polled write-back commands to SQL Server and acks the applied ones.
    fn apply_commands(
        &mut self,
        cmds: Vec<crate::platform::WritebackCommand>,
        deadline: Instant,
        res: &mut CycleResult,
    ) {
        res.polled += cmds.len() as i64;
        let mut to_ack = Vec::new();
        for cmd in cmds {
            let id = cmd.id.to_string();
            if self.outbox.is_applied(&id) {
                res.skipped += 1;
                to_ack.push(cmd.id);
                continue;
            }
            let apply = match writeback_apply_from(&cmd) {
                Ok(a) => a,
                Err(e) => {
                    log::error(
                        "writeback: map command failed",
                        &[("command_id", V::S(&id)), ("error", V::S(&e))],
                    );
                    res.writeback_errors.list.push(e);
                    continue;
                }
            };
            let timeout = deadline.saturating_duration_since(Instant::now());
            if let Err(e) = self.sql.apply_writeback(&apply, timeout) {
                log::error(
                    "writeback: apply failed",
                    &[("command_id", V::S(&id)), ("error", V::S(&e))],
                );
                res.sql_errors.push(e.clone());
                res.writeback_errors.list.push(e);
                continue;
            }
            if let Err(e) = self.outbox.mark_applied(&id) {
                log::error(
                    "writeback: mark applied failed",
                    &[("command_id", V::S(&id)), ("error", V::S(&e))],
                );
                res.outbox_errors.push(e.clone());
                res.writeback_errors.list.push(e);
                continue;
            }
            res.applied += 1;
            to_ack.push(cmd.id);
        }
        if !to_ack.is_empty() {
            match self.cli.ack(&to_ack, deadline) {
                Ok(()) => res.acked += to_ack.len() as i64,
                Err(e) => {
                    if e != ApiError::DeviceReplaced {
                        res.poll_failed = Some(e.to_string());
                    }
                    res.writeback_errors.api(e);
                }
            }
        }
    }

    /// `RunOnce`: each cycle under its own `cycle_timeout`.
    pub fn run_once(&mut self) -> CycleResult {
        let t = self.cfg.cycle_timeout;
        let mut res = CycleResult::default();
        let started = Instant::now();
        self.read_cycle(started + t, &mut res);
        res.select_ms = ms_since(started);
        let started = Instant::now();
        self.push_cycle(started + t, &mut res);
        res.push_ms = ms_since(started);
        if self.writeback_worker {
            res.absorb_writeback(std::mem::take(&mut self.writeback_pending));
        } else {
            let started = Instant::now();
            self.writeback_cycle(started + t, &mut res);
            res.writeback_ms = ms_since(started);
        }
        res
    }

    /// Hands write-back to a dedicated worker (RT-0785): from now on the cycle reports
    /// what the worker did instead of polling itself.
    pub fn hand_writeback_to_worker(&mut self) {
        self.writeback_worker = true;
    }

    /// Applies commands the write-back worker polled, under the cycle timeout. The
    /// outcome is returned and also kept for the next cycle's log, heartbeat and problems.
    pub fn apply_polled_writeback(
        &mut self,
        cmds: Vec<crate::platform::WritebackCommand>,
    ) -> CycleResult {
        let started = Instant::now();
        let mut res = CycleResult::default();
        self.apply_commands(cmds, started + self.cfg.cycle_timeout, &mut res);
        res.writeback_ms = ms_since(started);
        self.writeback_pending.absorb_writeback(res.clone());
        res
    }

    /// What the write-back worker did since the last cycle (tests).
    #[cfg(test)]
    pub fn take_writeback_pending(&mut self) -> CycleResult {
        std::mem::take(&mut self.writeback_pending)
    }

    /// Records a failed write-back poll of the worker for the next cycle's report.
    pub fn note_writeback_poll_error(&mut self, e: ApiError) {
        let w = &mut self.writeback_pending;
        if e != ApiError::DeviceReplaced {
            w.poll_failed = Some(e.to_string());
        }
        w.writeback_errors.api(e);
    }

    /// `refreshHubConfig`.
    pub fn refresh_hub_config(&mut self) {
        match self.cli.config(Instant::now() + Duration::from_secs(10)) {
            Err(e) => {
                self.config_stale_streak += 1;
                let err = e.to_string();
                if self.config_stale_streak >= CONFIG_STALE_ESCALATE_AFTER {
                    log::error(
                        "config refresh failing for many cycles; running on stale collect-from/sources/recalc",
                        &[
                            (
                                "consecutive_failures",
                                V::I(i64::from(self.config_stale_streak)),
                            ),
                            ("error", V::S(&err)),
                        ],
                    );
                } else {
                    log::warn(
                        "config refresh failed; keeping current collect-from/sources",
                        &[("error", V::S(&err))],
                    );
                }
            }
            Ok(remote) => {
                self.config_stale_streak = 0;
                let same = match (self.cfg.collect_from, remote.collect_from) {
                    (Some(a), Some(b)) => a.unix_nanos() == b.unix_nanos(),
                    (None, None) => true,
                    _ => false,
                };
                if !same {
                    let f = |t: Option<dl_time::OffsetDateTime>| {
                        t.map(|t| t.to_rfc3339()).unwrap_or_else(|| "unset".into())
                    };
                    log::info(
                        "collect-from updated from Hub",
                        &[
                            ("old", V::S(&f(self.cfg.collect_from))),
                            ("new", V::S(&f(remote.collect_from))),
                        ],
                    );
                    self.cfg.collect_from = remote.collect_from;
                }
                if self.cfg.collect_sources != remote.collect_sources {
                    log::info(
                        "collect-sources updated from Hub",
                        &[
                            ("old", V::S(&self.cfg.collect_sources)),
                            ("new", V::S(&remote.collect_sources)),
                        ],
                    );
                    self.cfg.collect_sources = remote.collect_sources;
                }
                if self.cfg.recalc_tax_into_sc != remote.recalc_tax_into_sc {
                    log::info(
                        "recalc-tax-into-sc updated from Hub",
                        &[
                            ("old", V::B(self.cfg.recalc_tax_into_sc)),
                            ("new", V::B(remote.recalc_tax_into_sc)),
                        ],
                    );
                    self.cfg.recalc_tax_into_sc = remote.recalc_tax_into_sc;
                }
                if self.sql.pos_item_name() != remote.collect_profile.pos_item_name {
                    log::info(
                        "collect-profile pos_item_name updated from Hub",
                        &[(
                            "new",
                            V::S(&format!("{:?}", remote.collect_profile.pos_item_name)),
                        )],
                    );
                    self.sql
                        .set_pos_item_name(remote.collect_profile.pos_item_name);
                }
            }
        }
    }

    fn send_heartbeat(&self, res: &CycleResult) {
        let hb = Heartbeat {
            agent_version: heartbeat_version(),
            device_name: self.device_name.clone(),
            read: res.read,
            pushed: res.pushed,
            errored: res.errored,
            last_error: res.first_error().unwrap_or_default().to_owned(),
            logs: log::drain(),
        };
        if let Err(e) = self.cli.heartbeat(&hb) {
            log::warn("heartbeat failed", &[("error", V::S(&e.to_string()))]);
        }
    }

    /// Consecutive failed hub-config refreshes.
    pub fn config_stale_streak(&self) -> u32 {
        self.config_stale_streak
    }

    /// `runCycle`: refresh the hub config, run one cycle, log it and send the heartbeat.
    /// The caller stops for good when [`CycleResult::replaced`] is true.
    pub fn run_cycle(&mut self) -> CycleResult {
        self.refresh_hub_config();
        let res = self.run_once();
        log::info(
            "cycle complete",
            &[
                ("read", V::I(res.read)),
                ("enqueued", V::I(res.enqueued)),
                ("pushed", V::I(res.pushed)),
                ("errored", V::I(res.errored)),
                ("remaining", V::I(res.remaining)),
                ("polled", V::I(res.polled)),
                ("applied", V::I(res.applied)),
                ("acked", V::I(res.acked)),
                ("select_ms", V::I(res.select_ms)),
                ("push_ms", V::I(res.push_ms)),
                ("writeback_ms", V::I(res.writeback_ms)),
            ],
        );
        self.send_heartbeat(&res);
        if res.replaced() {
            log::error(
                "agent replaced by a deployment on another machine; stopping. To run the agent HERE instead, re-run the deploy command from the tenant's Agent tab on this machine.",
                &[("error", V::S(&ApiError::DeviceReplaced.to_string()))],
            );
        }
        res
    }
}

fn ms_since(t: Instant) -> i64 {
    i64::try_from(t.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// The `agent_version` the platform sees: the host binary, not this crate.
pub fn heartbeat_version() -> String {
    format!("dl-agent-{}", dl_agent::agent_version())
}

/// What one source reader could not do.
#[derive(Default)]
struct ReadFailures {
    sql: Vec<String>,
    items: Vec<String>,
}

/// `writebackApplyFrom`.
pub fn writeback_apply_from(
    cmd: &crate::platform::WritebackCommand,
) -> Result<WritebackApply, String> {
    let Some((bill_type, bill_id)) = cmd.source_ref.split_once(':') else {
        return Err(format!("agent: malformed source_ref {:?}", cmd.source_ref));
    };
    let mut a = WritebackApply {
        bill_id: bill_id.to_owned(),
        bill_type: bill_type.to_owned(),
        issued_at: cmd.issued_at,
        ..Default::default()
    };
    match cmd.kind.as_str() {
        "SET_INVOICE_NUMBER" => {
            a.invoice_number = cmd.invoice_number.clone();
            a.ky_hieu = cmd.ky_hieu.clone();
        }
        "SET_MA_CQT" => a.ma_cqt = cmd.ma_cqt.clone(),
        other => {
            return Err(format!(
                "agent: unknown writeback command type {other:?} for {}",
                cmd.source_ref
            ));
        }
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::WritebackCommand;

    #[test]
    fn heartbeat_names_the_host_binary() {
        let v = heartbeat_version();
        assert_eq!(v, format!("dl-agent-{}", dl_agent::agent_version()));
        assert!(!v.contains("rust-"));
    }

    #[test]
    fn sources() {
        assert_eq!(sources_for(""), vec!["FO", "POS"]);
        assert_eq!(sources_for("POS"), vec!["POS"]);
        assert_eq!(sources_for("x"), vec!["FO", "POS"]);
    }

    #[test]
    fn writeback_mapping() {
        let mut c = WritebackCommand {
            source_ref: "FO:123".into(),
            kind: "SET_INVOICE_NUMBER".into(),
            invoice_number: "7".into(),
            ky_hieu: "1C26TAA".into(),
            ..Default::default()
        };
        let a = writeback_apply_from(&c).unwrap();
        assert_eq!(
            (
                a.bill_type.as_str(),
                a.bill_id.as_str(),
                a.invoice_number.as_str()
            ),
            ("FO", "123", "7")
        );
        c.kind = "SET_SOMETHING".into();
        assert!(writeback_apply_from(&c).is_err());
        c.source_ref = "nocolon".into();
        assert!(writeback_apply_from(&c).is_err());
    }
}
