use std::path::PathBuf;

use super::*;
use crate::sql::legacy::{Found, Location};
use crate::store::testdir::TestDir;

fn found(product: &'static str, location: Location, conn: &str) -> Found {
    Found {
        product,
        label: "Old agent",
        location,
        file: PathBuf::from(format!("C:/x/{product}.json")),
        connection: conn.into(),
    }
}

fn scan() -> Scan {
    Scan {
        found: vec![
            found(
                "checkin",
                Location::InPlace,
                "Server=(local)\\SMILE;User ID=smile;Password=pw1",
            ),
            found(
                "pms",
                Location::InPlace,
                "Server=localhost\\smile;User ID=SMILE;Password=pw1",
            ),
            found(
                "einvoice",
                Location::BenchOld,
                "sqlserver://ei:pw2@db1?database=SMILE_FO",
            ),
        ],
        warnings: Vec::new(),
    }
}

/// Answers from a list of servers that work; counts the menu.
struct FakeTrial {
    works: Vec<&'static str>,
    tried: Vec<String>,
    asked: usize,
}

impl Trial for FakeTrial {
    fn test(&mut self, c: &Candidate) -> Result<(), String> {
        self.tried.push(format!("{}:{}", c.auth, c.server));
        if self.works.contains(&c.server.as_str()) {
            Ok(())
        } else {
            Err("Login failed for user (SQL error 18456)".into())
        }
    }
    fn ask(&mut self) -> Result<(), Failure> {
        self.asked += 1;
        Ok(())
    }
}

#[test]
fn candidates_keep_priority_order_and_drop_duplicates() {
    let c = candidates(
        &scan(),
        Some(("Server=.\\SMILE;User ID=smile;Password=pw1", "script")),
        Some(".\\SMILE"),
    );
    let got: Vec<(String, String, String)> = c
        .iter()
        .map(|c| (c.auth.clone(), c.server.clone(), c.user.clone()))
        .collect();
    assert_eq!(
        got,
        [
            ("sql".into(), "localhost\\SMILE".into(), "smile".into()),
            ("sql".into(), "db1".into(), "ei".into()),
            ("windows".into(), ".\\SMILE".into(), String::new()),
        ]
    );
    assert_eq!(c[0].source["product"].as_str(), Some("checkin"));
    assert_eq!(c[1].source["location"].as_str(), Some("bench_old"));
    assert_eq!(c[2].source["kind"].as_str(), Some("service_account"));
    assert!(!format!("{c:?}").contains("pw1"));
    assert!(!dl_json::to_string(&c[0].source).contains("pw1"));
}

#[test]
fn the_first_working_candidate_is_chosen_without_asking() {
    let d = TestDir::new("sql-auto-ok");
    let c = candidates(&scan(), None, Some("."));
    let mut t = FakeTrial {
        works: vec!["db1", "."],
        tried: Vec::new(),
        asked: 0,
    };
    assert_eq!(
        choose(&c, true, &mut t, &d.0).unwrap(),
        Chosen::Candidate(1)
    );
    assert_eq!(t.tried, ["sql:localhost\\SMILE", "sql:db1"]);
    assert_eq!(t.asked, 0);
    assert!(!d.0.join("reports.json").exists());
}

#[test]
fn none_working_asks_at_a_console() {
    let d = TestDir::new("sql-auto-ask");
    let c = candidates(&scan(), None, None);
    let mut t = FakeTrial {
        works: Vec::new(),
        tried: Vec::new(),
        asked: 0,
    };
    assert_eq!(choose(&c, true, &mut t, &d.0).unwrap(), Chosen::Asked);
    assert_eq!((t.tried.len(), t.asked), (2, 1));
    assert!(!d.0.join("reports.json").exists());
}

#[test]
fn none_working_unattended_fails_and_reports_without_secrets() {
    let d = TestDir::new("sql-auto-fail");
    let c = candidates(&scan(), None, Some("."));
    let mut t = FakeTrial {
        works: Vec::new(),
        tried: Vec::new(),
        asked: 0,
    };
    assert_eq!(choose(&c, false, &mut t, &d.0).unwrap(), Chosen::Failed);
    assert_eq!(t.asked, 0);
    let spool = std::fs::read_to_string(d.0.join("reports.json")).unwrap();
    assert!(spool.contains("sql_auto_failed"), "{spool}");
    assert!(spool.contains("18456"));
    assert!(spool.contains("legacy_agent"));
    assert!(!spool.contains("pw1") && !spool.contains("pw2"), "{spool}");
}

#[test]
fn servers_on_this_computer_are_one_server() {
    assert_eq!(normal_server("(local)\\SMILE"), ".\\smile");
    assert_eq!(normal_server("localhost,1433"), ".,1433");
    assert_eq!(normal_server("tcp:db1"), "db1");
}
