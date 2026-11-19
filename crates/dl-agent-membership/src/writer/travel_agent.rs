//! `TravelAgentReader.FindAsync`: the `ClientSts` + `CLIENT` row of a travel agent code.

use super::session::{Session, p};
use crate::jobs::plan::WriteError;
use crate::jobs::travel_agent::{TravelAgent, USER_DEFINE_COUNT};

const FIND_SQL: &str = "SELECT TOP 1
       S.ClientCode                              AS Code,
       ISNULL(C.ClientName, '')                  AS ClientName,
       CAST(ISNULL(C.Deleted, 0) AS bit)         AS IsDeleted,
       S.Origin, S.MSegment, S.MBusiness, S.MChanel, S.GEO1, S.GEO2,
       S.UDef1, S.UDef2, S.UDef3, S.UDef4, S.UDef5, S.UDef6, S.UDef7, S.UDef8
FROM ClientSts S
JOIN CLIENT C ON C.ClientFolioNum = S.ClientCode
WHERE S.ClientCode = @Code";

const UDEFS: [&str; USER_DEFINE_COUNT] = [
    "UDef1", "UDef2", "UDef3", "UDef4", "UDef5", "UDef6", "UDef7", "UDef8",
];

/// `Clean`: trailing blanks dropped (SMILE codes such as `'HU '`).
fn clean(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim_end().to_owned())
}

/// `FindAsync`: `None` when the code has no `ClientSts` row joined to `CLIENT`.
pub fn find(s: &mut dyn Session, code: i64) -> Result<Option<TravelAgent>, WriteError> {
    let rows = s.rows(FIND_SQL, &[p("@Code", dl_tds::ParamValue::I64(Some(code)))])?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    let mut user_defines = Vec::with_capacity(USER_DEFINE_COUNT);
    for c in UDEFS {
        user_defines.push(clean(r.opt_str(c)?));
    }
    Ok(Some(TravelAgent {
        code: r.int("Code")?,
        client_name: Some(
            r.opt_str("ClientName")?
                .unwrap_or_default()
                .trim_end()
                .to_owned(),
        ),
        is_deleted: r.opt_bool("IsDeleted")?.unwrap_or(false),
        origin: clean(r.opt_str("Origin")?),
        market_segment: clean(r.opt_str("MSegment")?),
        market_business: clean(r.opt_str("MBusiness")?),
        market_channel: clean(r.opt_str("MChanel")?),
        geographic1: clean(r.opt_str("GEO1")?),
        geographic2: clean(r.opt_str("GEO2")?),
        user_defines,
    }))
}

#[cfg(test)]
mod tests {
    use dl_tds::Value;

    use super::*;
    use crate::writer::recorder::Recorder;
    use crate::writer::session::Rec;

    #[test]
    fn a_found_agent_is_trimmed_and_a_missing_one_is_none() {
        let mut r = Recorder::default();
        assert_eq!(find(&mut r, 5).unwrap(), None);
        assert_eq!(r.log[0].params[0].1, "Int64");
        let mut cols = vec![
            ("Code", Value::I64(5)),
            ("ClientName", Value::String("Agency  ".into())),
            ("IsDeleted", Value::Bool(false)),
            ("Origin", Value::String("HU ".into())),
            ("MSegment", Value::Null),
            ("MBusiness", Value::Null),
            ("MChanel", Value::Null),
            ("GEO1", Value::Null),
            ("GEO2", Value::Null),
        ];
        for u in UDEFS {
            cols.push((u, Value::Null));
        }
        r.rows("FROM ClientSts", vec![Rec::new(cols)]);
        let ta = find(&mut r, 5).unwrap().unwrap();
        assert_eq!(ta.client_name.as_deref(), Some("Agency"));
        assert_eq!(ta.origin.as_deref(), Some("HU"));
        assert_eq!(ta.user_defines.len(), USER_DEFINE_COUNT);
    }
}
