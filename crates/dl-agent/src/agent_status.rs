//! `dl-agent status`: what this machine knows without asking the hub.

use std::path::Path;

use dl_json::{Map, Value, json};

use crate::config::ConfigStore;
use crate::identity::Identity;
use crate::report::Spool;
use crate::update::UpdateState;
use crate::{Version, current_target};

/// What `dl-agent status` prints: local state only, no hub call.
pub fn local_status(data_dir: &Path, product: &str, version: &Version) -> Value {
    let identity = match Identity::load(data_dir) {
        Ok(id) => {
            json!({"agent_id": id.agent_id, "tenant": id.tenant, "site": id.site, "hub_urls": id.hub_urls})
        }
        Err(e) => json!({"error": e.to_string()}),
    };
    let mut out = Map::new();
    out.insert("product", product);
    out.insert("version", version.to_string());
    out.insert("target", current_target());
    out.insert("data_dir", data_dir.display().to_string());
    out.insert("identity", identity);
    out.insert(
        "config_version",
        ConfigStore::load(data_dir).map_or(0, |c| c.current.version),
    );
    let update = UpdateState::load(data_dir).ok();
    out.insert(
        "update_pending",
        update
            .as_ref()
            .and_then(|u| u.pending.as_ref())
            .map(|p| p.version.to_string()),
    );
    out.insert(
        "failed_versions",
        update
            .map(|u| {
                u.failed_versions()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    out.insert(
        "spooled_reports",
        Spool::open(data_dir.join("reports.json")).len() as u64,
    );
    Value::Object(out)
}
