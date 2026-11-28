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
//!   `fo.inhouse_search` (RT-1296); [`PosRevenueSource`] and [`PosBillSource`] answer the
//!   read-only `pos.revenue_summary` and `pos.bill_list` (RT-1427); [`master`] holds the
//!   `master.upsert` allow-list (ADR 0147) and [`CashierCommandSupport`] /
//!   [`CashierCommandUnit`] the seams of its entity `cashier` (ADR 0150); [`TxnCommandUnit`]
//!   is the seam of `txn.snapshot` (ADR 0159, RT-1427); [`TxnStockUnit`] the SMILE_BO seam
//!   of the stock verbs and [`PosStockSource`] the read of `pos.stock_balance` (RT-1427).
//! - [`CommandChannel`]: WebSocket / long-poll link to the cloud; [`OrderGrantsMonitor`]:
//!   the grants self-check; [`ChangeTrackingMonitor`]: the Change Tracking check run
//!   beside it behind the flag `capture.change_tracking` (RT-1318), one round of which
//!   is [`change_tracking_round`]; [`service::services`]: the host's loop factory.

pub mod batches;
pub mod batches_money;
mod cashier;
pub mod change_tracking;
#[cfg(test)]
pub(crate) mod change_tracking_fakes;
pub mod change_tracking_round;
pub mod channel;
mod error;
mod executor;
pub mod fo_inhouse;
pub mod fo_inhouse_match;
pub mod grants;
pub mod grants_repair;
pub mod keyed_pool;
pub mod master;
pub mod order_residue;
mod po_number;
pub mod pos_bills;
pub mod pos_revenue;
pub mod pos_stock;
mod resnapshot;
pub mod result_outbox;
mod schema_guard;
pub mod service;
mod sql;
mod sql_cashier;
mod sql_money;
mod sql_order;
mod sql_order_residue;
mod sql_stock;
mod sql_stock_post;
mod sql_stock_text;
mod sql_store;
mod sql_table;
pub mod sql_text;
mod sql_txn;
mod sql_txn_counters;
mod sql_txn_room_charge;
pub mod stock_unit;
mod txn;
mod txn_counters;
mod txn_room_charge;
mod txn_value;
mod units;
pub mod verify;
pub mod verify_manifest;

pub use cashier::{CashierCommandSupport, cashier_reasons};
pub use change_tracking::{ChangeTrackingMonitor, CtParts, CtState};
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
pub use order_residue::{
    OrderResidueSnapshot, OrderResidueSource, ResidueCheck, ResidueCleaned, ResidueInfoRow,
    ResidueReport,
};
pub use pos_bills::{POS_BILL_TENDERS, POS_BILLS, PosBillSource, pos_bill_parameters};
pub use pos_revenue::{DAILY_REVENUE, PosRevenueSource, daily_revenue_parameters};
pub use pos_stock::{PosStockRead, PosStockSource, stock_source_of};
pub use resnapshot::ResnapshotCommandHandler;
pub use schema_guard::{LogOnlyEvents, TableSchemaGuard, columns as schema_columns};
pub use service::{CommandExtras, CommandService};
pub use sql_store::{ConfigSource, SqlSiteCommandStore, SqlSiteCommandUnit};
pub use sql_txn::{
    COLUMNS_SQL as TXN_COLUMNS_SQL, LOCK_SQL as TXN_LOCK_SQL, SAVE_SQL as TXN_SAVE_SQL,
    SAVEPOINT as TXN_SAVEPOINT, delete_sql as txn_delete_sql, insert_sql as txn_insert_sql,
    select_sql as txn_select_sql,
};
pub use stock_unit::{
    StockApplied, StockCodeCount, StockCountDifference, StockCountLine, StockCountPosted,
    StockCountSession, StockError, StockHeldLine, StockIssueLine, StockIssuePosted,
    StockLedgerProfile, StockReceipt, StockReceiptLedger, StockReceiptLine, StockReceiptPosted,
    StockReceiptPostedLine, StockResult, StockSettings, StockStoreMap, StockVoucher,
    StockVoucherLine, TxnStockUnit,
};
pub use txn::{
    TxnAppliedState, TxnCommandUnit, aggregate_hash as txn_aggregate_hash,
    aggregate_line as txn_aggregate_line, convert_rows as txn_convert_rows,
};
pub use txn_counters::TxnCounterUnit;
pub use txn_room_charge::{FoActiveRow, FoFolio, FoPostLine, ROOM_CHARGE_CLERK, TxnRoomChargeUnit};
pub use txn_value::{
    TCVN3_MAX_LENGTH, TxnInsertValue, TxnLiveColumn, TxnTableWrite, TxnValue,
    max_chars as txn_max_chars, param as txn_param, try_convert as txn_try_convert,
};
pub use units::{
    CashierApply, CashierCommandUnit, CheckMoneyFacts, CommandWork, EmptyBillFacts,
    OrderCloseOutcome, OrderCommandUnit, OrderLineFacts, OrderPostOutcome, SiteCommandStore,
    SiteCommandUnit, StoreColumn, TableCommandUnit,
};
pub use verify::{VerifyCommandHandler, VerifyParts};
pub use verify_manifest::{
    CloudManifest, CloudManifestKeys, CloudManifestSource, HttpCloudManifestSource,
};

/// The parameter lists the SQL units bind (`SqlOrderCommandUnit.*Parameters`,
/// `SqlTableCommandUnit.*Parameters`), public so tests can hold each batch's free
/// `@variables` against them.
pub mod params {
    pub use super::fo_inhouse::DATABASE_ACCESS_SQL;
    pub use super::sql_cashier::{
        DELETE_ALL_RVCS_SQL, DELETE_RVCS_SQL, INSERT_COLUMNS, INSERT_RVC_SQL, PERMISSION_SQL,
        PERMISSION_UPSERT_SQL, PIN_TAKEN_SQL, RVC_SQL, SCHEMA_SQL, insert_sql, update_sql,
    };
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
    pub use super::sql_txn_counters::{
        LOCK_SQL as COUNTER_LOCK_SQL, OWN_OBJECT_SQL as COUNTER_OWN_OBJECT_SQL,
        RAISE_SQL as COUNTER_RAISE_SQL, backing as counter_backing,
        in_use_sql as counter_in_use_sql,
    };
    pub use super::sql_txn_room_charge::{
        ACTIVE_SQL as ROOM_CHARGE_ACTIVE_SQL, FOLIO_BY_NUMBER_SQL, FOLIOS_BY_ROOM_SQL, LINK_SQL,
        POST_SQL as ROOM_CHARGE_POST_SQL, post_parameters as room_charge_post_parameters,
        reverse_sql as room_charge_reverse_sql,
    };
}

#[cfg(test)]
mod tests;
