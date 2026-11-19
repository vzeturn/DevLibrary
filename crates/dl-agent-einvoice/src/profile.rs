//! The per-tenant collect profile: customer-specific collection choices the hub sets in
//! `GET /config` under `"collect_profile"`. Every key is opt-in and defaults to the
//! behaviour every site had before the key existed, so a missing or unknown value never
//! changes what a site collects. The shared SQL procs stay identical for everyone.

use dl_json::Value;

use crate::log::{self, V};

/// Where the POS line name comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PosItemName {
    /// The name the till printed (`Trn.TrnDesc`, what `sp_FetchPOS` returns). The default.
    #[default]
    Till,
    /// `ItemDef.IName1`, the Vietnamese item name, when the site fills it in.
    IName1,
}

/// The collect profile. New keys must default to the old behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CollectProfile {
    /// `pos_item_name`: `till` (default) or `iname1`.
    pub pos_item_name: PosItemName,
}

impl CollectProfile {
    /// Parse the `collect_profile` object of the hub config (`None`: key absent).
    /// A missing field or an unknown value falls back to the default; an unknown value
    /// is logged at warn.
    pub fn parse(v: Option<&Value>) -> CollectProfile {
        let mut p = CollectProfile::default();
        let Some(obj) = v else { return p };
        if let Some(raw) = obj.get("pos_item_name") {
            match raw.as_str() {
                Some("till") => {}
                Some("iname1") => p.pos_item_name = PosItemName::IName1,
                other => log::warn(
                    "collect_profile: unknown pos_item_name, using till",
                    &[("value", V::S(other.unwrap_or("<not a string>")))],
                ),
            }
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> CollectProfile {
        let v = dl_json::parse_slice(json.as_bytes()).unwrap();
        CollectProfile::parse(v.get("collect_profile"))
    }

    #[test]
    fn missing_key_or_field_is_till() {
        assert_eq!(parse("{}"), CollectProfile::default());
        assert_eq!(
            parse(r#"{"collect_profile":{}}"#),
            CollectProfile::default()
        );
        assert_eq!(
            parse(r#"{"collect_profile":null}"#),
            CollectProfile::default()
        );
    }

    #[test]
    fn known_values() {
        let p = parse(r#"{"collect_profile":{"pos_item_name":"iname1"}}"#);
        assert_eq!(p.pos_item_name, PosItemName::IName1);
        let p = parse(r#"{"collect_profile":{"pos_item_name":"till"}}"#);
        assert_eq!(p.pos_item_name, PosItemName::Till);
    }

    #[test]
    fn unknown_value_is_till() {
        for j in [
            r#"{"collect_profile":{"pos_item_name":"bogus"}}"#,
            r#"{"collect_profile":{"pos_item_name":7}}"#,
        ] {
            assert_eq!(parse(j).pos_item_name, PosItemName::Till);
        }
    }
}
