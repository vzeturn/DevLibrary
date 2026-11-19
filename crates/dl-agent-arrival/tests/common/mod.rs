//! Shared helpers for the integration tests: the .NET golden fixtures and small JSON
//! accessors. Not every test binary uses every helper.
#![allow(dead_code)]

pub mod cloud;

use dl_agent_arrival::stj;
use dl_json::Value;

/// `testdata/golden.json`, written by the .NET exporter.
pub fn golden() -> Value {
    stj::parse(include_str!("../../testdata/golden.json")).expect("golden.json parses")
}

/// One array of the fixture file.
pub fn cases<'a>(g: &'a Value, name: &str) -> &'a [Value] {
    g.as_object()
        .and_then(|m| m.get(name))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("fixture {name} missing"))
}

/// A field of a fixture object.
pub fn field<'a>(v: &'a Value, name: &str) -> &'a Value {
    v.as_object()
        .and_then(|m| m.get(name))
        .unwrap_or_else(|| panic!("field {name} missing"))
}

/// A string field (panics when absent or not a string).
pub fn s<'a>(v: &'a Value, name: &str) -> &'a str {
    field(v, name)
        .as_str()
        .unwrap_or_else(|| panic!("field {name} is not a string"))
}

/// A string-or-null field.
pub fn opt_s<'a>(v: &'a Value, name: &str) -> Option<&'a str> {
    match field(v, name) {
        Value::Null => None,
        other => Some(
            other
                .as_str()
                .unwrap_or_else(|| panic!("field {name} is not a string")),
        ),
    }
}

/// A boolean field.
pub fn b(v: &Value, name: &str) -> bool {
    field(v, name)
        .as_bool()
        .unwrap_or_else(|| panic!("field {name} is not a bool"))
}

/// An integer field.
pub fn i(v: &Value, name: &str) -> i64 {
    field(v, name)
        .as_i64()
        .unwrap_or_else(|| panic!("field {name} is not an integer"))
}

/// Lower-case hex decoding.
pub fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|k| u8::from_str_radix(&s[k..k + 2], 16).expect("hex"))
        .collect()
}

/// Strict base64 decoding.
pub fn unb64(s: &str) -> Vec<u8> {
    dl_agent_arrival::contracts::values::parse_base64(s).expect("base64")
}

/// A directory under the system temp dir, removed on drop.
#[derive(Debug)]
pub struct TempDir(pub std::path::PathBuf);

impl TempDir {
    /// A fresh, empty directory.
    pub fn new(tag: &str) -> TempDir {
        let p = std::env::temp_dir().join(format!(
            "dl-arrival-{tag}-{}-{:016x}",
            std::process::id(),
            dl_rand::u64()
        ));
        std::fs::create_dir_all(&p).expect("temp dir");
        TempDir(p)
    }

    /// The directory.
    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("could not remove {}: {e}", self.0.display());
        }
    }
}

/// `Rows.Folios`: one row per `(FolioNum, GuestName)`.
pub fn folios(items: &[(i32, &str)]) -> Vec<dl_agent_arrival::contracts::PmsRow> {
    items
        .iter()
        .map(|(folio, name)| {
            let mut row = dl_agent_arrival::contracts::PmsRow::new();
            row.insert("FolioNum", Value::from(*folio));
            row.insert("GuestName", Value::String((*name).to_string()));
            row
        })
        .collect()
}

/// A fresh random tenant key, base64.
pub fn new_tenant_key() -> String {
    dl_agent_arrival::contracts::values::to_base64(&dl_rand::secure_bytes::<32>().expect("random"))
}
