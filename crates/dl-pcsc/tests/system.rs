//! Tests against the machine's PC/SC stack. Without a reader the Windows Smart Card
//! service is stopped and pcscd is usually missing, so these accept "no service" and
//! "library missing" outcomes and print which one happened.

use std::time::Duration;

use dl_pcsc::{Context, Error, PNP_NOTIFICATION, Protocols, ReaderWait, Scope, Share, State};

/// Establishes a context, or explains why none is available (`None`).
fn context() -> Option<Context> {
    match Context::establish(Scope::User) {
        Ok(ctx) => {
            println!("context established");
            Some(ctx)
        }
        Err(e) if e.is_no_service() => {
            println!("resource manager not running: {e}");
            None
        }
        Err(Error::Unsupported(reason)) if !cfg!(windows) => {
            // winscard.dll always exists on Windows; elsewhere libpcsclite may be missing.
            println!("PC/SC library missing: {reason}");
            None
        }
        Err(e) => panic!("unexpected establish error: {e}"),
    }
}

#[test]
fn library_loads() {
    match dl_pcsc::load_library() {
        Ok(()) => println!("PC/SC library loaded"),
        Err(e) if cfg!(windows) => panic!("winscard.dll must load: {e}"),
        Err(e) => println!("{e}"),
    }
}

#[test]
fn establish_and_list_readers() {
    let Some(ctx) = context() else { return };
    match ctx.list_readers() {
        Ok(readers) => println!("readers: {readers:?}"),
        Err(e) if e.is_no_service() => println!("list_readers: {e}"),
        Err(e) => panic!("unexpected list_readers error: {e}"),
    }
    match ctx.connect("dl-pcsc no such reader", Share::Shared, Protocols::ANY) {
        Ok(_) => panic!("connected to a reader that does not exist"),
        Err(e) => println!("connect to a missing reader: {e}"),
    }
}

#[test]
fn wait_times_out_or_reports_no_service() {
    let Some(ctx) = context() else { return };
    let readers = [ReaderWait::new(PNP_NOTIFICATION, State::UNAWARE)];
    match ctx.wait(&readers, Some(Duration::from_millis(50))) {
        Ok(states) => println!("wait: {states:?}"),
        Err(e) if e.is_timeout() || e.is_no_service() => println!("wait: {e}"),
        Err(e) => println!("wait (PnP pseudo reader not supported here?): {e}"),
    }
}

#[test]
fn cancel_from_another_thread() {
    let Some(ctx) = context() else { return };
    let canceller = ctx.canceller();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        canceller.cancel()
    });
    let mut readers = vec![ReaderWait::new(PNP_NOTIFICATION, State::UNAWARE)];
    // The first call returns the current PnP state at once; the second one blocks.
    let r = match ctx.wait(&readers, Some(Duration::from_secs(5))) {
        Ok(states) => {
            readers = states.iter().map(|s| s.to_wait()).collect();
            ctx.wait(&readers, Some(Duration::from_secs(5)))
        }
        Err(e) => Err(e),
    };
    let cancel_result = t.join().unwrap();
    println!("cancel: {cancel_result:?}, wait: {r:?}");
    match r {
        Err(e) if e.is_cancelled() || e.is_no_service() => {}
        // A reader plugged in during the test, or no PnP support.
        other => println!("wait ended otherwise: {other:?}"),
    }
    // Cancelling after the context is gone is a no-op.
    let late = ctx.canceller();
    drop(ctx);
    late.cancel().unwrap();
}

#[test]
fn wait_rejects_bad_input() {
    let Some(ctx) = context() else { return };
    assert!(matches!(
        ctx.wait(&[], None),
        Err(Error::InvalidArgument(_))
    ));
    let bad = [ReaderWait::new("a\0b", State::UNAWARE)];
    assert!(matches!(
        ctx.wait(&bad, None),
        Err(Error::InvalidArgument(_))
    ));
}

/// Needs a reader: set `DL_PCSC_READER` to its name (or a unique part of it), run with
/// `--ignored --nocapture`, then hold a card on the reader.
#[test]
#[ignore = "needs a PC/SC reader and a card; set DL_PCSC_READER"]
fn hardware_read_uid() {
    let wanted = std::env::var("DL_PCSC_READER").expect("set DL_PCSC_READER");
    let ctx = Context::establish(Scope::User).expect("establish");
    let readers = ctx.list_readers().expect("list_readers");
    println!("readers: {readers:?}");
    let reader = readers
        .iter()
        .find(|r| r.contains(&wanted))
        .unwrap_or_else(|| panic!("no reader matching {wanted:?}"));
    println!("waiting up to 30 s for a card on {reader}");
    let st = ctx
        .wait_for_card(reader, Some(Duration::from_secs(30)))
        .expect("wait_for_card");
    println!("state {:?}, ATR {:02X?}", st.state, st.atr);
    let card = ctx
        .connect(reader, Share::Shared, Protocols::ANY)
        .expect("connect");
    println!("protocol {:?}, status {:?}", card.protocol(), card.status());
    println!("UID {}", card.uid().expect("uid"));
}
