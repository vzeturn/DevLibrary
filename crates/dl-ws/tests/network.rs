//! `wss://` against a public echo server. Prints a note and passes when the network or
//! dl-tls is not available.

use std::time::Duration;

use dl_ws::{CloseFrame, ConnectOptions, Error, Message};

#[test]
fn wss_public_echo() {
    let options = ConnectOptions {
        timeout: Some(Duration::from_secs(20)),
        ..Default::default()
    };
    let (mut ws, _) = match dl_ws::connect_with(
        &dl_http::Client::new(),
        "wss://echo.websocket.org/",
        &options,
    ) {
        Ok(v) => v,
        Err(e @ (Error::Http(_) | Error::Io(_) | Error::Rejected { .. })) => {
            println!("note: skipping wss echo: {e}");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    ws.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    ws.send_text("dl-ws xin chào").unwrap();
    // The server may greet first ("Request served by ...").
    let mut got = None;
    for _ in 0..4 {
        match ws.read() {
            Ok(Message::Text(t)) if t == "dl-ws xin chào" => {
                got = Some(t);
                break;
            }
            Ok(_) => {}
            Err(e) if e.is_timeout() => {
                println!("note: skipping wss echo: no reply ({e})");
                return;
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(got.as_deref(), Some("dl-ws xin chào"));
    ws.close(Some(CloseFrame::normal())).unwrap();
    let _ = ws.read();
}
