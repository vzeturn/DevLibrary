//! Bootstrap records (port of `Bootstrap/BootstrapModels.cs`).

use std::collections::HashSet;
use std::fmt;

/// A reachable SQL Server instance, e.g. `.\SQLEXPRESS` or `host,1433` (`SqlInstance`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlInstance {
    /// The `Data Source` text.
    pub server_name: String,
    /// Where it was found: `registry`, `sql-browser`, `default`, `argument`.
    pub source: String,
}

impl SqlInstance {
    /// An instance.
    pub fn new(server_name: impl Into<String>, source: impl Into<String>) -> SqlInstance {
        SqlInstance {
            server_name: server_name.into(),
            source: source.into(),
        }
    }
}

impl fmt::Display for SqlInstance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.server_name)
    }
}

/// What a bootstrap step creates or grants (`StepKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepKind {
    /// The server login.
    Login,
    /// The database user.
    User,
    /// An agent-owned table.
    Table,
    /// Read access and the agent tables' writes.
    Grants,
    /// The narrow vendor-table writes of the site command executor (sync S9).
    CommandGrants,
    /// The vendor procedure and table grants of the order verbs (OW1b, ADR 0104).
    OrderGrants,
    /// The column grant of the covers verb (OW1c, ADR 0107).
    OrderGrantsOw1c,
    /// The grants of the table and multi-check verbs (OW1e, ADR 0112).
    OrderGrantsOw1e,
    /// The grants on the per-RVC check and line counters `dbo.LastCheck<rvc>` and
    /// `dbo.LastTrn<rvc>` that `dbo.GetRVCSeqNumber` reaches through dynamic SQL on some
    /// builds (RT-0654).
    SequenceGrants,
    /// The agent-owned `dbo.Pda_TxnApplied` with its `pda_sync` grant (ADR 0159).
    TxnApplied,
    /// `INSERT, DELETE` on the six bill tables for `txn.snapshot` (ADR 0159).
    TxnGrants,
    /// The nullable `StoreKey` column of `dbo.Pda_TxnApplied`, the identity a shift got in
    /// the store (ADR 0160, script 145; RT-1427 for this agent).
    TxnStoreKey,
    /// The `CashierShift`, `ShiftMoney` and `DailyCashLog` grants of the shift and cash
    /// verbs (ADR 0160, script 150; RT-1427 for this agent).
    TxnShiftGrants,
    /// The Front Office database user and folio grants of `txn.room_charge` (ADR 0162,
    /// script 160; RT-1427 for this agent); acts in the database `Para InterfaceDB` names.
    FoRoomChargeGrants,
    /// The SMILE_BO stock voucher grants of `txn.stock_deplete` / `txn.stock_restore` /
    /// `txn.stock_issue` (ADR 0164, script 175; RT-1427 for this agent).
    BoStockGrants,
    /// The SMILE_BO count session grants of `txn.stock_count` (ADR 0167, script 176).
    BoCountGrants,
    /// The SMILE_BO goods receipt grants of `txn.stock_receipt` (ADR 0169, script 177).
    BoReceiptGrants,
    /// The tender and close table writes of `order.tender_*` / `order.check_close`
    /// (ADR 0174, RT-1297).
    MoneyGrants,
    /// The read of `dbo.Folio` in the Front Office database by `fo.inhouse_search`
    /// (RT-1296, ADR 0172 §7).
    FoInhouseGrants,
    /// The Front Office reads and the posting procedure of the close and the room charge
    /// (`195_post_to_smile_grants.sql`, ADR 0179, RT-1297).
    PostToSmileGrants,
    /// The column grant of `order.po_number` on `dbo.tmpInvoice (PONumber)` (RT-1413,
    /// script 200; RT-1425 for this agent).
    PoNumberGrants,
    /// Full read/write of `pda_sync` on the POS database, `SMILE_FO` and `SMILE_BO`
    /// (owner decision RT-1425, temporary; script 210).
    FullRwGrants,
}

impl StepKind {
    /// The enum name (`StepKind.ToString()`), the `@kind` of the existence query.
    pub fn name(self) -> &'static str {
        match self {
            StepKind::Login => "Login",
            StepKind::User => "User",
            StepKind::Table => "Table",
            StepKind::Grants => "Grants",
            StepKind::CommandGrants => "CommandGrants",
            StepKind::OrderGrants => "OrderGrants",
            StepKind::OrderGrantsOw1c => "OrderGrantsOw1c",
            StepKind::OrderGrantsOw1e => "OrderGrantsOw1e",
            StepKind::SequenceGrants => "SequenceGrants",
            StepKind::TxnApplied => "TxnApplied",
            StepKind::TxnGrants => "TxnGrants",
            StepKind::TxnStoreKey => "TxnStoreKey",
            StepKind::TxnShiftGrants => "TxnShiftGrants",
            StepKind::FoRoomChargeGrants => "FoRoomChargeGrants",
            StepKind::BoStockGrants => "BoStockGrants",
            StepKind::BoCountGrants => "BoCountGrants",
            StepKind::BoReceiptGrants => "BoReceiptGrants",
            StepKind::MoneyGrants => "MoneyGrants",
            StepKind::FoInhouseGrants => "FoInhouseGrants",
            StepKind::PostToSmileGrants => "PostToSmileGrants",
            StepKind::PoNumberGrants => "PoNumberGrants",
            StepKind::FullRwGrants => "FullRwGrants",
        }
    }
}

/// One idempotent bootstrap action: an existence check by name, then the embedded script
/// (`BootstrapStep`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapStep {
    /// Step id (`login`, `user`, ...).
    pub id: &'static str,
    /// What it creates.
    pub kind: StepKind,
    /// The object the existence check looks for.
    pub object_name: &'static str,
    /// The C# manifest resource name (`Sql.010_login.sql`).
    pub resource_name: &'static str,
    /// Runs in `master` rather than the POS database.
    pub server_level: bool,
}

/// What happened to one step (`StepOutcome`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutcome {
    /// The step id.
    pub id: String,
    /// It already existed.
    pub skipped: bool,
    /// Its script ran.
    pub executed: bool,
}

/// The bootstrap of one database (`BootstrapReport`).
#[derive(Debug, Clone, PartialEq)]
pub struct BootstrapReport {
    /// The instance.
    pub instance: SqlInstance,
    /// The POS database.
    pub database: String,
    /// Nothing was changed.
    pub dry_run: bool,
    /// The steps in order.
    pub steps: Vec<StepOutcome>,
    /// The schema fingerprint score of the database.
    pub fingerprint_score: f64,
}

/// A set of table names compared ordinal-ignore-case (`HashSet<string>(OrdinalIgnoreCase)`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableSet(HashSet<String>);

fn fold(name: &str) -> String {
    name.chars().flat_map(char::to_uppercase).collect()
}

impl TableSet {
    /// An empty set.
    pub fn new() -> TableSet {
        TableSet::default()
    }

    /// Adds a name.
    pub fn insert(&mut self, name: &str) {
        self.0.insert(fold(name));
    }

    /// Removes a name.
    pub fn remove(&mut self, name: &str) {
        self.0.remove(&fold(name));
    }

    /// Whether the set holds `name` (ignoring case).
    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(&fold(name))
    }

    /// Number of names.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<S: AsRef<str>> FromIterator<S> for TableSet {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> TableSet {
        let mut s = TableSet::new();
        for n in iter {
            s.insert(n.as_ref());
        }
        s
    }
}
