//! `ProfileWriter` and `ProfileMemberKey`: the member's SMILE `Profile` row, keyed by
//! `ExternalProfileNumber` (the member code, or the legacy 12-character GUID prefix).

use dl_decimal::Decimal;
use dl_uuid::Uuid;

use super::applock::{self, DEFAULT_TIMEOUT_MS, PROFILE_NUMBER_LOCK};
use super::schema::{ColumnList, Mode, SchemaProbe, TableSchema};
use super::session::{Rec, Session, decimal, nvarchar, p, varchar};
use crate::jobs::guest::smile_gender;
use crate::jobs::payload::Contact;
use crate::jobs::plan::{ProfileRequest, WriteError};
use crate::log::{self, V};
use crate::smile::SmileError;

/// `ProfileMemberKey.MaxLength` (`Profile.ExternalProfileNumber` is `varchar(12)`).
pub const MEMBER_KEY_MAX_LENGTH: usize = 12;

const BY_EXTERNAL_SQL: &str = "SELECT TOP 1 ProfileNumber, COUNT(*) OVER () AS Total
FROM Profile WHERE ExternalProfileNumber = @Ext ORDER BY ProfileNumber";

/// `ProfileMemberKey`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberKey {
    /// `MemberId`.
    pub member_id: Uuid,
    /// `MemberCode` (trimmed, `None` when blank).
    pub member_code: Option<String>,
}

impl MemberKey {
    /// The key; a member code wider than the column is refused, never cut.
    pub fn new(member_id: Uuid, member_code: Option<&str>) -> Result<MemberKey, WriteError> {
        let code = member_code
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        if let Some(c) = &code
            && c.chars().count() > MEMBER_KEY_MAX_LENGTH
        {
            return Err(WriteError::Coded {
                code: "MEMBER_CODE_TOO_LONG".into(),
                message: format!(
                    "Mã hội viên '{c}' dài {} ký tự, vượt độ rộng Profile.ExternalProfileNumber ({MEMBER_KEY_MAX_LENGTH}) trên SMILE — không ghi để tránh bị cắt cụt thành khoá sai.",
                    c.chars().count()
                ),
            });
        }
        Ok(MemberKey {
            member_id,
            member_code: code,
        })
    }

    /// `LegacyKey`: the first 12 characters of the GUID.
    pub fn legacy_key(&self) -> String {
        self.member_id.to_string()[..MEMBER_KEY_MAX_LENGTH].to_owned()
    }

    /// `Key`.
    pub fn key(&self) -> String {
        self.member_code
            .clone()
            .unwrap_or_else(|| self.legacy_key())
    }

    /// `HasLegacyFallback`.
    pub fn has_legacy_fallback(&self) -> bool {
        self.member_code.is_some()
    }
}

struct Hit {
    number: Decimal,
    total: i64,
}

fn first_by_external(s: &mut dyn Session, ext: &str) -> Result<Option<Hit>, WriteError> {
    let rows = s.rows(BY_EXTERNAL_SQL, &[p("@Ext", varchar(Some(ext)))])?;
    rows.first().map(hit).transpose()
}

fn hit(r: &Rec) -> Result<Hit, WriteError> {
    Ok(Hit {
        number: r.dec("ProfileNumber")?,
        total: r.int("Total")?,
    })
}

/// `ContactColumns`.
fn contact_columns<'a>(
    schema: &'a TableSchema,
    first_name: &str,
    contact: Option<&Contact>,
    mode: Mode,
) -> ColumnList<'a> {
    let gender = smile_gender(contact.and_then(|c| c.gender.as_deref())).map(|g| g == 1);
    let first = Some(first_name.trim()).filter(|f| !f.is_empty());
    ColumnList::new(schema)
        .opt_mode("FirstName", nvarchar(first), mode)
        .opt_mode(
            "PhoneNum",
            nvarchar(contact.and_then(|c| c.phone.as_deref())),
            mode,
        )
        .opt_mode(
            "Email",
            nvarchar(contact.and_then(|c| c.email.as_deref())),
            mode,
        )
        .opt_mode(
            "BirthDate",
            dl_tds::ParamValue::DateTime(
                contact.and_then(|c| c.date_of_birth).map(|d| d.midnight()),
            ),
            mode,
        )
        .opt_mode("Gender", dl_tds::ParamValue::Bool(gender), mode)
        .opt_mode(
            "CountryCode",
            nvarchar(contact.and_then(|c| c.nationality.as_deref())),
            mode,
        )
}

/// `ProfileWriter`.
#[derive(Debug, Default)]
pub struct ProfileWriter {
    probe: SchemaProbe,
}

impl ProfileWriter {
    /// `LookupAsync`: (number, total, matched the legacy key).
    fn lookup(
        &self,
        s: &mut dyn Session,
        key: &MemberKey,
    ) -> Result<Option<(Hit, bool)>, WriteError> {
        if let Some(h) = first_by_external(s, &key.key())? {
            return Ok(Some((h, false)));
        }
        if !key.has_legacy_fallback() {
            return Ok(None);
        }
        Ok(first_by_external(s, &key.legacy_key())?.map(|h| (h, true)))
    }

    /// `FindByMemberAsync`.
    pub fn find_by_member(
        &self,
        s: &mut dyn Session,
        key: &MemberKey,
    ) -> Result<Option<Decimal>, WriteError> {
        Ok(self.lookup(s, key)?.map(|(h, _)| h.number))
    }

    /// `EnsureProfileAsync`: the member's `ProfileNumber`, issued when missing.
    pub fn ensure_profile(
        &self,
        s: &mut dyn Session,
        req: &ProfileRequest,
    ) -> Result<Decimal, WriteError> {
        let key = MemberKey::new(req.member_id, req.member_code.as_deref())?;
        if key.member_code.is_none() {
            log::warn(
                "job has no member code (old hub); the SMILE profile keeps the legacy key",
                &[
                    ("member", V::S(&key.member_id.to_string())),
                    ("legacy_key", V::S(&key.legacy_key())),
                ],
            );
        }
        if let Some((found, legacy)) = self.lookup(s, &key)? {
            if legacy {
                self.adopt_legacy(s, &key, &found)?;
            }
            match self.fill_blank(s, found.number, &req.first_name, req.contact.as_ref()) {
                Ok(()) => {}
                Err(WriteError::Smile(SmileError::Server(n))) => log::warn(
                    "could not fill the SMILE profile contact; keeping the existing profile number",
                    &[
                        ("profile", V::S(&found.number.to_string())),
                        ("sql", V::I(i64::from(n))),
                    ],
                ),
                Err(e) => return Err(e),
            }
            return Ok(found.number);
        }
        let schema = self.probe.get(s, "Profile")?;
        applock::in_lock(s, &[PROFILE_NUMBER_LOCK], DEFAULT_TIMEOUT_MS, None, |s| {
            if let Some(raced) = first_by_external(s, &key.key())? {
                return Ok(raced.number);
            }
            let rows = s.call("GetProfileNumber", &[])?.rows;
            let number = rows
                .first()
                .ok_or(WriteError::Smile(SmileError::Decode("ProfileNumber")))?
                .dec("ProfileNumber")?;
            let c = contact_columns(
                &schema,
                &req.first_name,
                req.contact.as_ref(),
                Mode::Overwrite,
            )
            .core("ProfileNumber", decimal(number))
            .core("ExternalProfileNumber", varchar(Some(&key.key())))
            .core("LastName", nvarchar(Some(&req.last_name)))
            .core_lit("FirstVisitDate", "GETDATE()")
            .core_lit("NumVisit", "0")
            .core_lit("NumNight", "0")
            .core_lit("TotalRevenue", "0");
            s.execute(&c.insert_sql()?, c.params())?;
            Ok(number)
        })
    }

    /// `AdoptLegacyAsync`: the legacy key becomes the member code (a no-op on retry).
    fn adopt_legacy(
        &self,
        s: &mut dyn Session,
        key: &MemberKey,
        found: &Hit,
    ) -> Result<(), WriteError> {
        s.execute(
            "UPDATE Profile SET ExternalProfileNumber = @Code WHERE ProfileNumber = @ProfileNumber AND ExternalProfileNumber = @Legacy",
            &[
                p("@Code", varchar(Some(&key.key()))),
                p("@Legacy", varchar(Some(&key.legacy_key()))),
                p("@ProfileNumber", decimal(found.number)),
            ],
        )?;
        log::info(
            "adopted SMILE profile: ExternalProfileNumber legacy key -> member code",
            &[
                ("profile", V::S(&found.number.to_string())),
                ("member", V::S(&key.member_id.to_string())),
            ],
        );
        if found.total > 1 {
            log::warn(
                "member still has duplicate SMILE profiles under the legacy key; the front desk should merge them",
                &[
                    ("member", V::S(&key.member_id.to_string())),
                    ("duplicates", V::I(found.total - 1)),
                    ("legacy_key", V::S(&key.legacy_key())),
                    ("profile", V::S(&found.number.to_string())),
                ],
            );
        }
        Ok(())
    }

    /// `FillBlankContactAsync`: only the blank contact cells of an existing profile.
    fn fill_blank(
        &self,
        s: &mut dyn Session,
        number: Decimal,
        first_name: &str,
        contact: Option<&Contact>,
    ) -> Result<(), WriteError> {
        let schema = self.probe.get(s, "Profile")?;
        let c = contact_columns(&schema, first_name, contact, Mode::FillBlank);
        if c.is_empty() {
            return Ok(());
        }
        let c = c.param("@ProfileNumber", decimal(number));
        s.execute(
            &format!(
                "UPDATE Profile SET {} WHERE ProfileNumber = @ProfileNumber",
                c.set_clause()?
            ),
            c.params(),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use dl_tds::Value;

    use super::*;
    use crate::writer::recorder::Recorder;
    use crate::writer::session::Called;

    const MEMBER: &str = "12345678-1234-1234-1234-123456789abc";

    fn req(code: Option<&str>) -> ProfileRequest {
        ProfileRequest {
            member_id: MEMBER.parse().unwrap(),
            member_code: code.map(str::to_owned),
            last_name: "Test".into(),
            first_name: " Member ".into(),
            contact: None,
        }
    }

    fn profile_schema(r: &mut Recorder) {
        r.rows(
            "INFORMATION_SCHEMA",
            ["FirstName", "PhoneNum", "Email"]
                .into_iter()
                .map(|c| {
                    Rec::new(vec![
                        ("Name", Value::String(c.into())),
                        ("MaxLength", Value::Null),
                    ])
                })
                .collect(),
        );
    }

    #[test]
    fn keys_follow_dotnet() {
        let k = MemberKey::new(MEMBER.parse().unwrap(), Some(" MB-000001 ")).unwrap();
        assert_eq!(k.key(), "MB-000001");
        assert_eq!(k.legacy_key(), "12345678-123");
        assert!(k.has_legacy_fallback());
        assert!(MemberKey::new(MEMBER.parse().unwrap(), Some("MB-0000000001")).is_err());
        let legacy = MemberKey::new(MEMBER.parse().unwrap(), Some("  ")).unwrap();
        assert_eq!(legacy.key(), "12345678-123");
    }

    #[test]
    fn a_new_member_gets_a_numbered_profile_under_the_applock() {
        let mut r = Recorder::default();
        profile_schema(&mut r);
        r.on("GetProfileNumber", |_| {
            Ok(Called {
                rows: vec![Rec::new(vec![(
                    "ProfileNumber",
                    Value::Decimal(Decimal::from(42i64)),
                )])],
                status: Some(0),
            })
        });
        let n = ProfileWriter::default()
            .ensure_profile(&mut r, &req(Some("MB-1")))
            .unwrap();
        assert_eq!(n, Decimal::from(42i64));
        let ins = r
            .log
            .iter()
            .find(|s| s.text.starts_with("INSERT INTO Profile"))
            .unwrap();
        assert_eq!(
            ins.text,
            "INSERT INTO Profile (FirstName, ProfileNumber, ExternalProfileNumber, LastName, FirstVisitDate, NumVisit, NumNight, TotalRevenue) VALUES (@FirstName, @ProfileNumber, @ExternalProfileNumber, @LastName, GETDATE(), 0, 0, 0)"
        );
        let texts = r.texts();
        let lock = texts.iter().position(|t| *t == "sp_getapplock").unwrap();
        let ins_at = texts
            .iter()
            .position(|t| t.starts_with("INSERT INTO Profile"))
            .unwrap();
        assert!(lock < ins_at);
    }

    #[test]
    fn a_legacy_profile_is_adopted_and_its_blanks_filled() {
        let mut r = Recorder::default();
        profile_schema(&mut r);
        r.on("ExternalProfileNumber = @Ext", |st| {
            let legacy = st.params[0].2.as_deref() == Some("12345678-123");
            Ok(Called {
                rows: if legacy {
                    vec![Rec::new(vec![
                        ("ProfileNumber", Value::Decimal(Decimal::from(7i64))),
                        ("Total", Value::I32(2)),
                    ])]
                } else {
                    Vec::new()
                },
                status: Some(0),
            })
        });
        let n = ProfileWriter::default()
            .ensure_profile(&mut r, &req(Some("MB-1")))
            .unwrap();
        assert_eq!(n, Decimal::from(7i64));
        let texts = r.texts();
        assert!(texts.iter().any(|t| t.starts_with(
            "UPDATE Profile SET ExternalProfileNumber = @Code WHERE ProfileNumber = @ProfileNumber AND ExternalProfileNumber = @Legacy"
        )));
        assert!(texts.iter().any(|t| t.starts_with(
            "UPDATE Profile SET FirstName = CASE WHEN LTRIM(RTRIM(ISNULL(FirstName, ''))) = '' THEN @FirstName ELSE FirstName END WHERE"
        )));
        assert!(!texts.contains(&"GetProfileNumber"));
    }

    #[test]
    fn a_fill_blank_sql_error_keeps_the_number() {
        let mut r = Recorder::default();
        profile_schema(&mut r);
        r.rows(
            "ExternalProfileNumber = @Ext",
            vec![Rec::new(vec![
                ("ProfileNumber", Value::Decimal(Decimal::from(9i64))),
                ("Total", Value::I32(1)),
            ])],
        );
        r.on("UPDATE Profile SET FirstName", |_| {
            Err(SmileError::Server(8152))
        });
        let n = ProfileWriter::default()
            .ensure_profile(&mut r, &req(Some("MB-1")))
            .unwrap();
        assert_eq!(n, Decimal::from(9i64));
    }
}
