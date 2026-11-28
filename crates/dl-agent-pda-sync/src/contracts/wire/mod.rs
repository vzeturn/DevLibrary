//! Wire DTOs and constants (port of `MM.PdaOrder.Sync.Contracts/Wire`). Every DTO of the
//! snake_case contexts implements [`WireJson`](crate::contracts::WireJson).

mod blobs;
mod envelope;
mod fingerprint;
mod fo_inhouse;
mod hops;
mod legacy_import;
mod money;
mod order;
mod pos_bills;
mod pos_order_residue;
mod pos_revenue;
mod print;
mod site_commands;
mod site_events;
mod staff_sync;
mod streams;
mod sync_verify;
mod table_commands;
mod txn_commands;

pub use blobs::{SyncBlobRef, SyncBlobRefBatch, SyncBlobRefsAck, blob_contract};
pub use envelope::{
    CommandStatus, StreamLag, StreamResnapshot, SyncBatch, SyncCommand, SyncCommandResult,
    SyncHeartbeat, SyncMode, SyncRow, SyncTombstone, wire_version,
};
pub use fingerprint::{CheckStateFingerprint, CheckStateHeader, CheckStateLine, check_aggregate};
pub use fo_inhouse::{FoInHouseGuest, FoInHouseSearchPayload, FoInHouseSearchResult};
pub use hops::{BatchHops, ResultHops, with_t_sent};
pub use legacy_import::{
    LegacyImportAck, LegacyImportBranchAck, LegacyImportHostPrinterAck, LegacyImportTableAck,
    legacy_import_wire,
};
pub use money::{
    OrderCheckClosePayload, OrderCheckCloseResult, OrderRoomChargePayload, OrderTenderAddPayload,
    OrderTenderRemovePayload, money_command_reasons, money_command_types,
};
pub use order::{
    OrderCheckLine, OrderCheckOpenPayload, OrderCheckResult, OrderComboPostPayload,
    OrderCoversPayload, OrderItemPostPayload, OrderItemVoidPayload, OrderSendPayload,
    order_aggregate, order_command_reasons, order_command_types,
};
pub use pos_bills::{
    PosBill, PosBillListPayload, PosBillListResult, PosBillPayment, pos_bill_statuses,
};
pub use pos_order_residue::{
    PosOrderResiduePayload, PosOrderResidueResult, PosResidueCheck, PosResidueInfo,
};
pub use pos_revenue::{PosRevenueDay, PosRevenueSummaryPayload, PosRevenueSummaryResult};
pub use print::{
    PrintQueueAutoFill, PrintQueueInfo, PrintQueueReport, PrintQueueReportResult,
    print_queue_kinds, print_queue_limits, print_spooler_address,
};
pub use site_commands::{
    KdsLineStatePayload, KdsLineStateResult, ReservationCancelPayload, ReservationSavePayload,
    ReservationSavedResult, SiteCommandDelivery, SiteCommandSpec, SyncResnapshotPayload,
    SyncResnapshotResult, kds_line_actions, site_command_contract, site_command_reasons,
    site_command_types,
};
pub use site_events::{
    SiteEvent, SiteEventBatch, site_event_levels, site_event_limits, site_event_sources,
};
pub use staff_sync::{
    StaffClerk, StaffSnapshot, staff_function_ids, staff_pin_status, staff_sync_error_codes,
    staff_sync_limits, staff_sync_routes,
};
pub use streams::sync_streams;
pub use sync_verify::{
    SyncVerifyCommandResult, SyncVerifyPayload, SyncVerifySkippedTable, SyncVerifyTableResult,
    sync_verify_repair,
};
pub use table_commands::{
    OrderCheckCancelEmptyPayload, OrderCheckExpectation, OrderCheckMergePayload,
    OrderItemsMovePayload, OrderMultiCheckResult, OrderPoNumberPayload, OrderTableChangePayload,
    OrderTableTransferPayload, is_local_timestamp, multi_check_aggregate, table_command_reasons,
    table_command_types,
};
pub use txn_commands::{
    TxnSnapshotPayload, TxnSnapshotResult, TxnTableRows, json_int64 as txn_json_int64,
    txn_command_types, txn_outcomes, txn_reasons, txn_scopes,
};
