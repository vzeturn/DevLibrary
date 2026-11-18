# dl-signalr

A blocking client for ASP.NET Core SignalR hubs: the JSON hub protocol (version 1) over
WebSockets. Negotiate (with redirects) or a direct connection, access tokens, invoke with
results, fire-and-forget sends, server-to-client events, keepalive pings and the
server timeout. Built on `dl-ws`, `dl-http` and `dl-json`; no `unsafe`, no dependencies
outside DevLibrary.

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| .NET      | `Microsoft.AspNetCore.SignalR.Client` (`HubConnection`, JSON protocol) |
| Node.js   | `@microsoft/signalr` |
| Go        | `github.com/philippseith/signalr` (client side) |
| Rust      | `signalrs-client` |

## Usage

```rust,no_run
use std::time::Duration;
use dl_signalr::{ConnectOptions, HubConnection};

let mut options = ConnectOptions::new("https://pos.example.com/hubs/order");
options.access_token = Some("eyJhbGciOi...".into());
// Registered before connecting: also sees what the hub sends from OnConnectedAsync.
options.on("OrderPlaced", |args| println!("order {args:?}"));

let hub = HubConnection::connect(&options)?;
let joined = hub.invoke("JoinRvc", vec![5.into()], Some(Duration::from_secs(15)))?;
println!("joined: {joined}");
hub.send("Heartbeat", vec![])?; // no invocation id, no completion

// Handlers run on their own thread and may call back into the hub.
let inner = hub.clone();
hub.on("PrintRequested", move |args| {
    let job = args.first().cloned().unwrap_or_default();
    if let Err(e) = inner.invoke("AckPrintJob", vec![job, true.into()], None) {
        eprintln!("ack failed: {e}");
    }
});

println!("connection ended: {}", hub.wait_closed());
# Ok::<(), dl_signalr::Error>(())
```

Reconnecting is the caller's job; a loop with backoff:

```rust,no_run
use std::time::Duration;
use dl_signalr::{CloseReason, ConnectOptions, HubConnection};

let mut options = ConnectOptions::new("https://pos.example.com/hubs/order");
options.on("OrderPlaced", |args| println!("order {:?}", args));
let mut delay = Duration::ZERO;
loop {
    std::thread::sleep(delay);
    delay = (delay * 2).clamp(Duration::from_secs(1), Duration::from_secs(30));
    let hub = match HubConnection::connect(&options) {
        Ok(hub) => hub,
        Err(e) => {
            eprintln!("connect failed: {e}");
            continue;
        }
    };
    // Groups are per connection: join them again after every connect.
    if let Err(e) = hub.invoke("JoinRvc", vec![5.into()], Some(Duration::from_secs(15))) {
        eprintln!("join failed: {e}");
    }
    delay = Duration::ZERO;
    match hub.wait_closed() {
        CloseReason::Server { allow_reconnect: false, error } => {
            eprintln!("server closed the connection for good: {error:?}");
            break;
        }
        reason => eprintln!("{reason}, reconnecting"),
    }
}
```

The protocol codec has no I/O and works on every target, including
`wasm32-unknown-unknown` (where `HubConnection::connect` returns
`Error::Unsupported`):

```rust
use dl_signalr::protocol::{self, HubMessage, RecordReader};

let mut records = RecordReader::new(1 << 20);
records.push(b"{}\x1e{\"type\":1,\"target\":\"Notify\",\"arguments\":[1.10]}\x1e")?;
protocol::parse_handshake_response(records.next_record().unwrap())?;
let HubMessage::Invocation { target, arguments, .. } = protocol::parse(records.next_record().unwrap())? else {
    unreachable!()
};
assert_eq!(target, "Notify");
assert_eq!(arguments[0].to_string(), "1.10"); // numbers keep their exact text
# Ok::<(), dl_signalr::Error>(())
```

## Behaviour

- **Connecting.** With `negotiate: true` (default) the client posts to
  `{url}/negotiate?negotiateVersion=1` (the hub URL's query string is kept), follows
  `url`/`accessToken` redirects (up to 100, as the JavaScript client), requires the
  WebSockets transport with the text format, and connects to `ws(s)://...?id=<token>`.
  With `negotiate: false` it connects the WebSocket directly (`skipNegotiation`). The
  access token goes into `Authorization: Bearer` on negotiate and on the upgrade, and
  into the `access_token` query parameter (percent-encoded). `http` takes a shared
  `Arc<dl_http::Client>` for proxy and TLS settings.
- **Handshake.** `{"protocol":"json","version":1}` and the record separator; an `error`
  in the response fails `connect` with `Error::Handshake`. Messages that arrive in the
  same frame as the response are kept.
- **Framing.** Every message ends with `0x1E`. A WebSocket message may carry several hub
  messages and one hub message may span several WebSocket messages.
- **Invocations** carry an increasing `invocationId` and wait for their completion:
  the result, `Value::Null` for `void`, or `Error::Hub` with the server's error text.
  An invocation that times out is forgotten; a late completion is dropped. `send` has no
  invocation id, so the server reports nothing back.
- **Events.** Server-to-client invocations go to the handlers for the target, compared
  case-insensitively like ASP.NET Core. Handlers run one at a time, in order, on a
  dispatcher thread, never on the thread that reads the socket, so they may call
  `invoke`, `send` and `close`. A panicking handler is caught. A server invocation that
  asks for a result (client results) is answered with the error
  `Client didn't provide a result.`, as the .NET client does without a result handler.
- **Keepalive.** A ping is sent when nothing was sent for `keep_alive_interval`
  (15 s); ASP.NET Core enforces its `ClientTimeoutInterval` from the client's first
  ping on. The connection fails with `Error::ServerTimeout` when nothing arrives for
  `server_timeout` (30 s).
- **Closing.** A close message from the server ends the connection with
  `CloseReason::Server { error, allow_reconnect }`; a transport failure, a WebSocket
  closed without a close message, or a malformed message with
  `CloseReason::Transport(err)`; `close()` or dropping the last clone with
  `CloseReason::Local`. Pending invocations then fail with `Error::Closed`.
- **Numbers** in arguments and results are kept as their exact JSON text
  (`dl_json::Number::as_raw_str`), so money values and 64-bit ids survive.
- **Threads.** One thread owns the socket. It reads with a 20 ms timeout and sends
  queued messages in between, so an invoke waits at most about 20 ms before it is
  written. A second thread runs the handlers.

## Limits

- WebSockets only: no Server-Sent Events or long polling.
- JSON protocol only: no MessagePack.
- No streaming in either direction: a stream item for an invocation fails it with
  `Error::StreamingNotSupported` (ASP.NET Core itself refuses to stream to a normal
  invocation), and `StreamInvocation`/`CancelInvocation` messages are ignored.
- No client results (handlers return nothing), no stateful reconnect, no automatic
  reconnect.
- Blocking API with threads; there is no async interface.

## Tests

`cargo test -p dl-signalr` runs the codec tests and connection tests against an
in-process fake hub (negotiate responder plus server-side WebSocket framing from
`dl-ws`). `testdata/hub` is a minimal ASP.NET Core hub; with it running, the ignored
tests in `tests/aspnet.rs` check this client against the real server:

```text
dotnet run --project crates/dl-signalr/testdata/hub --urls http://127.0.0.1:5199
DL_SIGNALR_TEST_URL=http://127.0.0.1:5199/hub cargo test -p dl-signalr --test aspnet -- --ignored
```
