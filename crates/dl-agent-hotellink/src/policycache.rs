//! The last-known-good hub policy (Go `policycache.go`): `policy-cache.json` in the state
//! directory, so a restart applies the hub's policy before the hub answers.

use std::path::{Path, PathBuf};

use crate::jobmodel::{CancelPolicy, policy_from_value};

/// `policyCachePath`.
pub fn path(dir: &Path) -> PathBuf {
    dir.join("policy-cache.json")
}

/// `writePolicyCache` (atomic, owner-only).
pub fn write(dir: &Path, p: &CancelPolicy) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    crate::config::write_atomic(&path(dir), p.to_json().as_bytes())
}

/// `readPolicyCache`: `None` when missing or unreadable.
pub fn read(dir: &Path) -> Option<CancelPolicy> {
    let raw = std::fs::read(path(dir)).ok()?;
    let v = dl_json::parse_slice(&raw).ok()?;
    v.as_object()?;
    Some(policy_from_value(&v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobmodel::OpPolicy;
    use crate::jobqueue::tests::temp_dir;

    #[test]
    fn round_trip() {
        let dir = temp_dir("policy");
        let p = CancelPolicy {
            on_cancel: OpPolicy {
                allow_when_room_assigned: true,
                allow_when_checked_in: true,
                ..OpPolicy::default()
            },
            ..CancelPolicy::default()
        };
        write(&dir, &p).expect("write");
        assert_eq!(read(&dir), Some(p));
    }

    #[test]
    fn missing_or_broken_is_none() {
        let dir = temp_dir("policy-missing");
        assert_eq!(read(&dir), None);
        std::fs::write(path(&dir), "{oops").expect("write");
        assert_eq!(read(&dir), None);
    }

    #[test]
    fn reads_the_go_indented_file() {
        let dir = temp_dir("policy-go");
        std::fs::write(
            path(&dir),
            "{\n  \"onCancel\": {\n    \"allowWhenRoomAssigned\": true,\n    \"allowWhenAbnormalBookStatus\": false,\n    \"allowWhenCheckedIn\": false\n  },\n  \"onModify\": {\n    \"allowWhenRoomAssigned\": false,\n    \"allowWhenAbnormalBookStatus\": false,\n    \"allowWhenCheckedIn\": true\n  }\n}",
        )
        .expect("write");
        let p = read(&dir).expect("policy");
        assert!(p.on_cancel.allow_when_room_assigned && p.on_modify.allow_when_checked_in);
    }
}
