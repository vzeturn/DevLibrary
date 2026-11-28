//! `master.upsert` (ADR 0147, 0151, 0152): the cloud writes one master row into SMILE_POS
//! only when the store row still hashes to the hash the editor read, and creates an outlet
//! row only when its key is absent. The allow-list ([`ENTITIES`]) is code and fails closed;
//! field values are never logged (names may be personal data). The verb itself runs in
//! `executor/master.rs`.

use crate::contracts::wire::site_command_reasons;

mod column;
mod combo;
mod entities;
mod key;
mod menu;
pub mod outlet_params;
mod store;
mod values;

pub use column::{ColumnKind, MasterColumn};
pub use combo::COMBO_LINE;
pub use entities::{
    CURRENCY, ENTITIES, ITEM_CLASS, OUTLET_PARAM, PAYMENT_MODE, SPECIAL_TAX, TABLE, TABLE_AREA,
    TAX_CLASS,
};
pub use key::{KeyKind, KeyValue, MasterKey, MasterKeyColumn};
pub use store::{COLUMNS_SQL, CREATE_LOCK_SQL, create_lock_resource};
pub(super) use store::{insert_row, read_row, update_row};
pub use values::{MAX_TEXT_LENGTH, TCVN3_ENCODE, encode_tcvn3};

/// A master entity the agent may write (`MasterEntity`): a code allow-list, every
/// statement a constant, so the SQL the grants allow is the SQL that runs.
#[derive(Debug, Clone, Copy)]
pub struct MasterEntity {
    /// Wire name of the entity.
    pub wire: &'static str,
    /// Catalog name of the vendor table (`dbo.<table>`).
    pub table: &'static str,
    /// The logical key, in payload order (ADR 0151, 0152): the payload key is the parts
    /// joined with `:`.
    pub key: &'static [MasterKeyColumn],
    /// Writable columns.
    pub writable: &'static [MasterColumn],
    /// Parameterized update: one parameter per key part and per writable column.
    pub update_sql: &'static str,
    /// A bit column that, when set, makes the row read-only for the cloud (base currency).
    pub guard_column: Option<&'static str>,
    /// Parameterized insert of a new row (same parameters as `update_sql`); `None` for an
    /// update-only entity.
    pub insert_sql: Option<&'static str>,
    /// Keys the cloud may never write (outlet parameters that steer numbering or the
    /// business date).
    pub is_protected: Option<fn(&MasterKey) -> bool>,
    /// The update writes every column as `COALESCE(@Col, Col)`, so a field the editor left
    /// out of an update is bound NULL and keeps the store value. A create still needs every
    /// field.
    pub keeps_missing: bool,
}

impl MasterEntity {
    /// `MasterEntity.SelectSql`: update-locks the row for the rest of the command
    /// transaction; the column list is added at run time. A create also holds the range
    /// (`HOLDLOCK`).
    pub fn select_sql(&self, column_list: &str, for_create: bool) -> String {
        format!(
            "SELECT {column_list} FROM dbo.{} WITH (UPDLOCK, {}) WHERE {};",
            self.table,
            if for_create { "HOLDLOCK" } else { "ROWLOCK" },
            self.key_predicate()
        )
    }

    /// `MasterEntity.KeyPredicate`: one equality per key part.
    pub fn key_predicate(&self) -> String {
        key::key_predicate(self.key)
    }

    /// `MasterEntity.KeyShape`: the key shape for an error message.
    pub fn key_shape(&self) -> String {
        key::key_shape(self.key)
    }

    /// `MasterEntity.TryParseKey`: the payload key, or `None` when the text is not a key of
    /// this entity.
    pub fn parse_key(&self, text: &str) -> Option<MasterKey> {
        MasterKey::parse(self.key, text)
    }

    /// Whether the cloud may never write `key`.
    pub fn protects(&self, key: &MasterKey) -> bool {
        self.is_protected.is_some_and(|f| f(key))
    }
}

/// Rejection reasons of `master.upsert` (`MasterReasons`).
pub mod master_reasons {
    /// The entity is not in the allow-list.
    pub const UNSUPPORTED_ENTITY: &str = "master_unsupported_entity";
    /// A field is not writable for the entity.
    pub const FIELD_NOT_ALLOWED: &str = "master_field_not_allowed";
    /// A create was requested for an update-only entity.
    pub const CREATE_UNSUPPORTED: &str = "master_create_unsupported";
    /// The row is not in the store.
    pub const ROW_NOT_FOUND: &str = "master_row_not_found";
    /// A create named a key the store already holds (the mirror had not seen it yet);
    /// nothing was written.
    pub const ROW_EXISTS: &str = "master_row_exists";
    /// A create found no template row (a `Para` name no outlet has, a store without any
    /// table); nothing was written.
    pub const NO_TEMPLATE: &str = "master_create_no_template";
    /// The row is read-only for the cloud (the base currency, a protected outlet
    /// parameter); nothing was written.
    pub const ROW_PROTECTED: &str = "master_row_protected";
    /// The row changed on the site since the editor read it.
    pub const CHANGED: &str = super::site_command_reasons::MASTER_CHANGED;
}

/// The allow-listed entity of a wire name (ordinal).
pub fn find_entity(wire: &str) -> Option<&'static MasterEntity> {
    ENTITIES.iter().copied().find(|e| e.wire == wire)
}
