//! Staff sync wire (`Wire/StaffSync.cs`, ADR 0101; `StaffJsonContext`).

use crate::contracts::json::{self, WireJson};
use crate::util::stj::{Obj, ReadError, Writer};

/// Body of `POST /sync/v1/staff`: the whole POS clerk list of the store.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StaffSnapshot {
    /// Agent id.
    pub agent_id: String,
    /// Agent version.
    pub agent_version: String,
    /// Fingerprint of the PIN key the lookups were made with (`StaffPin::key_id`).
    pub key_id: String,
    /// Clerks.
    pub clerks: Vec<StaffClerk>,
}

impl WireJson for StaffSnapshot {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("agent_id").string(&self.agent_id);
        w.key("agent_version").string(&self.agent_version);
        w.key("key_id").string(&self.key_id);
        w.key("clerks");
        json::write_items(w, &self.clerks);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(StaffSnapshot {
            agent_id: o.string("agent_id")?,
            agent_version: o.string("agent_version")?,
            key_id: o.string("key_id")?,
            clerks: o.list("clerks", json::item)?,
        })
    }
}

/// One POS clerk; never a credential in clear.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StaffClerk {
    /// `CashierDef.Initial`, trimmed.
    pub key: String,
    /// `CashierDef.FullName`, decoded and trimmed.
    pub name: String,
    /// False when the POS disabled the clerk.
    pub active: bool,
    /// Base64 of `HMAC-SHA256(K_t, TouchID)`; `None` unless the status is `set`.
    pub pin_lookup: Option<String>,
    /// One of [`staff_pin_status`].
    pub pin_status: String,
    /// `CashierDef.Security`.
    pub security: Option<i32>,
    /// RVCs of the clerk.
    pub rvcs: Vec<i32>,
    /// Allowed [`staff_function_ids::MAPPED`] ids.
    pub functions: Vec<String>,
}

impl WireJson for StaffClerk {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("key").string(&self.key);
        w.key("name").string(&self.name);
        w.key("active").bool(self.active);
        json::opt_str(w, "pin_lookup", &self.pin_lookup);
        w.key("pin_status").string(&self.pin_status);
        json::opt_int(w, "security", self.security.map(i64::from));
        w.key("rvcs");
        json::write_ints(w, &self.rvcs);
        w.key("functions");
        json::write_strings(w, &self.functions);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(StaffClerk {
            key: o.string("key")?,
            name: o.string("name")?,
            active: o.bool("active")?,
            pin_lookup: o.opt_string("pin_lookup")?,
            pin_status: o.string("pin_status")?,
            security: o.opt_int("security")?,
            rvcs: o.list("rvcs", json::int_item)?,
            functions: o.list("functions", json::string_item)?,
        })
    }
}

/// Why a clerk does or does not carry a PIN lookup.
pub mod staff_pin_status {
    /// A usable PIN was hashed.
    pub const SET: &str = "set";
    /// No PIN, or the clerk is disabled.
    pub const NONE: &str = "none";
    /// The POS PIN cannot be typed on the cloud keypad.
    pub const UNUSABLE: &str = "unusable";

    /// Whether the value is one of the above.
    pub fn is_known(value: Option<&str>) -> bool {
        matches!(value, Some(SET | NONE | UNUSABLE))
    }
}

/// The `ClerkPermission.FunctionID` ids the cloud role mapping reads.
pub mod staff_function_ids {
    /// Payment.
    pub const PAYMENT: &str = "PT_Payment";
    /// Cashier.
    pub const CASHIER: &str = "Cashier";
    /// Kitchen display.
    pub const KITCHEN_DISPLAY: &str = "SS_Kitchen";
    /// New check.
    pub const NEW_CHECK: &str = "PT_NewCheck";
    /// Pickup.
    pub const PICKUP: &str = "PT_Pickup";
    /// The ids sent.
    pub const MAPPED: &[&str] = &[PAYMENT, CASHIER, KITCHEN_DISPLAY, NEW_CHECK, PICKUP];
}

/// Limits of a snapshot.
pub mod staff_sync_limits {
    /// Most clerks.
    pub const MAX_CLERKS: usize = 5000;
    /// Longest key.
    pub const MAX_KEY_LENGTH: usize = 50;
    /// Longest name.
    pub const MAX_NAME_LENGTH: usize = 200;
    /// Most RVCs per clerk.
    pub const MAX_RVCS: usize = 200;
}

/// Routes.
pub mod staff_sync_routes {
    /// Snapshot upload.
    pub const SNAPSHOT: &str = "/sync/v1/staff";
    /// PIN key fetch.
    pub const PIN_KEY: &str = "/sync/v1/staff/pin-key";
}

/// Error codes.
pub mod staff_sync_error_codes {
    /// The lookups were made with a PIN key the cloud no longer uses.
    pub const KEY_STALE: &str = "SYNC_STAFF_KEY_STALE";
    /// The snapshot names another agent.
    pub const AGENT_MISMATCH: &str = "SYNC_STAFF_AGENT_MISMATCH";
}
