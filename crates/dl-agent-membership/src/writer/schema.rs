//! `SmileSchemaProbe`, `SmileTableSchema` and `SmileColumnList`: which optional columns
//! this SMILE install has (and how wide its text columns are), and the column lists the
//! writers build from that whitelist. Only whitelisted column names ever reach SQL text.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use dl_tds::{Param, ParamValue};

use super::session::{Session, nvarchar, p, text_of};
use crate::jobs::plan::WriteError;
use crate::log::{self, V};

/// `SmileSchemaProbe.OptionalColumns`.
pub const OPTIONAL_COLUMNS: &[(&str, &[&str])] = &[
    (
        "Folio",
        &[
            "BookStatus",
            "FirstName",
            "RoomTypeCode",
            "TelNumM",
            "MailM",
            "PaymentCode",
            "RateOverideFlag",
            "FolioCurrencyCode",
            "FolioExRate",
            "UpdateExRateAtCI",
            "NumRoom",
            "WalkInFlag",
            "ReservationTime",
            "ReservationClerkID",
            "LastChangeTime",
            "LastChangeClerkID",
            "CancelTime",
            "CancelClerkID",
            "NetRate",
            "Booker",
            "IncludeBF",
            "BreakfastCode",
            "ConfirmPrintFlag",
            "DscPcnt",
            "DscAmount",
            "ExtraBed",
            "NumEnf",
            "UserInfo3",
        ],
    ),
    (
        "FolioRate",
        &[
            "RateOverride",
            "CreateBy",
            "CreateTime",
            "DscPcnt",
            "DscAmount",
        ],
    ),
    (
        "AdditionalName",
        &[
            "ArrivalTime",
            "DepartureTime",
            "MobileNum",
            "Email",
            "BirthDate",
            "Gender",
            "NationalityCode",
            "AdtStatus",
            "IsChild",
            "VIPLevel",
            "ConfirmNum",
            "AOriginCode",
            "FolioSubNum",
        ],
    ),
    (
        "Folio_Special",
        &[
            "SpecialCode",
            "Qty",
            "Price",
            "FromDay",
            "ToDay",
            "SPNotes",
            "CancelNum",
        ],
    ),
    (
        "Profile",
        &[
            "PhoneNum",
            "Email",
            "BirthDate",
            "Gender",
            "CountryCode",
            "FirstName",
        ],
    ),
];

const COLUMNS_SQL: &str = "\
SELECT COLUMN_NAME AS Name, CHARACTER_MAXIMUM_LENGTH AS MaxLength
FROM INFORMATION_SCHEMA.COLUMNS
WHERE TABLE_NAME = @Table AND TABLE_SCHEMA IN (SCHEMA_NAME(), 'dbo')";

fn whitelist(table: &str) -> Option<&'static [&'static str]> {
    OPTIONAL_COLUMNS
        .iter()
        .find(|(t, _)| *t == table)
        .map(|(_, c)| *c)
}

/// `SmileTableSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
    table: &'static str,
    optional: Vec<&'static str>,
    lengths: HashMap<String, Option<i64>>,
}

impl TableSchema {
    /// A schema from the columns found (name, `CHARACTER_MAXIMUM_LENGTH`).
    pub fn from_columns(table: &'static str, found: &[(&str, Option<i64>)]) -> TableSchema {
        let mut lengths = HashMap::new();
        for (name, len) in found {
            lengths.entry(name.to_lowercase()).or_insert(*len);
        }
        let optional = whitelist(table)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|c| lengths.contains_key(&c.to_lowercase()))
            .collect();
        TableSchema {
            table,
            optional,
            lengths,
        }
    }

    /// `Table`.
    pub fn table(&self) -> &'static str {
        self.table
    }

    /// `Has`: the optional column exists on this install.
    pub fn has(&self, column: &str) -> bool {
        self.optional.contains(&column)
    }

    /// `IsWhitelisted`.
    pub fn is_whitelisted(&self, column: &str) -> bool {
        whitelist(self.table).is_some_and(|c| c.contains(&column))
    }

    /// `MaxLength`: the declared width of a text column (`None` for `max` or non-text).
    pub fn max_length(&self, column: &str) -> Option<usize> {
        self.lengths
            .get(&column.to_lowercase())
            .copied()
            .flatten()
            .filter(|n| *n > 0)
            .and_then(|n| usize::try_from(n).ok())
    }

    /// The missing optional columns, for the one-time warning.
    fn missing(&self) -> Vec<&'static str> {
        whitelist(self.table)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|c| !self.has(c))
            .collect()
    }
}

/// `SmileSchemaProbe`: one `INFORMATION_SCHEMA` read per table, cached for the life of the
/// writer (one writer serves one SMILE database).
#[derive(Debug, Default)]
pub struct SchemaProbe {
    cache: Mutex<HashMap<&'static str, Arc<TableSchema>>>,
}

impl SchemaProbe {
    /// `GetAsync`.
    pub fn get(
        &self,
        s: &mut dyn Session,
        table: &'static str,
    ) -> Result<Arc<TableSchema>, WriteError> {
        if whitelist(table).is_none() {
            return Err(WriteError::Coded {
                code: "SCHEMA_TABLE_NOT_WHITELISTED".into(),
                message: format!("Bảng '{table}' không nằm trong whitelist dò schema."),
            });
        }
        if let Some(hit) = self.lock().get(table) {
            return Ok(hit.clone());
        }
        let rows = s.rows(COLUMNS_SQL, &[p("@Table", nvarchar(Some(table)))])?;
        let mut found = Vec::with_capacity(rows.len());
        for r in &rows {
            found.push((
                r.opt_str("Name")?.unwrap_or_default(),
                r.opt_int("MaxLength")?,
            ));
        }
        let pairs: Vec<(&str, Option<i64>)> = found.iter().map(|(n, l)| (n.as_str(), *l)).collect();
        let schema = Arc::new(TableSchema::from_columns(table, &pairs));
        let mut cache = self.lock();
        if !cache.contains_key(table) {
            let missing = schema.missing();
            if !missing.is_empty() {
                log::warn(
                    "SMILE_FO table lacks optional columns; those booking fields are skipped",
                    &[
                        ("table", V::S(table)),
                        ("missing", V::S(&missing.join(","))),
                    ],
                );
            }
        }
        Ok(cache.entry(table).or_insert(schema).clone())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<&'static str, Arc<TableSchema>>> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `SmileUpdateMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `Overwrite`.
    Overwrite,
    /// `KeepExistingWhenNull`.
    KeepWhenNull,
    /// `FillBlankOnly`.
    FillBlank,
}

#[derive(Debug, Clone)]
struct Item {
    column: String,
    value_sql: String,
    mode: Mode,
    is_string: bool,
}

/// `SmileColumnList`.
#[derive(Debug)]
pub struct ColumnList<'a> {
    schema: &'a TableSchema,
    items: Vec<Item>,
    params: Vec<Param>,
    bad: Option<String>,
}

impl<'a> ColumnList<'a> {
    /// An empty list over `schema`.
    pub fn new(schema: &'a TableSchema) -> ColumnList<'a> {
        ColumnList {
            schema,
            items: Vec::new(),
            params: Vec::new(),
            bad: None,
        }
    }

    /// `IsEmpty`.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether a parameter `@column` was added.
    pub fn has_param(&self, column: &str) -> bool {
        let name = format!("@{column}");
        self.params.iter().any(|p| p.name() == name)
    }

    /// `Core`: a column every SMILE install has.
    pub fn core(mut self, column: &str, value: ParamValue) -> Self {
        self.params.push(p(&format!("@{column}"), value));
        self.items.push(Item {
            column: column.to_owned(),
            value_sql: format!("@{column}"),
            mode: Mode::Overwrite,
            is_string: false,
        });
        self
    }

    /// `CoreLiteral`.
    pub fn core_lit(mut self, column: &str, sql: &str) -> Self {
        self.items.push(Item {
            column: column.to_owned(),
            value_sql: sql.to_owned(),
            mode: Mode::Overwrite,
            is_string: false,
        });
        self
    }

    fn admit(&mut self, column: &str) -> bool {
        if !self.schema.is_whitelisted(column) {
            self.bad
                .get_or_insert_with(|| format!("{}.{column}", self.schema.table()));
            return false;
        }
        self.schema.has(column)
    }

    /// `Optional` with `Overwrite`.
    pub fn opt(self, column: &str, value: ParamValue) -> Self {
        self.opt_mode(column, value, Mode::Overwrite)
    }

    /// `Optional`: skipped when the install lacks the column, the value is `NULL` or a
    /// string is wider than the column (logged).
    pub fn opt_mode(mut self, column: &str, value: ParamValue, mode: Mode) -> Self {
        if !self.admit(column) {
            return self;
        }
        let mut value = value;
        if let (Some(s), Some(max)) = (text_of(&value), self.schema.max_length(column)) {
            let len = s.chars().count();
            if len > max {
                log::warn(
                    "SMILE_FO column is too narrow; value not written",
                    &[
                        ("table", V::S(self.schema.table())),
                        ("column", V::S(column)),
                        ("length", V::I(len as i64)),
                        ("max_length", V::I(max as i64)),
                    ],
                );
                value = ParamValue::Null;
            }
        }
        if super::session::is_null(&value) {
            return self;
        }
        let is_string = matches!(value, ParamValue::String(_) | ParamValue::VarChar(_));
        self.params.push(p(&format!("@{column}"), value));
        self.items.push(Item {
            column: column.to_owned(),
            value_sql: format!("@{column}"),
            mode,
            is_string,
        });
        self
    }

    /// `OptionalLiteral`.
    pub fn opt_lit(mut self, column: &str, sql: &str) -> Self {
        if !self.admit(column) {
            return self;
        }
        self.items.push(Item {
            column: column.to_owned(),
            value_sql: sql.to_owned(),
            mode: Mode::Overwrite,
            is_string: false,
        });
        self
    }

    /// Adds a key parameter that is not a column (`@FolioNum` of a `WHERE`).
    pub fn param(mut self, name: &str, value: ParamValue) -> Self {
        self.params.retain(|q| q.name() != name);
        self.params.push(p(name, value));
        self
    }

    fn check(&self) -> Result<(), WriteError> {
        match &self.bad {
            Some(col) => Err(WriteError::Coded {
                code: "SCHEMA_COLUMN_NOT_WHITELISTED".into(),
                message: format!("Cột '{col}' không nằm trong whitelist tuỳ chọn."),
            }),
            None => Ok(()),
        }
    }

    /// `InsertSql`.
    pub fn insert_sql(&self) -> Result<String, WriteError> {
        self.check()?;
        let cols: Vec<&str> = self.items.iter().map(|i| i.column.as_str()).collect();
        let vals: Vec<&str> = self.items.iter().map(|i| i.value_sql.as_str()).collect();
        Ok(format!(
            "INSERT INTO {} ({}) VALUES ({})",
            self.schema.table(),
            cols.join(", "),
            vals.join(", ")
        ))
    }

    /// `SetClause`.
    pub fn set_clause(&self) -> Result<String, WriteError> {
        self.check()?;
        let parts: Vec<String> = self
            .items
            .iter()
            .map(|i| {
                let (c, v) = (&i.column, &i.value_sql);
                match i.mode {
                    Mode::KeepWhenNull => format!("{c} = COALESCE({v}, {c})"),
                    Mode::FillBlank if i.is_string => format!(
                        "{c} = CASE WHEN LTRIM(RTRIM(ISNULL({c}, ''))) = '' THEN {v} ELSE {c} END"
                    ),
                    Mode::FillBlank => format!("{c} = COALESCE({c}, {v})"),
                    Mode::Overwrite => format!("{c} = {v}"),
                }
            })
            .collect();
        Ok(parts.join(", "))
    }

    /// The parameters, in the order they were added.
    pub fn params(&self) -> &[Param] {
        &self.params
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::session::{int, varchar};

    fn folio() -> TableSchema {
        TableSchema::from_columns(
            "Folio",
            &[
                ("FolioNum", None),
                ("FirstName", Some(5)),
                ("TelNumM", Some(20)),
            ],
        )
    }

    #[test]
    fn optional_columns_follow_the_install_and_its_widths() {
        let s = folio();
        let c = ColumnList::new(&s)
            .core("FolioNum", int(1))
            .opt("FirstName", varchar(Some("Too long")))
            .opt("TelNumM", nvarchar(Some("0900")))
            .opt("MailM", nvarchar(Some("x@y")))
            .opt_lit("LastChangeTime", "GETDATE()");
        assert_eq!(
            c.insert_sql().unwrap(),
            "INSERT INTO Folio (FolioNum, TelNumM) VALUES (@FolioNum, @TelNumM)"
        );
    }

    #[test]
    fn update_modes_render_like_dotnet() {
        let s = TableSchema::from_columns("Folio", &[("TelNumM", None), ("FirstName", None)]);
        let c = ColumnList::new(&s)
            .opt_mode("TelNumM", nvarchar(Some("1")), Mode::KeepWhenNull)
            .opt_mode("FirstName", varchar(Some("A")), Mode::FillBlank);
        assert_eq!(
            c.set_clause().unwrap(),
            "TelNumM = COALESCE(@TelNumM, TelNumM), FirstName = CASE WHEN LTRIM(RTRIM(ISNULL(FirstName, ''))) = '' THEN @FirstName ELSE FirstName END"
        );
    }

    #[test]
    fn a_column_outside_the_whitelist_fails_the_statement() {
        let s = folio();
        let c = ColumnList::new(&s).opt("Password", nvarchar(Some("x")));
        assert!(c.insert_sql().is_err());
    }
}
