//! `PmsRow`: one PMS row, column name to JSON value, looked up case-insensitively and
//! written in the order the columns were read.

use dl_json::{Map, Value};

use crate::stj;

/// A row (`Dictionary<string, JsonElement>` with `OrdinalIgnoreCase` keys).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PmsRow {
    entries: Vec<(String, Value)>,
}

impl PmsRow {
    /// An empty row.
    pub fn new() -> PmsRow {
        PmsRow::default()
    }

    /// Sets column `name`: an existing column with the same name in any case keeps its
    /// position and spelling and takes the new value (the dictionary indexer).
    pub fn insert(&mut self, name: impl Into<String>, value: Value) {
        let name = name.into();
        match self.entries.iter_mut().find(|(k, _)| stj::ci_eq(k, &name)) {
            Some(slot) => slot.1 = value,
            None => self.entries.push((name, value)),
        }
    }

    /// The value of column `name`, matched case-insensitively.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(k, _)| stj::ci_eq(k, name))
            .map(|(_, v)| v)
    }

    /// Columns in read order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Number of columns.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` without columns.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Reads a JSON object.
    ///
    /// # Errors
    /// When `v` is not an object.
    pub fn from_json(v: &Value) -> Result<PmsRow, String> {
        let map = v.as_object().ok_or("a row must be an object")?;
        let mut row = PmsRow::new();
        for (k, val) in map.iter() {
            row.insert(k.clone(), val.clone());
        }
        Ok(row)
    }

    /// The row as a JSON object (column order kept).
    pub fn to_json(&self) -> Value {
        let mut m = Map::with_capacity(self.entries.len());
        for (k, v) in &self.entries {
            m.insert(k.clone(), v.clone());
        }
        Value::Object(m)
    }

    /// Appends the row's JSON text as STJ writes it.
    pub fn write(&self, out: &mut String) {
        out.push('{');
        for (i, (k, v)) in self.entries.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            stj::escape_into(out, k);
            out.push(':');
            stj::write_value(out, v);
        }
        out.push('}');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_in_any_case_and_keep_their_place() {
        let mut r = PmsRow::new();
        r.insert("FolioNum", Value::from(1));
        r.insert("Name", Value::from("a"));
        r.insert("folionum", Value::from(2));
        assert_eq!(r.len(), 2);
        assert_eq!(r.get("FOLIONUM"), Some(&Value::from(2)));
        let mut s = String::new();
        r.write(&mut s);
        assert_eq!(s, r#"{"FolioNum":2,"Name":"a"}"#);
    }
}
