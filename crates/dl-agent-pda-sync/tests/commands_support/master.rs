//! The fake's `master.upsert` rows (ADR 0147, 0151, 0152): `dbo.IClassDef` by code and
//! the other master tables by (wire entity, canonical key text).

use dl_agent_pda_sync::commands::Result;
use dl_agent_pda_sync::commands::master::{ColumnKind, KeyValue, MasterEntity, MasterKey};
use dl_agent_pda_sync::contracts::mapping::{RawValue, VendorRow};

use super::Unit;

/// The `dbo.IClassDef` row as capture reads it: `ClsName` as raw bytes.
pub fn item_class_row(key: i32, name: &str) -> VendorRow {
    let mut row = VendorRow::new();
    row.insert("ClsCode", RawValue::I32(key));
    row.insert("ClsName", RawValue::Bytes(name.as_bytes().to_vec()));
    row
}

impl Unit<'_> {
    pub(super) fn fake_read_master(
        &mut self,
        entity: &MasterEntity,
        key: &MasterKey,
        for_create: bool,
        exclude: &[String],
        bytes_columns: &[String],
    ) -> Result<Option<VendorRow>> {
        self.owner.call(format!(
            "master {} {} {key} exclude={} bytes={}",
            if for_create { "read-absent" } else { "read" },
            entity.wire,
            exclude.len(),
            bytes_columns.join("+")
        ));
        if entity.wire != "item_class" {
            return Ok(self
                .s
                .masters
                .get(&(entity.wire.to_owned(), key.text.clone()))
                .cloned());
        }
        let code = key.int(0).unwrap();
        Ok(self
            .s
            .item_classes
            .get(&code)
            .map(|name| item_class_row(code, name)))
    }

    pub(super) fn fake_update_master(
        &mut self,
        entity: &MasterEntity,
        key: &MasterKey,
        values: &[(&str, &str)],
    ) -> Result<bool> {
        self.owner
            .call(format!("master update {} {key}", entity.wire));
        if entity.wire != "item_class" {
            let Some(row) = self
                .s
                .masters
                .get_mut(&(entity.wire.to_owned(), key.text.clone()))
            else {
                return Ok(false);
            };
            write_master(entity, row, values);
            return Ok(true);
        }
        let Some(name) = self.s.item_classes.get_mut(&key.int(0).unwrap()) else {
            return Ok(false);
        };
        for (column, value) in values {
            assert_eq!(*column, "ClsName");
            *name = (*value).to_owned();
        }
        Ok(true)
    }

    pub(super) fn fake_insert_master(
        &mut self,
        entity: &MasterEntity,
        key: &MasterKey,
        values: &[(&str, &str)],
    ) -> Result<bool> {
        self.owner
            .call(format!("master insert {} {key}", entity.wire));
        let same_entity = |k: &(String, String)| k.0 == entity.wire;
        // Para takes its vendor-only columns from a row of the same name (ADR 0152): none,
        // nothing inserted.
        if entity.wire == "outlet_param" {
            let name = key.str(1).unwrap().to_ascii_lowercase();
            let known =
                self.s.masters.keys().any(|k| {
                    same_entity(k) && k.1.to_ascii_lowercase().ends_with(&format!(":{name}"))
                });
            if !known {
                return Ok(false);
            }
        }
        // A new table copies TableType from a table of the store: a store without any table
        // inserts nothing.
        if entity.wire == "table" && !self.s.masters.keys().any(same_entity) {
            return Ok(false);
        }
        let mut row = VendorRow::new();
        for (part, value) in entity.key.iter().zip(&key.values) {
            row.insert(
                part.column,
                match value {
                    KeyValue::Int(n) => RawValue::I32(*n),
                    KeyValue::Str(s) => RawValue::String(s.clone()),
                },
            );
        }
        write_master(entity, &mut row, values);
        self.s
            .masters
            .insert((entity.wire.to_owned(), key.text.clone()), row);
        Ok(true)
    }
}

/// Writes the sent values typed by column kind; a column left out of a keeps-missing update
/// keeps the store value (`COALESCE` in the real statement).
fn write_master(entity: &MasterEntity, row: &mut VendorRow, values: &[(&str, &str)]) {
    for (column, value) in values {
        let w = entity.writable.iter().find(|w| w.name == *column).unwrap();
        let v = match w.kind {
            ColumnKind::Numeric { .. } => RawValue::Decimal(w.decimal(value).unwrap()),
            ColumnKind::Int { .. } => RawValue::I32(w.int_value(value).unwrap()),
            ColumnKind::Bool => RawValue::Bool(w.bool_value(value).unwrap()),
            _ => RawValue::String((*value).to_owned()),
        };
        row.insert(*column, v);
    }
}
