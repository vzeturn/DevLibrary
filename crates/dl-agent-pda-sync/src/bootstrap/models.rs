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
