//! The SQL logins the old SmilePMS site agent left on this machine
//! (`%ProgramData%\SmilePMS\Agent\state\databases.json`, one DSN per database, in place or
//! renamed by the bench), for the installer to reuse (RT-0303).

use dl_agent::legacy_sql::{Candidate, copies, from_connection_string, machine_dirs};
use dl_json::Value;

/// The SMILE_* logins first, then the others, of every readable `databases.json`.
pub fn pms_legacy_sql() -> Vec<Candidate> {
    let mut out = Vec::new();
    for dir in machine_dirs(&["SmilePMS\\Agent"], &[])
        .iter()
        .flat_map(|d| copies(d))
    {
        let file = dir.join("state").join("databases.json");
        let doc = match dl_agent::store::read_json(&file) {
            Ok(Some(d)) => d,
            Ok(None) => continue,
            Err(e) => {
                eprintln!("Warning: {} is not readable: {e}", file.display());
                continue;
            }
        };
        out.extend(candidates(
            &doc,
            &format!("the old PMS site agent ({})", file.display()),
        ));
    }
    out
}

/// The DSNs of one `databases.json` (`{"SMILE_FO": "<dsn>", …}`), SMILE_* first.
pub fn candidates(doc: &Value, source: &str) -> Vec<Candidate> {
    let Some(map) = doc.as_object() else {
        return Vec::new();
    };
    let mut entries: Vec<(&str, &str)> = map
        .iter()
        .filter_map(|(k, v)| Some((k.as_str(), v.as_str()?)))
        .collect();
    entries.sort_by_key(|(k, _)| !k.to_ascii_uppercase().starts_with("SMILE_"));
    entries
        .into_iter()
        .filter_map(|(_, dsn)| from_connection_string(dsn, source))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_smile_databases_come_first() {
        let doc = dl_json::parse(
            r#"{"MM_PMS":"sqlserver://pms_agent:a@localhost:1433?database=MM_PMS",
                "SMILE_FO":"sqlserver://smile:b@localhost?database=SMILE_FO",
                "SMILE_POS":"Server=.;User ID=smile;Password=b;Database=SMILE_POS",
                "bad":"x=y"}"#,
        )
        .unwrap();
        let got: Vec<(String, String)> = candidates(&doc, "s")
            .into_iter()
            .map(|c| (c.user.clone(), c.password.expose().to_string()))
            .collect();
        assert_eq!(
            &got[..2],
            &[("smile".into(), "b".into()), ("smile".into(), "b".into())]
        );
        assert_eq!(
            got.last().unwrap(),
            &("pms_agent".to_string(), "a".to_string())
        );
    }
}
