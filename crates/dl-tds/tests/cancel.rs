//! [`CancelToken`] against a real SQL Server: a cancel from another thread sends ATTENTION,
//! the running batch returns [`Error::Cancelled`] and the connection stays usable.
//!
//! Set `DL_TDS_TEST_URL` as for `integration.rs` (a disposable server; this test creates
//! nothing). Without it the test prints a note and passes.

use std::time::{Duration, Instant};

use dl_tds::{CancelToken, Client, Config, Error};

fn conn() -> Option<Client> {
    let url = match std::env::var("DL_TDS_TEST_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!("note: DL_TDS_TEST_URL is not set; skipping SQL Server integration test");
            return None;
        }
    };
    let cfg: Config = url
        .parse()
        .expect("DL_TDS_TEST_URL is not a valid connection string");
    Some(Client::connect(&cfg).expect("connect"))
}

fn select_five(c: &mut Client) -> Option<i32> {
    let r = c.query("SELECT 5", &[]).expect("SELECT 5");
    r.rows().first().and_then(|row| row.try_get::<i32>(0))
}

#[test]
fn cancel_token_sends_attention_and_connection_survives() {
    let Some(mut c) = conn() else { return };
    let token = CancelToken::new();
    c.set_cancel(Some(token.clone()));
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        token.cancel();
    });
    let start = Instant::now();
    assert!(matches!(
        c.batch("WAITFOR DELAY '00:00:10'; SELECT 1"),
        Err(Error::Cancelled)
    ));
    assert!(start.elapsed() < Duration::from_secs(5));
    stopper.join().unwrap();
    assert!(!c.is_broken());
    // The token stays cancelled: later requests fail the same way until it is replaced.
    assert!(matches!(c.batch("SELECT 1"), Err(Error::Cancelled)));
    c.set_cancel(None);
    assert_eq!(select_five(&mut c), Some(5));
}
