//! `agent.json`: the non-secret enrollment result (port of `Config/AgentIdentity.cs`).

use super::paths::{AgentPaths, write_atomic};
use super::status::{ReadError, read_text};
use super::stj_read::{self, JsonError};
use crate::util::stj::{Style, Writer};
use std::io;

/// Non-secret enrollment result persisted next to the keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentity {
    /// The agent id the cloud assigned.
    pub agent_id: dl_uuid::Uuid,
    /// The tenant (empty when a hand-edited file has none).
    pub tenant_id: String,
    /// The cloud base URL.
    pub cloud_url: String,
    /// The machine id sent at enrollment.
    pub machine_id: String,
    /// The SQL Server instance, if known.
    pub sql_instance: Option<String>,
    /// The POS database, if known.
    pub sql_database: Option<String>,
}

impl AgentIdentity {
    /// The agent id as .NET's `Guid.ToString()` writes it (lowercase, hyphenated).
    pub fn agent_id_text(&self) -> String {
        self.agent_id.to_string()
    }

    /// The JSON System.Text.Json writes (Web defaults, indented, nulls omitted).
    pub fn to_json(&self) -> String {
        let mut w = Writer::new(Style::Indented);
        w.begin_object();
        w.key("agentId").string(&self.agent_id_text());
        w.key("tenantId").string(&self.tenant_id);
        w.key("cloudUrl").string(&self.cloud_url);
        w.key("machineId").string(&self.machine_id);
        w.opt_str_member("sqlInstance", self.sql_instance.as_deref());
        w.opt_str_member("sqlDatabase", self.sql_database.as_deref());
        w.end_object();
        w.finish()
    }

    /// Parses `agent.json` content; `Ok(None)` for `null`.
    pub fn from_json(text: &str) -> Result<Option<AgentIdentity>, JsonError> {
        let v = stj_read::parse(text)?;
        let Some(m) = stj_read::root_object(&v)? else {
            return Ok(None);
        };
        let agent_id = match stj_read::get_ci(m, "agentId") {
            None => dl_uuid::Uuid::NIL,
            Some(v) => stj_read::guid(v, "agentId")?,
        };
        let s = |name: &str| stj_read::opt_string(stj_read::get_ci(m, name), name);
        Ok(Some(AgentIdentity {
            agent_id,
            tenant_id: s("tenantId")?.unwrap_or_default(),
            cloud_url: s("cloudUrl")?.unwrap_or_default(),
            machine_id: s("machineId")?.unwrap_or_default(),
            sql_instance: s("sqlInstance")?,
            sql_database: s("sqlDatabase")?,
        }))
    }

    /// Reads `agent.json`; `Ok(None)` when it does not exist (or holds `null`).
    pub fn try_load(paths: &AgentPaths) -> Result<Option<AgentIdentity>, ReadError> {
        let file = paths.identity_file();
        if !file.is_file() {
            return Ok(None);
        }
        Ok(AgentIdentity::from_json(&read_text(&file)?)?)
    }

    /// Writes `agent.json` (creating the data and logs folders first).
    pub fn save(&self, paths: &AgentPaths) -> io::Result<()> {
        paths.ensure_created()?;
        write_atomic(&paths.identity_file(), &self.to_json())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::stj::NEWLINE;

    #[test]
    fn json_matches_system_text_json() {
        let id = AgentIdentity {
            agent_id: dl_uuid::Uuid::parse("5f0c1b9e-0000-4000-8000-000000000001").unwrap(),
            tenant_id: "tenant-1".into(),
            cloud_url: "https://x.example.com/".into(),
            machine_id: "PC-1".into(),
            sql_instance: None,
            sql_database: Some("SMILE_POS".into()),
        };
        let nl = NEWLINE;
        assert_eq!(
            id.to_json(),
            format!(
                "{{{nl}  \"agentId\": \"5f0c1b9e-0000-4000-8000-000000000001\",{nl}  \"tenantId\": \"tenant-1\",{nl}  \"cloudUrl\": \"https://x.example.com/\",{nl}  \"machineId\": \"PC-1\",{nl}  \"sqlDatabase\": \"SMILE_POS\"{nl}}}"
            )
        );
        assert_eq!(AgentIdentity::from_json(&id.to_json()).unwrap(), Some(id));
        let read = AgentIdentity::from_json(
            r#"{"AGENTID":"5F0C1B9E-0000-4000-8000-000000000001","tenantId":"t","cloudUrl":"u","machineId":"m","extra":1}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(read.agent_id_text(), "5f0c1b9e-0000-4000-8000-000000000001");
        assert_eq!(read.sql_instance, None);
        assert!(
            AgentIdentity::from_json(r#"{"agentId":"5f0c1b9e000040008000000000000001"}"#).is_err()
        );
    }
}
