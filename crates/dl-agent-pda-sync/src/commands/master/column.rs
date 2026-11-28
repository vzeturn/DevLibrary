//! The writable columns of a master entity (`MasterColumn`, `MasterColumnKind`): how each
//! value is validated and bound.

use dl_decimal::{Decimal, RoundingMode};

use super::values::{self, MAX_TEXT_LENGTH};

/// How a writable column is validated and bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    /// Printable ASCII of at most `max_length` characters, bound as text.
    Text,
    /// Unsigned decimal text with at most `scale` fraction digits and a value of at most
    /// `max`, bound as a typed `decimal(18, scale)` parameter, never as text.
    Numeric {
        /// Fraction digits of the column.
        scale: u32,
        /// Ceiling of the value.
        max: i64,
    },
    /// Exactly `0`, `1`, `true` or `false`, bound as `bit`.
    Bool,
    /// Integer text (optional leading `-`, digits only) from `min` to `max`, bound as `int`.
    Int {
        /// Lowest value.
        min: i32,
        /// Highest value.
        max: i32,
    },
    /// Text `dbo.Unicode2TCVN` stores unchanged, bound as `nvarchar` and written as
    /// `Col = dbo.Unicode2TCVN(@Col)` ([`master_tcvn3_set!`](crate::master_tcvn3_set)) into a
    /// TCVN3 `char`/`varchar` column.
    Tcvn3Text,
    /// Text without control characters, bound as `nvarchar` and written as `Col = @Col` into a
    /// real `nvarchar` column that capture reads without the TCVN3 decode.
    Unicode,
}

/// A writable master column: name and how its value is validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasterColumn {
    /// Column name, bound as the parameter of the same name.
    pub name: &'static str,
    /// Longest text value: characters for `Text`/`Tcvn3Text`, UTF-16 units for `Unicode`.
    pub max_length: usize,
    /// How the value is validated and bound.
    pub kind: ColumnKind,
}

impl MasterColumn {
    /// An ASCII text column.
    pub const fn text(name: &'static str, max_length: usize) -> Self {
        Self {
            name,
            max_length,
            kind: ColumnKind::Text,
        }
    }

    /// An unsigned decimal column (`decimal(18, scale)`), value at most `max`.
    pub const fn numeric(name: &'static str, scale: u32, max: i64) -> Self {
        Self {
            name,
            max_length: 18,
            kind: ColumnKind::Numeric { scale, max },
        }
    }

    /// A `bit` column.
    pub const fn boolean(name: &'static str) -> Self {
        Self {
            name,
            max_length: 5,
            kind: ColumnKind::Bool,
        }
    }

    /// An `int` column, value from `min` to `max`.
    pub const fn int(name: &'static str, min: i32, max: i32) -> Self {
        assert!(min <= max, "max must not be below min");
        Self {
            name,
            max_length: 11,
            kind: ColumnKind::Int { min, max },
        }
    }

    /// A TCVN3 column; the entity's update SQL writes it as
    /// [`master_tcvn3_set!`](crate::master_tcvn3_set)`("Col")`.
    pub const fn tcvn3(name: &'static str, max_length: usize) -> Self {
        Self {
            name,
            max_length: text_length(max_length),
            kind: ColumnKind::Tcvn3Text,
        }
    }

    /// A real `nvarchar` column, written as `Col = @Col`.
    pub const fn unicode(name: &'static str, max_length: usize) -> Self {
        Self {
            name,
            max_length: text_length(max_length),
            kind: ColumnKind::Unicode,
        }
    }

    /// Why `value` may not be written to this column; `None` when it may.
    pub fn problem(&self, value: &str) -> Option<String> {
        match self.kind {
            ColumnKind::Bool => values::bool_value(value)
                .is_none()
                .then(|| format!("{} must be 0, 1, true or false", self.name)),
            ColumnKind::Int { min, max } => values::int_value(value, min, max)
                .is_none()
                .then(|| format!("{} must be an integer from {min} to {max}", self.name)),
            ColumnKind::Tcvn3Text | ColumnKind::Unicode => values::text_problem(
                self.name,
                self.max_length,
                self.kind == ColumnKind::Tcvn3Text,
                value,
            ),
            ColumnKind::Text => {
                if value.chars().any(|ch| !(' '..='~').contains(&ch)) {
                    return Some(format!("{} must be ASCII", self.name));
                }
                (value.len() > self.max_length).then(|| {
                    format!(
                        "{} must be at most {} characters",
                        self.name, self.max_length
                    )
                })
            }
            ColumnKind::Numeric { scale, max } => self.decimal(value).is_none().then(|| {
                format!(
                    "{} must be an unsigned decimal with at most {scale} fraction digits and a value of at most {max}",
                    self.name
                )
            }),
        }
    }

    /// Strict decimal text (digits, optionally `.digits`; no sign, exponent, blanks or
    /// separators) within the column's scale and ceiling.
    pub fn decimal(&self, value: &str) -> Option<Decimal> {
        let ColumnKind::Numeric { scale, max } = self.kind else {
            return None;
        };
        let (whole, fraction) = value
            .split_once('.')
            .map_or((value, None), |(w, f)| (w, Some(f)));
        let digits = |t: &str| t.bytes().all(|b| b.is_ascii_digit());
        if whole.is_empty()
            || whole.len() > 12
            || !digits(whole)
            || fraction.is_some_and(|f| f.is_empty() || f.len() > scale as usize || !digits(f))
        {
            return None;
        }
        let parsed = Decimal::parse_rounded(value, RoundingMode::HalfEven).ok()?;
        (parsed <= Decimal::from(max)).then_some(parsed)
    }

    /// The value of a `Bool` column (`0`, `1`, `true`, `false`); `None` otherwise.
    pub fn bool_value(&self, value: &str) -> Option<bool> {
        (self.kind == ColumnKind::Bool)
            .then(|| values::bool_value(value))
            .flatten()
    }

    /// The value of an `Int` column within its range; `None` otherwise.
    pub fn int_value(&self, value: &str) -> Option<i32> {
        let ColumnKind::Int { min, max } = self.kind else {
            return None;
        };
        values::int_value(value, min, max)
    }

    /// Why `update_sql` does not write this column the way its kind needs; `None` when it
    /// does. A TCVN3 column is written only as `dbo.Unicode2TCVN(@Col)`; any other column
    /// never goes through that function.
    pub fn sql_problem(&self, update_sql: &str) -> Option<String> {
        values::sql_problem(self.name, self.kind == ColumnKind::Tcvn3Text, update_sql)
    }
}

/// A text column holds 1 to [`MAX_TEXT_LENGTH`] characters.
const fn text_length(max_length: usize) -> usize {
    assert!(
        max_length >= 1 && max_length <= MAX_TEXT_LENGTH,
        "a text column holds 1 to 4000 characters"
    );
    max_length
}
