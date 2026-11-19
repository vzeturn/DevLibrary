# dl-http

A small blocking HTTP/1.1 client with URL parsing, keep-alive pooling, deadlines,
`HTTP(S)_PROXY` support and TLS through `dl-tls`, plus a sans-I/O message layer (request
serialisation, response head parser, chunked decoder) that servers can reuse. No
dependencies outside DevLibrary, no `unsafe`.

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| Go        | `net/http` client (`http.Client`, `http.NewRequest`, `http.ProxyFromEnvironment`), `net/url` |
| .NET      | `HttpClient` for simple JSON calls, `System.Uri` |
| Node.js   | `axios` / `node-fetch` for simple JSON calls |
| Rust      | `ureq`, `url`, `httparse` |

## Client

```rust,no_run
use dl_http::{Client, Request, Url};

let client = Client::new(); // 30 s timeout, proxy from the environment
let resp = client.get("https://api.example.com/v1/health")?;
println!("{} {}", resp.status, resp.text()?);

// POST JSON with headers and a shorter timeout for this call.
let url = Url::parse("https://api.example.com/v1/invoices")?;
let req = Request::post(url, br#"{"id":42}"#.to_vec())
    .with_header("Authorization", "Bearer eyJhbGciOi...")?
    .with_header("X-Device-ID", "agent-01")?
    .with_header("Content-Type", "application/json")?
    .with_timeout(std::time::Duration::from_secs(10));
let resp = client.request(&req)?;
if !resp.is_success() {
    eprintln!("server said {}: {}", resp.status, String::from_utf8_lossy(&resp.body));
}

// Shortcut for the common case.
let resp = client.post_json("https://api.example.com/v1/ping", b"{}")?;
assert!(resp.status.as_u16() < 600);
# Ok::<(), dl_http::Error>(())
```

## Options, proxies and timeouts

```rust,no_run
use std::time::Duration;
use dl_http::{Client, ClientOptions, Proxy, Url};

let client = Client::with_options(ClientOptions {
    timeout: Some(Duration::from_secs(30)),        // whole request, incl. body
    connect_timeout: Some(Duration::from_secs(5)), // DNS and each TCP connect
    max_body: 2 * 1024 * 1024,                     // BodyTooLarge above 2 MiB
    proxy: Proxy::Http(Url::parse("http://user:pass@proxy.hotel.lan:3128")?),
    user_agent: Some("my-agent/1.4".into()),
    max_redirects: 5,                              // GET/HEAD only
    ..Default::default()
});
match client.get("https://api.example.com/") {
    Ok(resp) => println!("{}", resp.status),
    Err(dl_http::Error::Timeout) => eprintln!("timed out"),
    Err(dl_http::Error::ProxyConnect { status: 407 }) => eprintln!("proxy wants credentials"),
    Err(e) => eprintln!("{e}"),
}
# Ok::<(), dl_http::Error>(())
```

- `Proxy::FromEnv` (default) reads `HTTPS_PROXY` / `https_proxy` for `https` and
  `HTTP_PROXY` / `http_proxy` for `http` URLs, and `NO_PROXY` / `no_proxy` (`*`, host
  suffixes, `.subdomain-only`, `host:port`, IPs, CIDR ranges). `localhost` and loopback
  addresses are always direct. `HTTP_PROXY` is ignored when `REQUEST_METHOD` is set (CGI,
  "httpoxy").
- `Proxy::Rules(ProxyRules)` holds settings read once: `ProxyRules::from_windows(server,
  bypass)` takes the Windows Internet Options / `netsh winhttp` form (`host:port` or
  `http=…;https=…`, `ProxyOverride` with `*` wildcards, `:port` and `<local>` for dotless
  names), `ProxyRules::from_env` a snapshot of the variables above. `with_credentials`
  adds Basic credentials to a proxy whose URL has none; `describe()` never shows them.
- `http` URLs go to the proxy in absolute form (`GET http://host/path HTTP/1.1`);
  `https` URLs use a `CONNECT host:443` tunnel and TLS end to end. Credentials in the proxy
  URL become `Proxy-Authorization: Basic ...`.
- The overall deadline is enforced by setting the socket read/write timeout to the time
  left before every read and write; DNS runs on a helper thread so it is bounded too.

## Protocol upgrades

`Client::upgrade` sends a request on a new connection (same proxy, TLS and deadline
handling) and, on `101 Switching Protocols`, hands the connection over as an `Upgraded`
stream (`Read + Write`, with read/write timeouts and a cross-thread shutdown handle). Any
other status is returned with its body. `dl-ws` builds its WebSocket client on this.

## Sans-I/O layer

```rust
use dl_http::{BodyFraming, ChunkedDecoder, Method, parse_response_head, response_framing};

let wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
let (mut resp, used) = parse_response_head(wire)?.expect("complete head");
assert_eq!(response_framing(Method::Get, &resp)?, BodyFraming::Chunked);
let mut dec = ChunkedDecoder::new();
dec.decode(&wire[used..], &mut resp.body)?;
assert!(dec.is_done());
assert_eq!(resp.text()?, "hello");
# Ok::<(), dl_http::Error>(())
```

URLs:

```rust
use dl_http::{Url, encode_query_component};

let base = Url::parse("https://api.example.com/v1/")?;
let url = base.join(&format!("search?q={}", encode_query_component("hóa đơn")))?;
assert_eq!(url.to_string(), "https://api.example.com/v1/search?q=h%C3%B3a%20%C4%91%C6%A1n");
# Ok::<(), dl_http::Error>(())
```

## Limits and security notes

- Response heads: at most 64 KiB, 100 header fields and an 8 KiB status line. Obsolete
  line folding, bare CR, whitespace before the colon and control bytes are rejected.
- Bodies: `Content-Length` (one value, or repeated identical values), `chunked` (CRLF
  line endings, 4 KiB size lines, sizes checked for `u64` overflow, at most 100 trailer
  fields / 64 KiB) or read until close. `Transfer-Encoding` together with `Content-Length`
  is an error (request smuggling defence), as is any transfer coding other than a single
  `chunked`. Bodies above `max_body` fail with `BodyTooLarge` without buffering more than
  one extra read.
- Requests always carry a computed `Content-Length`; `Content-Length` and
  `Transfer-Encoding` set by the caller are ignored. Header values with CR/LF are
  rejected when added, so header injection is not possible.
- No `Accept-Encoding` is sent and no decompression is done.
- Redirects are off by default and only followed for `GET` / `HEAD`; `Authorization` and
  `Cookie` are dropped when a redirect leaves the origin.
- A pooled connection that fails before any response byte is retried once, on a fresh
  connection, only for idempotent methods or when the request was not fully written.
- `Debug` output redacts URL passwords and `Authorization`, `Proxy-Authorization`,
  `Cookie`, `Set-Cookie` values.
- Not supported: HTTP/2, `https://` or SOCKS proxies, cookies, streaming bodies,
  compression, `Expect: 100-continue`, internationalised domain names (use punycode),
  IPv6 zone IDs. A body delimited by connection close is accepted even if a TLS peer
  closes without `close_notify`.
- The client is not available on `wasm32-unknown-unknown`; URLs, headers, the message
  parser and proxy rules are.
