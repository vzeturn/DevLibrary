//! `order-residue [--apply] [--older-than-minutes <n>]` (RT-1345, `OrderResidueCommand.cs`):
//! the age check before anything else, the enrolment refusal, the report-only run that
//! changes nothing, the audited `--apply` and how failures are printed.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use dl_agent_pda_sync::commands::{
    CommandError, OrderResidueSnapshot, OrderResidueSource, ResidueCheck, ResidueCleaned,
    ResidueInfoRow, Result,
};
use dl_agent_pda_sync::config::{AgentError, ManualClock, Timestamp};
use dl_agent_pda_sync::contracts::time::DateTime;
use dl_agent_pda_sync::hosting::cli::{EXIT_FAILED, EXIT_OK, EXIT_UNHANDLED, EXIT_USAGE};
use dl_agent_pda_sync::hosting::order_residue::{
    AUDIT_FILE, NOTHING_CHANGED, ResidueArgs, min_age, run_parts,
};
use dl_agent_pda_sync::hosting::{Options, VerbTable, run_with};
use dl_agent_pda_sync::util::console::MemoryLines;
use dl_agent_pda_sync::util::ctx::Ctx;

const OURS: i64 = 400000101;
const POS_WIN: i64 = 400000202;
/// 2026-11-28T01:02:03.456Z.
const NOW_NANOS: i128 = 1_795_827_723_456_000_000;
const STAMP: &str = "2026-11-28T01:02:03.4560000Z";

struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        let d = std::env::temp_dir()
            .join("pda-sync-agent-tests")
            .join(dl_uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn chk(check_no: i64) -> ResidueCheck {
    ResidueCheck {
        rvc_no: 4,
        check_no,
        opened_local: DateTime::new(2026, 11, 26, 8, 0, 0).unwrap(),
    }
}

struct FakeSource {
    fail: Option<fn() -> CommandError>,
    min_age: Mutex<Option<i32>>,
    cleaned: Mutex<u32>,
}

impl FakeSource {
    fn new() -> FakeSource {
        FakeSource {
            fail: None,
            min_age: Mutex::new(None),
            cleaned: Mutex::new(0),
        }
    }
}

impl OrderResidueSource for FakeSource {
    fn our_check_nos(&self, _ctx: &Ctx) -> Result<HashSet<i64>> {
        match self.fail {
            Some(f) => Err(f()),
            None => Ok(HashSet::from([OURS])),
        }
    }

    fn read(&self, min_age_minutes: i32, _ctx: &Ctx) -> Result<OrderResidueSnapshot> {
        *self.min_age.lock().unwrap() = Some(min_age_minutes);
        Ok(OrderResidueSnapshot {
            empty_open_checks: vec![chk(OURS), chk(POS_WIN)],
            orphan_info_rows: vec![
                ResidueInfoRow {
                    check_no: OURS,
                    trn_no: 3,
                    sent: false,
                },
                ResidueInfoRow {
                    check_no: OURS,
                    trn_no: 4,
                    sent: true,
                },
            ],
            stale_open_logs: vec![chk(OURS)],
            orphan_send_audits: vec![POS_WIN],
        })
    }

    fn clean(
        &self,
        ours: &OrderResidueSnapshot,
        _cleaned_by: &str,
        _min_age_minutes: i32,
        _ctx: &Ctx,
    ) -> Result<ResidueCleaned> {
        *self.cleaned.lock().unwrap() += 1;
        Ok(ResidueCleaned {
            empty_checks: ours.empty_open_checks.len() as i32,
            info_rows: ours.orphan_info_rows.len() as i32,
            open_logs: ours.stale_open_logs.len() as i32,
        })
    }
}

fn args(apply: bool) -> ResidueArgs {
    ResidueArgs {
        apply,
        min_age_minutes: 45,
        by: "op@till1".to_owned(),
    }
}

fn run(source: &FakeSource, a: &ResidueArgs, audit: &std::path::Path) -> (i32, String, String) {
    let (out, err) = (MemoryLines::shared(), MemoryLines::shared());
    let clock = ManualClock::new(Timestamp::from_unix_nanos(NOW_NANOS));
    let code = run_parts(source, a, audit, &clock, &*out, &*err, &Ctx::background());
    (code, out.text(), err.text())
}

fn cli(a: &[&str]) -> (i32, String, String) {
    let a: Vec<String> = a.iter().map(|s| (*s).to_owned()).collect();
    let (out, err) = (MemoryLines::shared(), MemoryLines::shared());
    let code = run_with(&a, &*out, &*err, &VerbTable::default());
    (code, out.text(), err.text())
}

#[test]
fn the_age_is_a_whole_number_of_minutes_and_defaults_to_120() {
    assert_eq!(min_age(&Options::parse(&["order-residue"])), Some(120));
    assert_eq!(
        min_age(&Options::parse(&["--older-than-minutes", "30"])),
        Some(30)
    );
    for bad in ["0", "-5", "abc", "1.5", " 7", "+7", "99999999999"] {
        assert_eq!(
            min_age(&Options::parse(&["--older-than-minutes", bad])),
            None,
            "{bad}"
        );
    }
    // A bare flag is "true", not a number.
    assert_eq!(
        min_age(&Options::parse(&["--older-than-minutes", "--apply"])),
        None
    );
}

#[test]
fn a_bad_age_is_a_usage_error_before_the_enrolment_is_read() {
    let d = Dir::new();
    let data = d.0.join("data").to_string_lossy().into_owned();
    let (code, out, err) = cli(&[
        "order-residue",
        "--data-dir",
        &data,
        "--older-than-minutes",
        "0",
    ]);
    assert_eq!(code, EXIT_USAGE);
    assert_eq!(out, "");
    assert_eq!(
        err.trim_end(),
        "[BAD_ARGUMENTS] --older-than-minutes must be a whole number of minutes, 1 or more."
    );
}

#[test]
fn an_unenrolled_machine_is_refused() {
    let d = Dir::new();
    let data = d.0.join("data").to_string_lossy().into_owned();
    let (code, out, err) = cli(&["order-residue", "--data-dir", &data, "--apply"]);
    assert_eq!(code, EXIT_FAILED);
    assert_eq!(out, "");
    assert_eq!(
        err.trim_end(),
        "[NOT_ENROLLED] This machine is not enrolled."
    );
}

#[test]
fn the_report_alone_changes_nothing_and_writes_no_audit() {
    let d = Dir::new();
    let audit = d.0.join(AUDIT_FILE);
    let source = FakeSource::new();
    let (code, out, err) = run(&source, &args(false), &audit);
    assert_eq!(code, EXIT_OK, "{err}");
    assert_eq!(*source.min_age.lock().unwrap(), Some(45));
    assert_eq!(*source.cleaned.lock().unwrap(), 0);
    assert!(!audit.exists());
    assert!(
        out.contains("  empty open checks (no line, opened by the PDA workstation): 1"),
        "{out}"
    );
    assert!(
        out.contains("  rows of checks POS WIN opened by itself, ignored: 2"),
        "{out}"
    );
    assert!(!out.contains("Cleaned:"), "{out}");
    assert!(out.trim_end().ends_with(NOTHING_CHANGED), "{out}");
}

#[test]
fn apply_audits_every_action_before_and_after_the_cleanup() {
    let d = Dir::new();
    let audit = d.0.join(AUDIT_FILE);
    std::fs::write(&audit, "earlier line\n").unwrap();
    let source = FakeSource::new();
    let (code, out, err) = run(&source, &args(true), &audit);
    assert_eq!(code, EXIT_OK, "{err}");
    assert_eq!(*source.cleaned.lock().unwrap(), 1);
    assert!(
        out.contains("Cleaned: 1 empty checks, 1 info rows, 1 open-log rows."),
        "{out}"
    );
    assert!(!out.contains(NOTHING_CHANGED), "{out}");
    let text = std::fs::read_to_string(&audit).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let p = |l: &str| format!("{STAMP} op@till1 {l}");
    assert_eq!(
        lines,
        [
            "earlier line".to_owned(),
            p("order-residue apply by=op@till1 emptyChecks=1 infoRows=1 openLogs=1"),
            p(&format!("cancel-empty rvc=4 check={OURS}")),
            p(&format!("drop-info check={OURS} trn=3")),
            p(&format!("close-openlog rvc=4 check={OURS}")),
            p("order-residue done emptyChecks=1 infoRows=1 openLogs=1"),
        ]
    );
}

#[test]
fn an_audit_that_cannot_be_written_stops_before_the_cleanup() {
    let d = Dir::new();
    // A folder where the audit file should be: the append fails.
    let audit = d.0.join(AUDIT_FILE);
    std::fs::create_dir_all(&audit).unwrap();
    let source = FakeSource::new();
    let (code, out, err) = run(&source, &args(true), &audit);
    assert_eq!(code, EXIT_UNHANDLED);
    assert_eq!(out, "");
    assert_eq!(*source.cleaned.lock().unwrap(), 0);
    assert!(
        err.starts_with(
            "Unhandled exception. System.InvalidOperationException: Could not append to "
        ),
        "{err}"
    );
}

#[test]
fn a_coded_failure_prints_its_code_and_exits_1() {
    let d = Dir::new();
    let mut source = FakeSource::new();
    source.fail = Some(|| {
        CommandError::Agent(AgentError::new(
            "SQL_PASSWORD_MISSING",
            "No SQL password is stored.",
        ))
    });
    let (code, out, err) = run(&source, &args(true), &d.0.join(AUDIT_FILE));
    assert_eq!(code, EXIT_FAILED);
    assert_eq!(out, "");
    assert_eq!(
        err.trim_end(),
        "[SQL_PASSWORD_MISSING] No SQL password is stored."
    );
}

#[test]
fn any_other_failure_is_unhandled() {
    let d = Dir::new();
    let mut source = FakeSource::new();
    source.fail = Some(|| CommandError::invalid("store answered nonsense"));
    let (code, out, err) = run(&source, &args(false), &d.0.join(AUDIT_FILE));
    assert_eq!(code, EXIT_UNHANDLED);
    assert_eq!(out, "");
    assert_eq!(
        err.trim_end(),
        "Unhandled exception. System.InvalidOperationException: store answered nonsense"
    );
}
