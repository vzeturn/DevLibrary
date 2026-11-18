//! The runner: plans a bundle against the ledger and applies, dry-runs or checks it.

use super::*;

/// Applies bundles to one database.
pub struct Runner {
    pub(super) o: Options,
}

pub(super) fn named(name: &str, v: impl dl_tds::IntoParam) -> Param {
    Param::new(name, v)
}

pub(super) fn row_params(
    b: &Bundle,
    m: &Migration,
    status: &str,
    in_tx: bool,
    source: &str,
    agent_version: &str,
    ms: Option<i64>,
) -> Vec<Param> {
    vec![
        named("Version", m.entry.seq),
        named("Name", m.entry.name.as_str()),
        named("Sha256", m.entry.sha256.as_str()),
        named("Chain", m.entry.chain.as_str()),
        named("Status", status),
        named("InTx", in_tx),
        named("Source", source),
        named("BundleId", b.manifest.bundle_id.as_str()),
        named("AgentVersion", agent_version),
        named("DurationMs", ParamValue::I64(ms)),
    ]
}

pub(super) fn history_params(m: &Migration, ms: i64) -> Vec<Param> {
    vec![
        named("Name", m.entry.name.as_str()),
        named("Sha256", m.entry.sha256.as_str()),
        named("DurationMs", ms),
    ]
}

pub(super) fn begin(ctx: &Ctx, c: &mut Conn<'_>) -> Result<(), MErr> {
    c.batch(ctx, "BEGIN TRANSACTION").map(drop).map_err(db_err)
}

pub(super) fn rollback(c: &mut Conn<'_>) {
    let ctx = Ctx::background().with_timeout(Duration::from_secs(10));
    let _ = c.batch(&ctx, "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION");
}

pub(super) fn commit(ctx: &Ctx, c: &mut Conn<'_>) -> Result<(), DbError> {
    c.batch(ctx, "COMMIT TRANSACTION").map(drop)
}

impl Runner {
    /// A runner over `o` (legacy names defaulted).
    pub fn new(mut o: Options) -> Runner {
        o.legacy = std::mem::take(&mut o.legacy).with_defaults();
        Runner { o }
    }

    pub(super) fn event(
        &self,
        b: &Bundle,
        m: &Migration,
        phase: &str,
        in_tx: bool,
        ms: i64,
        code: &str,
    ) {
        (self.o.progress)(Event {
            bundle_id: b.manifest.bundle_id.clone(),
            seq: m.entry.seq,
            name: m.entry.name.clone(),
            phase: phase.into(),
            in_tx,
            duration_ms: ms,
            code: code.into(),
        });
    }

    /// The bundle on its own: agent version and the forbidden-object scan.
    pub(super) fn static_checks(&self, b: &Bundle) -> Result<(), MErr> {
        if !version_at_least(&self.o.agent_version, &b.manifest.min_agent_version) {
            return Err(MErr::new(
                CODE_AGENT_TOO_OLD,
                format!(
                    "bundle needs agent {} or newer, this is {}",
                    b.manifest.min_agent_version, self.o.agent_version
                ),
            ));
        }
        let f = scan(b);
        if let Some(first) = f.first() {
            return Err(MErr::new(
                CODE_BUNDLE_FORBIDDEN,
                format!(
                    "{} line {} references a forbidden object ({}); {} finding(s), bundle refused",
                    first.name,
                    first.line,
                    first.rule,
                    f.len()
                ),
            ));
        }
        Ok(())
    }

    /// Reads and checks the ledger (and history for adoption) against the bundle.
    pub(super) fn plan(
        &self,
        ctx: &Ctx,
        c: &mut Conn<'_>,
        b: &Bundle,
    ) -> (Plan, LedgerState, Result<(), MErr>) {
        let mut p = Plan {
            bundle_id: b.manifest.bundle_id.clone(),
            from: b.manifest.from_version,
            to: b.manifest.to_version,
            ..Plan::default()
        };
        let (rows, exists) = match ledger::read_ledger_if_exists(ctx, c) {
            Ok(x) => x,
            Err(e) => return (p, LedgerState::default(), Err(e)),
        };
        p.ledger_exists = exists;
        let mut st = match ledger::check_ledger(&rows) {
            Ok(st) => st,
            Err(e) => return (p, LedgerState::default(), Err(e)),
        };
        if rows.is_empty() {
            let history = match ledger::read_history(ctx, c) {
                Ok(h) => h,
                Err(e) => return (p, st, Err(e)),
            };
            match ledger::adoptable(&history, b) {
                Ok(n) => p.adopt = n,
                Err(e) => return (p, st, Err(e)),
            }
            for m in &b.migrations[..p.adopt] {
                st.chains.insert(
                    m.entry.seq,
                    Row {
                        version: m.entry.seq,
                        name: m.entry.name.clone(),
                        sha256: m.entry.sha256.clone(),
                        chain: m.entry.chain.clone(),
                        status: ledger::STATUS_APPLIED.into(),
                    },
                );
                st.head = m.entry.seq;
                st.head_chain = m.entry.chain.clone();
            }
        }
        if let Err(e) = ledger::compare(&st, &b.manifest) {
            return (p, st, Err(e));
        }
        p.head = st.head;
        if let Some(r) = &st.running {
            p.partial = r.version;
            if self.o.rerun_partial != r.version {
                let e = MErr::new(
                    CODE_NEEDS_OPERATOR,
                    format!(
                        "version {} ({}) ran outside a transaction and did not finish; inspect it, then re-run it with --rerun-partial {}",
                        r.version, r.name, r.version
                    ),
                );
                return (p, st, Err(e));
            }
            p.rerun = true;
        }
        for m in &b.migrations {
            if m.entry.seq <= st.head {
                continue;
            }
            let batches = split_batches(&String::from_utf8_lossy(&m.sql));
            p.pending.push(Pending {
                seq: m.entry.seq,
                name: m.entry.name.clone(),
                in_tx: runs_in_tx(&m.entry, &batches),
                requires_backup: m.entry.requires_backup,
                lock_timeout_ms: m.entry.lock_timeout_ms,
                online_index: m.entry.online_index,
            });
        }
        (p, st, Ok(()))
    }

    /// Verifies the bundle against the ledger and lists what would run, writing nothing:
    /// no ledger table, no lock, no backup.
    pub fn dry_run(&self, ctx: &Ctx, b: &Bundle) -> (Plan, Result<(), MErr>) {
        let bare = || Plan {
            bundle_id: b.manifest.bundle_id.clone(),
            ..Plan::default()
        };
        if let Err(e) = self.static_checks(b) {
            return (bare(), Err(e));
        }
        let mut c = match self.o.pool.get(ctx) {
            Ok(c) => c,
            Err(e) => return (bare(), Err(db_err(e))),
        };
        let (mut p, _, res) = self.plan(ctx, &mut c, b);
        if let Err(e) = res {
            return (p, Err(e));
        }
        if p.pending.iter().any(|pe| pe.requires_backup)
            && let Err(e) = self.o.backup.check((self.o.now)())
        {
            p.backup_refusal = e.to_string();
        }
        let perms = match c.batch(ctx, PERMS_SQL) {
            Ok(r) => r,
            Err(e) => return (p, Err(db_err(e))),
        };
        let vals = perms
            .rows()
            .first()
            .map(|r| r.values().to_vec())
            .unwrap_or_default();
        for (i, name) in PERM_NAMES.iter().enumerate() {
            let granted = vals
                .get(i)
                .and_then(|v| v.convert::<Option<i64>>().ok().flatten())
                == Some(1);
            if !granted {
                p.missing_permissions.push((*name).to_owned());
            }
        }
        (p, Ok(()))
    }

    /// Runs the bundle, holding the migration lock on one connection for the whole run;
    /// the first refusal or failure stops it. The connection is closed afterwards (its
    /// session carries the lock, session context and lock timeout).
    pub fn apply(&self, ctx: &Ctx, b: &Bundle) -> (Report, Result<(), MErr>) {
        let mut rep = Report {
            bundle_id: b.manifest.bundle_id.clone(),
            ..Report::default()
        };
        if let Err(e) = self.static_checks(b) {
            return (rep, Err(e));
        }
        let mut c = match self.o.pool.get(ctx) {
            Ok(c) => c,
            Err(e) => return (rep, Err(db_err(e))),
        };
        let res = self.lock(ctx, &mut c).and_then(|()| {
            let r = self.apply_locked(ctx, &mut c, b, &mut rep);
            self.unlock(&mut c);
            r
        });
        c.discard();
        (rep, res)
    }

    pub(super) fn apply_locked(
        &self,
        ctx: &Ctx,
        c: &mut Conn<'_>,
        b: &Bundle,
        rep: &mut Report,
    ) -> Result<(), MErr> {
        let legacy = &self.o.legacy;
        c.query(
            ctx,
            SESSION_CONTEXT_SQL,
            &[
                named("FoDb", legacy.fo.as_str()),
                named("PosDb", legacy.pos.as_str()),
                named("BoDb", legacy.bo.as_str()),
            ],
        )
        .map_err(db_err)?;
        c.batch(ctx, ledger::ENSURE_LEDGER_SQL).map_err(db_err)?;
        let (p, st, res) = self.plan(ctx, c, b);
        rep.head = st.head;
        res?;
        if p.adopt > 0 {
            self.adopt(ctx, c, b, p.adopt)?;
            rep.adopted = p.adopt;
        }
        let by_seq: BTreeMap<i64, &Migration> =
            b.migrations.iter().map(|m| (m.entry.seq, m)).collect();
        for pe in &p.pending {
            let m = by_seq[&pe.seq];
            if m.entry.requires_backup && rep.backup.is_none() {
                let proof = match self.o.backup.ensure(ctx, c, m.entry.seq, (self.o.now)()) {
                    Ok(p) => p,
                    Err(e) => {
                        self.event(b, m, "failed", pe.in_tx, 0, e.code);
                        return Err(e);
                    }
                };
                self.event(b, m, "backup", pe.in_tx, 0, "");
                self.o.log.info(
                    "agent.migrate.backup",
                    attrs![
                        "bundle_id" => &b.manifest.bundle_id,
                        "seq" => m.entry.seq,
                        "mode" => &proof.mode,
                        "file" => &proof.file
                    ],
                );
                rep.backup = Some(proof);
            }
            let rerun = p.rerun && pe.seq == p.partial;
            match self.apply_one(ctx, c, b, m, pe.in_tx, rerun) {
                Ok(took) => {
                    rep.head = m.entry.seq;
                    rep.applied.push(Applied {
                        seq: m.entry.seq,
                        name: m.entry.name.clone(),
                        in_tx: pe.in_tx,
                        duration_ms: took,
                    });
                    self.event(b, m, "applied", pe.in_tx, took, "");
                    self.o.log.info(
                        "agent.migrate.applied",
                        attrs![
                            "bundle_id" => &b.manifest.bundle_id,
                            "seq" => m.entry.seq,
                            "name" => &m.entry.name,
                            "in_tx" => pe.in_tx,
                            "ms" => took
                        ],
                    );
                }
                Err((took, e)) => {
                    self.event(b, m, "failed", pe.in_tx, took, e.code);
                    self.o.log.error(
                        "agent.migrate.failed",
                        attrs![
                            "bundle_id" => &b.manifest.bundle_id,
                            "seq" => m.entry.seq,
                            "name" => &m.entry.name,
                            "in_tx" => pe.in_tx,
                            "reason" => e.to_string()
                        ],
                    );
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    pub(super) fn lock(&self, ctx: &Ctx, c: &mut Conn<'_>) -> Result<(), MErr> {
        let wait = i64::try_from(self.o.lock_wait.as_millis()).unwrap_or(i64::MAX);
        let r = c
            .query(
                ctx,
                LOCK_SQL,
                &[named("Resource", LOCK_RESOURCE), named("Wait", wait)],
            )
            .map_err(db_err)?;
        let rc = r
            .rows()
            .first()
            .and_then(|row| row.get::<i64>(0).ok())
            .ok_or_else(|| MErr::new(CODE_DB, "database unavailable"))?;
        if rc < 0 {
            return Err(MErr::new(
                CODE_LOCK_BUSY,
                format!("another migration runner holds {LOCK_RESOURCE} (sp_getapplock {rc})"),
            ));
        }
        Ok(())
    }

    pub(super) fn unlock(&self, c: &mut Conn<'_>) {
        let ctx = Ctx::background().with_timeout(Duration::from_secs(10));
        if let Err(e) = c.query(&ctx, UNLOCK_SQL, &[named("Resource", LOCK_RESOURCE)]) {
            // The lock is session-owned: closing the connection frees it too.
            self.o
                .log
                .warn("agent.migrate.unlock", attrs!["reason" => e.to_string()]);
        }
    }

    pub(super) fn adopt(
        &self,
        ctx: &Ctx,
        c: &mut Conn<'_>,
        b: &Bundle,
        n: usize,
    ) -> Result<(), MErr> {
        begin(ctx, c)?;
        for m in &b.migrations[..n] {
            let params = row_params(
                b,
                m,
                ledger::STATUS_APPLIED,
                true,
                ledger::SOURCE_ADOPTED,
                &self.o.agent_version,
                None,
            );
            if let Err(e) = c.query(ctx, ledger::INSERT_ROW_SQL, &params) {
                rollback(c);
                return Err(db_err(e));
            }
        }
        commit(ctx, c).map_err(db_err)?;
        let last = &b.migrations[n - 1];
        (self.o.progress)(Event {
            bundle_id: b.manifest.bundle_id.clone(),
            seq: last.entry.seq,
            name: last.entry.name.clone(),
            phase: "adopt".into(),
            in_tx: true,
            ..Event::default()
        });
        self.o.log.info(
            "agent.migrate.adopted",
            attrs!["bundle_id" => &b.manifest.bundle_id, "versions" => n],
        );
        Ok(())
    }

    /// `SET LOCK_TIMEOUT n;`, the one statement built at run time: an integer from the
    /// signed manifest, range-checked by the verifier (1..600000).
    pub(super) fn run_batches(
        &self,
        ctx: &Ctx,
        c: &mut Conn<'_>,
        m: &Migration,
        batches: &[Batch],
    ) -> Result<(), MErr> {
        c.batch(
            ctx,
            &format!("SET LOCK_TIMEOUT {};", m.entry.lock_timeout_ms),
        )
        .map_err(db_err)?;
        for (i, batch) in batches.iter().enumerate() {
            if let Err(e) = c.batch(ctx, &batch.text) {
                let mut f = sql_failure(CODE_MIGRATION_FAILED, m.entry.seq, &e);
                f.msg = format!(
                    "version {} ({}) batch {} (line {}): {}",
                    m.entry.seq,
                    m.entry.name,
                    i + 1,
                    batch.line,
                    f.msg
                );
                return Err(f);
            }
        }
        Ok(())
    }

    pub(super) fn record(
        &self,
        ctx: &Ctx,
        c: &mut Conn<'_>,
        b: &Bundle,
        m: &Migration,
        ms: i64,
    ) -> Result<(), MErr> {
        let params = row_params(
            b,
            m,
            ledger::STATUS_APPLIED,
            true,
            ledger::SOURCE_AGENT,
            &self.o.agent_version,
            Some(ms),
        );
        c.query(ctx, ledger::INSERT_ROW_SQL, &params)
            .map_err(db_err)?;
        c.query(ctx, ledger::HISTORY_SQL, &history_params(m, ms))
            .map_err(db_err)?;
        Ok(())
    }

    pub(super) fn apply_one(
        &self,
        ctx: &Ctx,
        c: &mut Conn<'_>,
        b: &Bundle,
        m: &Migration,
        in_tx: bool,
        rerun: bool,
    ) -> Result<i64, (i64, MErr)> {
        let started = Instant::now();
        let batches = split_batches(&String::from_utf8_lossy(&m.sql));
        self.event(b, m, "start", in_tx, 0, "");
        if in_tx {
            begin(ctx, c).map_err(|e| (0, e))?;
            if let Err(e) = self.run_batches(ctx, c, m, &batches) {
                rollback(c);
                return Err((ms_since(started), e));
            }
            let ms = ms_since(started);
            if let Err(e) = self.record(ctx, c, b, m, ms) {
                rollback(c);
                return Err((ms, e));
            }
            if let Err(e) = commit(ctx, c) {
                let mut f = sql_failure(CODE_MIGRATION_FAILED, m.entry.seq, &e);
                f.msg = format!(
                    "version {} ({}) commit failed: {}",
                    m.entry.seq, m.entry.name, f.msg
                );
                return Err((ms, f));
            }
            return Ok(ms);
        }
        if !rerun {
            let params = row_params(
                b,
                m,
                ledger::STATUS_RUNNING,
                false,
                ledger::SOURCE_AGENT,
                &self.o.agent_version,
                None,
            );
            c.query(ctx, ledger::INSERT_ROW_SQL, &params)
                .map_err(|e| (0, db_err(e)))?;
        }
        // On failure the running row stays: the next run stops for an operator.
        self.run_batches(ctx, c, m, &batches)
            .map_err(|e| (ms_since(started), e))?;
        let ms = ms_since(started);
        let mut finish = || -> Result<(), MErr> {
            begin(ctx, c)?;
            let r = c
                .query(
                    ctx,
                    ledger::FINISH_ROW_SQL,
                    &[
                        named("DurationMs", ms),
                        named("AgentVersion", self.o.agent_version.as_str()),
                        named("BundleId", b.manifest.bundle_id.as_str()),
                        named("Version", m.entry.seq),
                    ],
                )
                .and_then(|_| c.query(ctx, ledger::HISTORY_SQL, &history_params(m, ms)));
            if let Err(e) = r {
                rollback(c);
                return Err(db_err(e));
            }
            commit(ctx, c).map_err(db_err)
        };
        finish().map_err(|e| (ms, e))?;
        Ok(ms)
    }

    /// Compares the ledger with the bundle, writing nothing; every version both hold is
    /// compared. An absent ledger is no drift.
    pub fn check_drift(&self, ctx: &Ctx, b: &Bundle) -> Result<Vec<DriftFinding>, MErr> {
        let mut c = self.o.pool.get(ctx).map_err(db_err)?;
        let (rows, _) = ledger::read_ledger_if_exists(ctx, &mut c)?;
        let f = |object: &str, code: &str| DriftFinding {
            kind: "ledger".into(),
            object: object.into(),
            code: code.into(),
        };
        let mut out = Vec::new();
        if ledger::check_ledger(&rows).is_err() {
            out.push(f(LEDGER_TABLE, CODE_LEDGER_CORRUPT));
        }
        let mut by_version = BTreeMap::new();
        for row in &rows {
            by_version.insert(row.version, row);
            if row.status == ledger::STATUS_RUNNING {
                out.push(f(&row.name, DRIFT_UNFINISHED));
            }
        }
        let m = &b.manifest;
        if m.from_version > 0
            && let Some(row) = by_version.get(&m.from_version)
            && row.chain != m.prev_chain
        {
            out.push(f(&format!("version {}", m.from_version), DRIFT_PREV_CHAIN));
        }
        for e in &m.migrations {
            let Some(row) = by_version.get(&e.seq) else {
                continue;
            };
            if row.name != e.name || row.sha256 != e.sha256 || row.chain != e.chain {
                out.push(f(&e.name, DRIFT_CHECKSUM));
            }
        }
        Ok(out)
    }
}
