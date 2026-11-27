//! Cloud command channel and SQL command units (port of `Commands/`).
//!
//! - [`batches`]: the legacy SMILE_POS batches of the order and table verbs, verbatim
//!   (`OrderCommandSql.Batches*.cs`); [`sql_text`]: the agent's own SQL.
//! - [`SiteCommandStore`] / [`SiteCommandUnit`] / [`OrderCommandUnit`] /
//!   [`TableCommandUnit`]: the store seams; [`SqlSiteCommandStore`] implements them over
//!   `dl_tds` with the command log `dbo.Pda_SyncCommandLog`.
//! - [`TableSchemaGuard`]: the column probe of the table verbs.
//! - [`SiteCommandExecutor`]: runs each command once in the store's transaction;
//!   [`ResnapshotCommandHandler`] runs `sync.resnapshot`; [`VerifyCommandHandler`] runs
//!   `sync.verify` (ADR 0126/0128); [`FoInHouseSearchCommandHandler`] runs the read-only
//!   `fo.inhouse_search` (RT-1296).
//! - [`CommandChannel`]: WebSocket / long-poll link to the cloud; [`OrderGrantsMonitor`]:
//!   the grants self-check; [`service::services`]: the host's loop factory.

pub mod batches;
pub mod batches_money;
pub mod channel;
mod error;
mod executor;
pub mod fo_inhouse;
pub mod fo_inhouse_match;
pub mod grants;
pub mod grants_repair;
pub mod keyed_pool;
mod po_number;
mod resnapshot;
pub mod result_outbox;
mod schema_guard;
pub mod service;
mod sql;
mod sql_money;
mod sql_order;
mod sql_store;
mod sql_table;
pub mod sql_text;
mod units;
pub mod verify;
pub mod verify_manifest;

pub use channel::{
    ChannelError, CommandChannel, CommandChannelParts, next_failure_count, reconnect_delay,
};
pub use error::{CommandError, PERMISSION_DENIED, Result};
pub use executor::{
    CATEGORY as EXECUTOR_CATEGORY, CommandExecutor, GrantsCheck, RejectingCommandExecutor,
    SiteCommandExecutor, is_order_verb, reasons, runnable_order_types,
};
pub use fo_inhouse::{
    FoInHouseRead, FoInHouseSearchCommandHandler, FoInHouseSource, SqlFoInHouseSource,
};
pub use grants::{
    BootstrapGrantsRepair, GrantsRepair, OrderGrantsMonitor, OrderGrantsService, RepairAttempt,
    RepairOutcome,
};
pub use resnapshot::ResnapshotCommandHandler;
pub use schema_guard::{LogOnlyEvents, TableSchemaGuard, columns as schema_columns};
pub use service::{CommandExtras, CommandService};
pub use sql_store::{ConfigSource, SqlSiteCommandStore, SqlSiteCommandUnit};
pub use units::{
    CheckMoneyFacts, CommandWork, EmptyBillFacts, OrderCloseOutcome, OrderCommandUnit,
    OrderLineFacts, OrderPostOutcome, SiteCommandStore, SiteCommandUnit, StoreColumn,
    TableCommandUnit,
};
pub use verify::{VerifyCommandHandler, VerifyParts};
pub use verify_manifest::{
    CloudManifest, CloudManifestKeys, CloudManifestSource, HttpCloudManifestSource,
};

/// The parameter lists the SQL units bind (`SqlOrderCommandUnit.*Parameters`,
/// `SqlTableCommandUnit.*Parameters`), public so tests can hold each batch's free
/// `@variables` against them.
pub mod params {
    pub use super::sql_money::{
        check_close_parameters, room_charge_parameters, tender_add_parameters,
        tender_remove_parameters,
    };
    pub use super::sql_order::{
        COMBO_CHECK_NOT_FOUND, COMBO_ITEM_NOT_FOUND, COMBO_POSTED, OPEN_NAME_REQUIRED_ERROR,
        OPEN_PRICE_REQUIRED_ERROR, combo_parameters, covers_parameters, open_parameters,
        post_parameters, send_parameters, void_parameters, void_unsent_parameters,
    };
    pub use super::sql_store::reservation_parameters;
    pub use super::sql_table::{
        cancel_empty_parameters, cancel_parameters, empty_bill_facts_parameters, lock_parameters,
        log_trn_move_parameters, merge_line_info_parameters, merge_lines_parameters,
        missing_columns_parameters, move_line_info_parameters, move_line_parameters,
        po_number_parameters, table_info_parameters, transfer_header_parameters,
        transfer_line_info_parameters,
    };
}

#[cfg(test)]
mod tests;
