//! Write-back: applying polled commands to SQL Server and acking them, under the budgets
//! of one batch (RT-1212).

use std::time::{Duration, Instant};

use super::{CycleResult, Runner, ms_since};
use crate::log::{self, V};
use crate::platform::{ApiError, WritebackCommand};
use crate::sql::WritebackApply;

/// The longest SQL timeout of one write-back command, so one bill whose UPDATE hangs (a
/// lock SMILE holds, a dead connection) cannot use up the whole batch budget (RT-1212).
pub(super) const WRITEBACK_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// The deadline of a batch's ack, counted from when the ack starts, not from the batch's
/// apply budget: before RT-1212 an ack sent after the budget ran out failed at once, and
/// the hub re-sent the whole batch until it marked every command FAILED.
pub(super) const WRITEBACK_ACK_TIMEOUT: Duration = Duration::from_secs(20);

/// The SQL timeout of the next write-back command: its own cap within what is left of
/// the batch budget, or `None` once the budget is spent (a zero timeout would mean no
/// timeout at all to the driver).
pub(super) fn command_timeout(deadline: Instant, now: Instant) -> Option<Duration> {
    let left = deadline.saturating_duration_since(now);
    (!left.is_zero()).then(|| left.min(WRITEBACK_COMMAND_TIMEOUT))
}

impl Runner {
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
        cmds: Vec<WritebackCommand>,
        deadline: Instant,
        res: &mut CycleResult,
    ) {
        res.polled += cmds.len() as i64;
        let mut to_ack = Vec::new();
        let mut left = 0i64;
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
            let Some(timeout) = command_timeout(deadline, Instant::now()) else {
                left += 1; // budget spent: the hub sends it again
                continue;
            };
            if let Err(e) = self.sql.apply_writeback(&apply, timeout) {
                log::error(
                    "writeback: apply failed",
                    &[
                        ("command_id", V::S(&id)),
                        ("source_ref", V::S(&cmd.source_ref)),
                        ("error", V::S(&e)),
                    ],
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
        if left > 0 {
            log::warn(
                "writeback: budget spent, rest left for the hub to send again",
                &[("not_applied", V::I(left))],
            );
            res.writeback_errors.list.push(format!(
                "agent: writeback budget spent with {left} commands not applied"
            ));
        }
        // The ack gets its own deadline: commands already applied are reported even when
        // the batch budget ran out on a later one (RT-1212).
        if !to_ack.is_empty() {
            match self
                .cli
                .ack(&to_ack, Instant::now() + WRITEBACK_ACK_TIMEOUT)
            {
                Ok(()) => res.acked += to_ack.len() as i64,
                Err(e) => {
                    log::error(
                        "writeback: ack failed; the hub sends these commands again",
                        &[
                            ("count", V::I(to_ack.len() as i64)),
                            ("error", V::S(&e.to_string())),
                        ],
                    );
                    if e != ApiError::DeviceReplaced {
                        res.poll_failed = Some(e.to_string());
                    }
                    res.writeback_errors.api(e);
                }
            }
        }
    }

    /// Hands write-back to a dedicated worker (RT-0785): from now on the cycle reports
    /// what the worker did instead of polling itself.
    pub fn hand_writeback_to_worker(&mut self) {
        self.writeback_worker = true;
    }

    /// Applies commands the write-back worker polled, under the cycle timeout. The
    /// outcome is returned and also kept for the next cycle's log, heartbeat and problems.
    pub fn apply_polled_writeback(&mut self, cmds: Vec<WritebackCommand>) -> CycleResult {
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
}

/// `writebackApplyFrom`.
pub fn writeback_apply_from(cmd: &WritebackCommand) -> Result<WritebackApply, String> {
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
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use dl_uuid::Uuid;

    use super::*;
    use crate::agent_config::AgentConfig;
    use crate::outbox::Outbox;
    use crate::platform::Platform;
    use crate::sql::Sql;

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

    #[test]
    fn command_timeout_is_capped_and_ends_with_the_budget() {
        let now = Instant::now();
        assert_eq!(
            command_timeout(now + Duration::from_secs(120), now),
            Some(WRITEBACK_COMMAND_TIMEOUT)
        );
        assert_eq!(
            command_timeout(now + Duration::from_secs(5), now),
            Some(Duration::from_secs(5))
        );
        assert_eq!(command_timeout(now, now), None);
        assert_eq!(command_timeout(now, now + Duration::from_secs(1)), None);
    }

    /// Every request a fake hub saw: (request line, body).
    type Seen = Arc<Mutex<Vec<(String, String)>>>;

    /// A fake hub on a local socket that answers every request with 204 and records it.
    fn fake_hub() -> (String, Seen) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen: Seen = Arc::default();
        let rec = seen.clone();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let mut rd = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                rd.read_line(&mut line).unwrap();
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    if rd.read_line(&mut h).unwrap() == 0 || h == "\r\n" {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; len];
                rd.read_exact(&mut body).unwrap();
                rec.lock().unwrap().push((
                    line.trim_end().to_owned(),
                    String::from_utf8_lossy(&body).into_owned(),
                ));
                s.write_all(b"HTTP/1.1 204 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .unwrap();
            }
        });
        (base, seen)
    }

    fn uuid(s: &str) -> Uuid {
        s.parse().unwrap()
    }

    /// RT-1212: a batch whose budget a hung SQL write used up still acks what was already
    /// applied, on the ack's own deadline, and leaves the rest for the hub without
    /// touching SQL with a spent budget.
    #[test]
    fn applied_commands_are_acked_after_the_batch_budget_is_spent() {
        let (base, seen) = fake_hub();
        let dir = std::env::temp_dir().join(format!("dl-einvoice-wb-ack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = AgentConfig::default();
        // Nothing listens on port 1: any SQL write would fail and be counted.
        let sql = Sql::new(
            "sqlserver://einvoice@127.0.0.1:1?database=SMILE_FO",
            "",
            cfg.sp_names(),
        );
        let cli = Platform::new(
            &base,
            "agent-key",
            uuid("6f9619ff-8b86-d011-b42d-00c04fc964ff"),
        );
        let done = uuid("0b5a1f3e-7c2d-4e8f-9a6b-1c3d5e7f9a01");
        let left = uuid("0b5a1f3e-7c2d-4e8f-9a6b-1c3d5e7f9a02");
        let mut outbox = Outbox::open(&dir.join("outbox.rs.json")).unwrap();
        outbox.mark_applied(&done.to_string()).unwrap();
        let mut r = Runner::new(sql, cli, outbox, cfg);
        let cmd = |id: Uuid, bill: &str| WritebackCommand {
            id,
            kind: "SET_INVOICE_NUMBER".into(),
            source_ref: format!("FO:{bill}"),
            invoice_number: "7".into(),
            ky_hieu: "1C26TAA".into(),
            ..Default::default()
        };
        let mut res = CycleResult::default();

        // The batch budget is already spent, as after one SQL write that hung through it.
        r.apply_commands(
            vec![cmd(done, "1"), cmd(left, "2")],
            Instant::now(),
            &mut res,
        );
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(
            (res.polled, res.skipped, res.applied, res.acked),
            (2, 1, 0, 1),
            "{res:?}"
        );
        assert!(res.sql_errors.is_empty(), "{:?}", res.sql_errors);
        assert_eq!(res.poll_failed, None);
        assert_eq!(
            res.writeback_errors.list,
            ["agent: writeback budget spent with 1 commands not applied"]
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "{seen:?}");
        let (line, body) = &seen[0];
        assert!(
            line.starts_with("POST /api/v1/agent/writeback:ack "),
            "{line}"
        );
        assert!(body.contains(&done.to_string()), "{body}");
        assert!(!body.contains(&left.to_string()), "{body}");
    }
}
