//! `CheckoutScanOptions` (`CheckoutScan` section) as `modules.membership` keys.
//!
//! ```text
//! checkout_scan_enabled (default: !read_only), checkout_interval_seconds (60),
//! checkout_max_per_scan (100), checkout_grace_days (1, 0 allowed),
//! walk_in_earn_enabled (true), walk_in_lookback_days (3), walk_in_max_per_scan (100)
//! ```
//!
//! The phone column is `guest_phone_column` (`CheckoutScan:GuestPhoneColumn`, shared with
//! the folio-observation scan). The .NET agent always runs the scan; here it follows
//! `read_only` unless set, because a read-only module runs beside the .NET service and two
//! agents would report every checkout twice.

use std::time::Duration;

use dl_json::Value;

/// `CheckoutScanOptions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutSettings {
    /// Whether the scan runs at all.
    pub enabled: bool,
    /// `IntervalSeconds`.
    pub interval: Duration,
    /// `MaxPerScan`: bookings asked of the hub per scan.
    pub max_per_scan: u32,
    /// `GraceDays`.
    pub grace_days: u32,
    /// `WalkInEarnEnabled`.
    pub walk_in_enabled: bool,
    /// `WalkInLookbackDays`.
    pub walk_in_lookback_days: i64,
    /// `MaxWalkInsPerScan`.
    pub max_walk_ins_per_scan: u32,
}

impl Default for CheckoutSettings {
    fn default() -> CheckoutSettings {
        CheckoutSettings {
            enabled: false,
            interval: Duration::from_secs(60),
            max_per_scan: 100,
            grace_days: 1,
            walk_in_enabled: true,
            walk_in_lookback_days: 3,
            max_walk_ins_per_scan: 100,
        }
    }
}

fn positive(v: &Value, key: &str, default: u32) -> u32 {
    v.get(key)
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(default)
}

impl CheckoutSettings {
    /// Reads the keys; absent or wrongly typed keys take the .NET default.
    pub fn from_config(v: &Value, read_only: bool) -> CheckoutSettings {
        let d = CheckoutSettings::default();
        CheckoutSettings {
            enabled: v
                .get("checkout_scan_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(!read_only),
            interval: Duration::from_secs(u64::from(positive(v, "checkout_interval_seconds", 60))),
            max_per_scan: positive(v, "checkout_max_per_scan", d.max_per_scan),
            grace_days: v
                .get("checkout_grace_days")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(d.grace_days),
            walk_in_enabled: v
                .get("walk_in_earn_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(d.walk_in_enabled),
            walk_in_lookback_days: i64::from(positive(v, "walk_in_lookback_days", 3)),
            max_walk_ins_per_scan: positive(v, "walk_in_max_per_scan", d.max_walk_ins_per_scan),
        }
    }
}

#[cfg(test)]
mod tests {
    use dl_json::json;

    use super::*;

    #[test]
    fn defaults_follow_checkout_scan_options_and_read_only() {
        let ro = CheckoutSettings::from_config(&Value::Null, true);
        assert!(!ro.enabled);
        assert_eq!(ro.interval, Duration::from_secs(60));
        assert_eq!((ro.max_per_scan, ro.grace_days), (100, 1));
        assert!(ro.walk_in_enabled);
        assert_eq!(
            (ro.walk_in_lookback_days, ro.max_walk_ins_per_scan),
            (3, 100)
        );
        assert!(CheckoutSettings::from_config(&Value::Null, false).enabled);
        let s = CheckoutSettings::from_config(
            &json!({"checkout_scan_enabled": true, "checkout_grace_days": 0,
                    "walk_in_earn_enabled": false, "checkout_max_per_scan": 0}),
            true,
        );
        assert!(s.enabled && !s.walk_in_enabled);
        assert_eq!((s.grace_days, s.max_per_scan), (0, 100));
    }
}
