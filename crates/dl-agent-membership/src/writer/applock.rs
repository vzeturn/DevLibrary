//! `AppLock`: a `READ COMMITTED` transaction holding transaction-scoped `sp_getapplock`
//! locks on SMILE's counters, so the agent and the front desk never draw the same folio,
//! confirm or cancel number. [`in_lock`] commits when the body succeeds and rolls back
//! otherwise; with a lock timeout it also bounds every row lock wait (`SET LOCK_TIMEOUT`)
//! and resets it afterwards.

use super::session::{Session, int, nvarchar, p};
use crate::jobs::plan::WriteError;
use crate::jobs::result::APPLOCK_TIMEOUT;
use crate::log::{self, V};

/// `FolioWriter.FolioNumberLock`.
pub const FOLIO_NUMBER_LOCK: &str = "SMILE_FolioNumber";
/// `FolioWriter.CancelNumberLock`.
pub const CANCEL_NUMBER_LOCK: &str = "SMILE_CancelNumber";
/// `ProfileWriter`'s lock on `GetProfileNumber`.
pub const PROFILE_NUMBER_LOCK: &str = "SMILE_ProfileNumber";
/// `AppLock.AcquireAsync`'s default wait.
pub const DEFAULT_TIMEOUT_MS: i32 = 15_000;

const BEGIN: &str = "SET TRANSACTION ISOLATION LEVEL READ COMMITTED; BEGIN TRANSACTION";
const COMMIT: &str = "COMMIT TRANSACTION";
const ROLLBACK: &str = "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION";

/// `AcquireAsync` (`lock_timeout_ms` `None`) or `AcquireManyAsync` (`Some`): runs `body`
/// inside the locked transaction.
pub fn in_lock<T>(
    s: &mut dyn Session,
    resources: &[&str],
    timeout_ms: i32,
    lock_timeout_ms: Option<i32>,
    body: impl FnOnce(&mut dyn Session) -> Result<T, WriteError>,
) -> Result<T, WriteError> {
    if resources.is_empty() {
        return Err(WriteError::Coded {
            code: "APPLOCK_NONE".into(),
            message: "Cần ít nhất một applock.".into(),
        });
    }
    if let Some(lt) = lock_timeout_ms {
        s.batch(&format!("SET LOCK_TIMEOUT {lt}"))?;
    }
    let out = locked(s, resources, timeout_ms, body);
    if lock_timeout_ms.is_some() {
        reset_lock_timeout(s);
    }
    out
}

fn locked<T>(
    s: &mut dyn Session,
    resources: &[&str],
    timeout_ms: i32,
    body: impl FnOnce(&mut dyn Session) -> Result<T, WriteError>,
) -> Result<T, WriteError> {
    s.batch(BEGIN)?;
    let run = (|| {
        for r in resources {
            get_app_lock(s, r, timeout_ms)?;
        }
        let v = body(s)?;
        s.batch(COMMIT)?;
        Ok(v)
    })();
    if run.is_err()
        && let Err(e) = s.batch(ROLLBACK)
    {
        log::warn(
            "Rollback of the SMILE applock transaction failed; SQL Server releases the transaction-scoped lock when the session ends",
            &[
                ("resource", V::S(&resources.join(","))),
                ("error", V::S(&e.to_string())),
            ],
        );
    }
    run
}

fn reset_lock_timeout(s: &mut dyn Session) {
    if let Err(e) = s.batch("SET LOCK_TIMEOUT -1") {
        log::warn(
            "Could not reset LOCK_TIMEOUT on the SMILE session after the booking write",
            &[("error", V::S(&e.to_string()))],
        );
    }
}

fn get_app_lock(s: &mut dyn Session, resource: &str, timeout_ms: i32) -> Result<(), WriteError> {
    let called = s.call(
        "sp_getapplock",
        &[
            p("@Resource", nvarchar(Some(resource))),
            p("@LockMode", nvarchar(Some("Exclusive"))),
            p("@LockOwner", nvarchar(Some("Transaction"))),
            p("@LockTimeout", int(timeout_ms)),
        ],
    )?;
    match called.status {
        Some(n) if n >= 0 => Ok(()),
        Some(-1) => Err(WriteError::Coded {
            code: APPLOCK_TIMEOUT.into(),
            message: format!("Không lấy được applock '{resource}' (mã -1: hết thời gian chờ)."),
        }),
        other => Err(WriteError::Coded {
            code: "APPLOCK_FAILED".into(),
            message: format!(
                "Không lấy được applock '{resource}' (mã {}).",
                other.map_or("?".into(), |n| n.to_string())
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::recorder::Recorder;
    use crate::writer::session::Called;

    #[test]
    fn the_body_commits_inside_the_lock_and_the_timeout_is_reset() {
        let mut r = Recorder::default();
        let v = in_lock(&mut r, &[FOLIO_NUMBER_LOCK], 10_000, Some(10_000), |s| {
            s.execute("UPDATE Folio SET X = 1", &[])?;
            Ok(7)
        })
        .unwrap();
        assert_eq!(v, 7);
        assert_eq!(
            r.texts(),
            vec![
                "SET LOCK_TIMEOUT 10000",
                BEGIN,
                "sp_getapplock",
                "UPDATE Folio SET X = 1",
                COMMIT,
                "SET LOCK_TIMEOUT -1"
            ]
        );
    }

    #[test]
    fn a_timed_out_lock_rolls_back_with_its_code() {
        let mut r = Recorder::default();
        r.on("sp_getapplock", |_| {
            Ok(Called {
                rows: Vec::new(),
                status: Some(-1),
            })
        });
        let e = in_lock(&mut r, &[FOLIO_NUMBER_LOCK], 15_000, None, |_| Ok(())).unwrap_err();
        assert!(matches!(e, WriteError::Coded { ref code, .. } if code == APPLOCK_TIMEOUT));
        assert_eq!(r.texts(), vec![BEGIN, "sp_getapplock", ROLLBACK]);
    }
}
