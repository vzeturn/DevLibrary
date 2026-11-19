//! Go `internal/discovery`: the user databases on a server.

use crate::Error;
use crate::sql::{Connector, Target};

/// The databases SQL Server ships with, never offered for backup schedules.
pub const SYSTEM_DATABASES: [&str; 4] = ["master", "model", "msdb", "tempdb"];

/// `names` without the system databases (Go `FilterSystemDatabases`, exact names).
pub fn filter_system(names: Vec<String>) -> Vec<String> {
    names
        .into_iter()
        .filter(|n| !SYSTEM_DATABASES.contains(&n.as_str()))
        .collect()
}

/// Every database on `target` (Go `ListDatabases`).
pub fn list_databases(sql: &dyn Connector, target: &Target) -> Result<Vec<String>, Error> {
    sql.connect(target)?.list_databases()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{FakeSql, SqlState};

    #[test]
    fn system_databases_are_filtered_out() {
        let names = ["master", "Sales", "model", "msdb", "tempdb", "HR", "Master"]
            .map(String::from)
            .to_vec();
        assert_eq!(filter_system(names), ["Sales", "HR", "Master"]);
    }

    #[test]
    fn lists_through_the_connector() {
        let db = FakeSql::new(SqlState::default());
        let all = list_databases(&db, &Target::default()).unwrap();
        assert_eq!(filter_system(all), ["Sales"]);
    }
}
