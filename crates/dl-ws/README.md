# dl-ws

WebSocket (RFC 6455) for blocking I/O: a frame codec, a connection type that works over any
`Read + Write` stream in the client or server role, and a client that connects to `ws://`
and `wss://` URLs through `dl-http` (proxies from the environment, OS TLS, deadlines). No
dependencies outside DevLibrary, no `unsafe`.

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| Go        | `nhooyr.io/websocket`, `github.com/coder/websocket`, `gorilla/websocket` (client side) |
| .NET      | `System.Net.WebSockets.ClientWebSocket` |
| Node.js   | `ws` (client) |
| Rust      | `tungstenite` (blocking) |

## Client

```rust,no_run
use std::time::Duration;
use dl_ws::{CloseFrame, ConnectOptions, Message};

let options = ConnectOptions {
    headers: vec![("Authorization".into(), "Bearer eyJhbGciOi...".into())],
    timeout: Some(Duration::from_secs(15)),
    ..Default::default()
};
let (mut ws, _response) =
    dl_ws::connect_with(&dl_http::Client::new(), "wss://api.example.com/hubs/order", &options)?;
ws.send_text(r#"{"protocol":"json","version":1}"#)?;

// Wake up every 200 ms to send queued work or a heartbeat; a timeout loses no data.
ws.set_read_timeout(Some(Duration::from_millis(200)))?;
loop {
    match ws.read() {
        Ok(Message::Text(t)) => println!("{t}"),
        Ok(Message::Close(_)) => break,
        Ok(_) => {} // pings are answered automatically
        Err(e) if e.is_timeout() => { /* send pending messages here */ }
        Err(e) => return Err(e),
    }
}
ws.close(Some(CloseFrame::normal()))?;
# Ok::<(), dl_ws::Error>(())
```

`WebSocket::shutdown_handle` returns a handle that closes the socket from another
thread, so a service can stop a reader blocked in `read`.

## Server-side framing

There is no HTTP server here. After your server has parsed the upgrade request, check it
and answer it, then wrap the stream:

```rust
let fields = [
    ("Upgrade", "websocket"),
    ("Connection", "Upgrade"),
    ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
    ("Sec-WebSocket-Version", "13"),
];
let accept = dl_ws::check_request("GET", &fields)?;
assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
// write "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n
//        Sec-WebSocket-Accept: {accept}\r\n\r\n", then:
// let ws = dl_ws::WebSocket::from_raw(stream, dl_ws::Role::Server, Default::default());
# Ok::<(), dl_ws::Error>(())
```

## Behaviour and limits

- Messages up to 16 MiB by default (`Config::max_message_size`, `max_frame_size`);
  larger ones close the connection with 1009. The declared length is checked before any
  payload is buffered.
- Fragmented messages are reassembled; control frames may be interleaved. Text is
  checked for UTF-8 (1007). Reserved bits or opcodes, non-minimal lengths, masked frames
  from a server (unmasked from a client), fragmented or oversized control frames and
  invalid close codes close the connection with 1002.
- Pings are answered with a pong before they are returned from `read`. A close frame
  from the peer is echoed with its status code; `close` starts the handshake from this
  end and `read` returns `Message::Close` when the peer answers.
- Each frame is written with one `write_all` and a flush. Client frames are masked with a
  key from the thread-local generator of `dl-rand` (seeded from the OS).
- Outgoing messages are sent as one frame. No extensions are offered, so
  `permessage-deflate` is never used; a server that selects an extension anyway fails the
  handshake.
- One thread at a time: the connection is not split into read and write halves (OpenSSL
  sessions cannot be used from two threads at once). Use read timeouts to interleave
  sends with reads.
- `connect` is not available on `wasm32-unknown-unknown`; the codec and `WebSocket` over
  your own stream are.
