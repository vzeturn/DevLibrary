//! Which SMILE database the agent is reading (`PmsDbIdentityPolicy`,
//! `SmileDbIdentityProbe` snapshot, `PmsIdentityTracker`).
//!
//! The fingerprint `lower(server)|lower(db)|create_date[.fff]` (plus `|{database_guid}` when
//! readable) goes to the hub in every heartbeat and folio-observation batch; the hub holds
//! reports from a database that is not the hotel's confirmed one.
//!
//! On top of .NET, the module pins the first fingerprint it reads in a run: when a later
//! read shows another database (a restored copy, a test database behind the same
//! connection string), the scanners stop and the module reports
//! `membership_db_identity_changed` until the original database is back or the module is
//! restarted.

pub mod gate;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dl_time::DateTime;
use dl_uuid::Uuid;

use crate::smile::SmileError;
use crate::wire::DbIdentity;

/// How often the identity is re-read.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// The retry delay after a failed read.
pub const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(60);

/// Whether SMILE shows recent activity by anyone but the agent (`ForeignActivityState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignState {
    /// The last foreign change is known.
    Known,
    /// No foreign change in the 30-day window.
    NoneInWindow,
    /// Not readable (permissions, schema).
    Unreadable,
}

/// One identity read (`SmileDbIdentitySnapshot`).
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// The identity.
    pub identity: DbIdentity,
    /// SMILE's `GETDATE()` at the read.
    pub pms_now: DateTime,
    /// The newest folio change by a clerk other than `APP`.
    pub last_foreign_change: Option<DateTime>,
    /// See [`ForeignState`].
    pub foreign_state: ForeignState,
}

impl Snapshot {
    /// The fingerprint.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.identity)
    }

    /// Minutes since the last foreign change at `pms_now` (`ForeignAgeMinutesAt`).
    pub fn foreign_age_minutes_at(&self, pms_now: DateTime) -> Option<i64> {
        self.last_foreign_change
            .map(|last| (pms_now - last).whole_minutes().max(0))
    }
}

/// `PmsDbIdentityPolicy.Fingerprint`.
pub fn fingerprint(id: &DbIdentity) -> String {
    let d = id.create_date;
    let core = format!(
        "{}|{}|{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
        id.server.trim().to_lowercase(),
        id.database.trim().to_lowercase(),
        d.year(),
        d.month().number(),
        d.day(),
        d.hour(),
        d.minute(),
        d.second(),
        d.nanosecond() / 1_000_000
    );
    match id.database_guid {
        Some(g) => format!("{core}|{g}"),
        None => core,
    }
}

/// `PmsDbIdentityPolicy.SameIdentityIgnoringMissingGuid`: equal cores, and at most one
/// side carries a GUID (a login that cannot read `database_guid`).
pub fn same_ignoring_missing_guid(a: &str, b: &str) -> bool {
    fn split(f: &str) -> (&str, bool) {
        match f.rfind('|') {
            Some(cut) if cut > 0 && is_guid_d(&f[cut + 1..]) => (&f[..cut], true),
            _ => (f, false),
        }
    }
    let (ca, ga) = split(a);
    let (cb, gb) = split(b);
    !(ga && gb) && ca == cb
}

fn is_guid_d(s: &str) -> bool {
    s.len() == 36 && s.parse::<Uuid>().is_ok()
}

/// Whether two fingerprints name the same database.
pub fn same_database(a: &str, b: &str) -> bool {
    a == b || same_ignoring_missing_guid(a, b)
}

/// Reads the identity (`ISmileDbIdentityProbe`).
pub trait IdentityProbe: Send + Sync {
    /// One read.
    fn read_identity(&self) -> Result<Snapshot, SmileError>;
}

#[derive(Debug, Default)]
struct State {
    reading: Option<(Snapshot, Instant)>,
    pinned: Option<String>,
    next_due: Option<Instant>,
    last_failed: bool,
}

/// What a refresh found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refresh {
    /// Not due; nothing read.
    Skipped,
    /// Read; same database as pinned (or the first read, which pins it).
    Same,
    /// Read; another database than the pinned one.
    Changed {
        /// The pinned fingerprint.
        pinned: String,
        /// What was read now.
        seen: String,
    },
    /// The read failed; the previous reading is kept.
    Failed(String),
}

/// `PmsIdentityTracker`: caches the identity, refreshes every 10 minutes (1 minute after a
/// failure), and pins the first fingerprint of the run.
pub struct Tracker {
    probe: Arc<dyn IdentityProbe>,
    state: Mutex<State>,
}

impl std::fmt::Debug for Tracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tracker").finish_non_exhaustive()
    }
}

impl Tracker {
    /// A tracker over `probe`.
    pub fn new(probe: Arc<dyn IdentityProbe>) -> Tracker {
        Tracker {
            probe,
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Re-reads when due (or `force`).
    pub fn refresh_if_due(&self, force: bool) -> Refresh {
        let mut s = self.lock();
        let now = Instant::now();
        let due = s.next_due.is_none_or(|at| now >= at);
        if !force && !due && (s.reading.is_some() || s.last_failed) {
            return Refresh::Skipped;
        }
        match self.probe.read_identity() {
            Ok(snap) => {
                let fp = snap.fingerprint();
                let previous = s.reading.as_ref().map(|(r, _)| r.fingerprint());
                s.reading = Some((snap, Instant::now()));
                s.next_due = Some(now + REFRESH_INTERVAL);
                s.last_failed = false;
                if previous.as_deref() != Some(fp.as_str()) {
                    crate::log::info(
                        "SMILE database identity read",
                        &[("fingerprint", crate::log::V::S(&fp))],
                    );
                }
                match &s.pinned {
                    None => {
                        s.pinned = Some(fp);
                        Refresh::Same
                    }
                    Some(p) if same_database(p, &fp) => Refresh::Same,
                    Some(p) => Refresh::Changed {
                        pinned: p.clone(),
                        seen: fp,
                    },
                }
            }
            Err(e) => {
                s.next_due = Some(now + RETRY_AFTER_FAILURE);
                s.last_failed = true;
                Refresh::Failed(e.to_string())
            }
        }
    }

    /// The last reading.
    pub fn current(&self) -> Option<Snapshot> {
        self.lock().reading.as_ref().map(|(r, _)| r.clone())
    }

    /// The last reading's fingerprint.
    pub fn current_fingerprint(&self) -> Option<String> {
        self.lock().reading.as_ref().map(|(r, _)| r.fingerprint())
    }

    /// The fingerprint pinned by the first read of the run.
    pub fn pinned(&self) -> Option<String> {
        self.lock().pinned.clone()
    }

    /// SMILE's clock now, extrapolated from the last read.
    pub fn pms_now(&self) -> Option<DateTime> {
        self.lock()
            .reading
            .as_ref()
            .map(|(r, at)| extrapolate(r.pms_now, *at))
    }

    /// Minutes since the last foreign change, at [`Tracker::pms_now`].
    pub fn foreign_age_minutes(&self) -> Option<i64> {
        let s = self.lock();
        let (r, at) = s.reading.as_ref()?;
        r.foreign_age_minutes_at(extrapolate(r.pms_now, *at))
    }
}

fn extrapolate(base: DateTime, at: Instant) -> DateTime {
    let ms = i64::try_from(at.elapsed().as_millis()).unwrap_or(i64::MAX);
    base.checked_add(dl_time::Duration::from_millis(ms))
        .unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_guid_on_one_side_is_the_same_database() {
        let a = "srv|db|2019-05-06T07:08:09.100";
        let b = "srv|db|2019-05-06T07:08:09.100|1b4e28ba-2fa1-11d2-883f-0016d3cca427";
        let c = "srv|db|2019-05-06T07:08:09.100|6f9619ff-8b86-d011-b42d-00c04fc964ff";
        assert!(same_ignoring_missing_guid(a, b) && same_ignoring_missing_guid(b, a));
        assert!(!same_ignoring_missing_guid(b, c), "two different GUIDs");
        assert!(
            !same_ignoring_missing_guid(b, b),
            "equal is decided by ==, not here"
        );
        assert!(same_database(b, b));
        assert!(!same_database(a, "srv|db2|2019-05-06T07:08:09.100"));
    }
}
