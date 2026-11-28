//! The logical key of a master entity (ADR 0151, 0152; `MasterKeyColumn`, `MasterKeyKind`,
//! `MasterKey` of the .NET agent): ordered parts, the payload key is the parts joined with
//! `:`.

use std::fmt;

use dl_tds::{Param, ParamValue};

/// How a key part is typed (`MasterKeyKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// The outlet: decimal digits (no sign), bound as `int`.
    Outlet,
    /// An integer code: decimal digits (no sign), bound as `int`.
    Int,
    /// An ASCII code: printable ASCII of 1..`max_len` characters without `:` and without
    /// leading or trailing blanks (SQL Server ignores trailing blanks in `=`), bound as
    /// `varchar`.
    Str {
        /// Longest code.
        max_len: usize,
    },
}

/// One part of a master entity's logical key (`MasterKeyColumn`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasterKeyColumn {
    /// Vendor key column.
    pub column: &'static str,
    /// SQL parameter name (without `@`) the statements of the entity use for this part.
    pub param: &'static str,
    /// How the part is typed.
    pub kind: KeyKind,
    /// The column may be NULL and the mirror reads NULL as 0 (`null_key_defaults`, the
    /// currency outlet).
    pub null_as_zero: bool,
}

impl MasterKeyColumn {
    /// The outlet part (parameter `Rvc`).
    pub const fn outlet(column: &'static str) -> Self {
        Self {
            column,
            param: "Rvc",
            kind: KeyKind::Outlet,
            null_as_zero: false,
        }
    }

    /// The outlet part of a column the mirror reads NULL as 0 (`ISNULL(col, 0) = @Rvc`).
    pub const fn outlet_null_as_zero(column: &'static str) -> Self {
        Self {
            null_as_zero: true,
            ..Self::outlet(column)
        }
    }

    /// An integer code part (parameter `Key`).
    pub const fn int(column: &'static str) -> Self {
        Self {
            column,
            param: "Key",
            kind: KeyKind::Int,
            null_as_zero: false,
        }
    }

    /// An ASCII code part of at most `max_len` characters (parameter `Key`).
    pub const fn str(column: &'static str, max_len: usize) -> Self {
        Self {
            column,
            param: "Key",
            kind: KeyKind::Str { max_len },
            null_as_zero: false,
        }
    }

    /// The same part bound as another parameter (`Str("ItemMenu", 30, "Menu")` of the .NET
    /// agent), for a key with two string parts.
    pub const fn named(self, param: &'static str) -> Self {
        Self { param, ..self }
    }

    /// `MasterKeyColumn.Predicate`: the equality of this part.
    pub fn predicate(&self) -> String {
        if self.null_as_zero {
            format!("ISNULL({}, 0) = @{}", self.column, self.param)
        } else {
            format!("{} = @{}", self.column, self.param)
        }
    }

    /// `MasterKeyColumn.Parse`: the typed value of one key part; `None` when the text is not
    /// valid for the part.
    pub fn parse(&self, part: &str) -> Option<KeyValue> {
        match self.kind {
            KeyKind::Str { max_len } => {
                let ok = !part.is_empty()
                    && part.len() <= max_len
                    && part
                        .bytes()
                        .all(|b| (b' '..=b'~').contains(&b) && b != b':')
                    && !part.starts_with(' ')
                    && !part.ends_with(' ');
                ok.then(|| KeyValue::Str(part.to_owned()))
            }
            KeyKind::Outlet | KeyKind::Int => (!part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse::<i32>().ok())
            .flatten()
            .map(KeyValue::Int),
        }
    }

    /// The typed parameter of this part: `int` for outlet/integer parts, `varchar` for string
    /// parts (vendor keys are varchar).
    pub fn param_of(&self, value: &KeyValue) -> Param {
        match value {
            KeyValue::Int(n) => Param::new(self.param, *n),
            KeyValue::Str(s) => Param::new(self.param, ParamValue::VarChar(Some(s.clone()))),
        }
    }
}

/// The typed value of one key part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyValue {
    /// An outlet or integer code.
    Int(i32),
    /// An ASCII code.
    Str(String),
}

impl fmt::Display for KeyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyValue::Int(n) => write!(f, "{n}"),
            KeyValue::Str(s) => f.write_str(s),
        }
    }
}

/// A parsed payload key (`MasterKey`): the canonical text (integers without leading zeros)
/// and one typed value per key part, in the entity's key order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterKey {
    /// Canonical key text, the parts joined with `:`.
    pub text: String,
    /// One value per key part.
    pub values: Vec<KeyValue>,
}

impl MasterKey {
    /// Parses `text` against `parts`: as many `:`-separated parts as the key has, each valid
    /// for its kind (`MasterEntity.TryParseKey`).
    pub fn parse(parts: &[MasterKeyColumn], text: &str) -> Option<MasterKey> {
        let pieces: Vec<&str> = text.split(':').collect();
        if pieces.len() != parts.len() {
            return None;
        }
        let values = parts
            .iter()
            .zip(&pieces)
            .map(|(p, t)| p.parse(t))
            .collect::<Option<Vec<_>>>()?;
        let text = values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(":");
        Some(MasterKey { text, values })
    }

    /// The value of part `i` as an integer; `None` for a string part or out of range.
    pub fn int(&self, i: usize) -> Option<i32> {
        match self.values.get(i) {
            Some(KeyValue::Int(n)) => Some(*n),
            _ => None,
        }
    }

    /// The value of part `i` as text; `None` for an integer part or out of range.
    pub fn str(&self, i: usize) -> Option<&str> {
        match self.values.get(i) {
            Some(KeyValue::Str(s)) => Some(s),
            _ => None,
        }
    }
}

impl fmt::Display for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// `MasterEntity.KeyPredicate`: one equality per key part, joined with ` AND `.
pub fn key_predicate(parts: &[MasterKeyColumn]) -> String {
    parts
        .iter()
        .map(MasterKeyColumn::predicate)
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// `MasterEntity.KeyShape`: the key shape for an error message.
pub fn key_shape(parts: &[MasterKeyColumn]) -> String {
    let not_str = |p: &MasterKeyColumn| !matches!(p.kind, KeyKind::Str { .. });
    if let [only] = parts
        && not_str(only)
    {
        return format!("an integer {}", only.column);
    }
    let joined = parts
        .iter()
        .map(|p| format!("<{}>", p.column))
        .collect::<Vec<_>>()
        .join(":");
    if parts.iter().all(not_str) {
        format!("{joined} integers")
    } else {
        joined
    }
}
