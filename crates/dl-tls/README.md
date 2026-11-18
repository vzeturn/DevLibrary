# dl-tls

A TLS 1.2/1.3 client over the operating system's TLS stack, for any `Read + Write`
stream. No dependencies and no bundled cryptography: certificates are checked against the
OS trust store, and security fixes arrive with OS updates.

| Platform | Backend | Versions |
|----------|---------|----------|
| Windows 10 1809+ / Server 2019+ | SChannel (`secur32.dll`) | TLS 1.2, TLS 1.3 where the OS supports it |
| Older Windows | SChannel, legacy credentials | TLS 1.2 |
| Linux and other non-Apple Unix | system OpenSSL 3 or 1.1, loaded at run time (`libssl.so.3`) | TLS 1.2, TLS 1.3 |
| macOS, WebAssembly | none yet: `connect` returns `Error::Unsupported` | |

TLS 1.0 and 1.1 are off unless `Config::min_version(Version::Tls10)` (or `Tls11`) asks
for them. They exist for servers that cannot do TLS 1.2, such as SQL Server 2008: use
them only as a fallback after a TLS 1.2 handshake failed. With OpenSSL 3 this also sets
the security level of that connection to 0, since level 1 and above refuse TLS 1.0/1.1
and SHA-1 signatures. The OS can still refuse them (on Windows, the SChannel
`Protocols\TLS 1.0\Client` registry key applies to every process). `tests/legacy_server.rs`
checks both outcomes against `openssl s_server -tls1` when `DL_TLS_TEST_TLS10_ADDR` is
set.

OpenSSL is opened with `dlopen` on first use, so programs build and link without OpenSSL
headers or libraries and report `Error::Unsupported` when it is missing at run time
(`apt install libssl3`).

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| Rust      | `rustls`, `native-tls`, `openssl`, `schannel` |
| Go        | `crypto/tls` (client side) |

## Example

```rust,no_run
use std::io::{Read, Write};
use std::net::TcpStream;
use dl_tls::{Config, TlsStream};

let tcp = TcpStream::connect(("example.com", 443))?;
let cfg = Config::new("example.com").alpn(&[b"http/1.1"]);
let mut tls = TlsStream::connect(&cfg, tcp)?;
println!("{:?} {:?}", tls.version(), tls.alpn_protocol());
tls.write_all(b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n")?;
let mut resp = Vec::new();
tls.read_to_end(&mut resp)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Verification

- `Verify::Full` (default): chain checked against the OS store, host name (or IP address)
  matched, validity period checked. SNI is sent for host names.
- `Verify::None`: any certificate is accepted. The traffic is encrypted but the server is
  not authenticated, so an active attacker on the network can read it. Use it only where
  the alternative is no encryption, such as SQL Server's `TrustServerCertificate=true` on a
  hotel LAN with a self-signed certificate.

Errors:

| Error | SChannel status | OpenSSL verify result |
|-------|-----------------|-----------------------|
| `UntrustedCertificate` | `SEC_E_UNTRUSTED_ROOT`, `CERT_E_UNTRUSTEDROOT`, `CERT_E_CHAINING`, `CERT_E_UNTRUSTEDCA` | 2, 18, 19, 20, 21 |
| `NameMismatch` | `SEC_E_WRONG_PRINCIPAL`, `CERT_E_CN_NO_MATCH` | 62, 64 |
| `ExpiredCertificate` | `SEC_E_CERT_EXPIRED`, `CERT_E_EXPIRED` | 9, 10 |
| `Handshake { code, message }` | any other status | other errors, with the OpenSSL error string |
| `Io(e)` | errors of the stream, unchanged (`TimedOut`, `UnexpectedEof`, ...) | same |

An invalid configuration (NUL in the server name, an empty name with `Verify::Full`, an
ALPN name that is empty or longer than 255 bytes) is `Io` with kind `InvalidInput`.

## Stream behaviour

Protocols that wrap TLS in their own framing rely on these guarantees:

- each handshake flight is written with one `write_all` followed by one `flush`;
- each `write` emits whole TLS records with one `write_all` (up to four records per call);
- after the handshake nothing is read from the stream unless `read` is called;
- `into_parts` ends TLS without `close_notify` and returns the stream plus any bytes read
  from it but not used. SQL Server's default "login-only" encryption depends on this: the
  client encrypts the login packet, then drops TLS and continues in clear text.
- socket read and write timeouts surface from `read` and `write` as `TimedOut` or
  `WouldBlock`, so callers can enforce deadlines;
- `read` returns `Ok(0)` after `close_notify` or a clean EOF between records, and
  `UnexpectedEof` when the connection ends inside a record.

`TlsStream<std::net::TcpStream>` is `Send`.

## Limits

- Client side only; no client certificates, session resumption control or custom roots.
- Windows: revocation is not checked (as with Go and most clients). With an unreachable
  revocation or AIA URL SChannel can wait during validation; set a read timeout on the
  socket.
- macOS (Security.framework) is planned.
- All `unsafe` code is in `src/schannel_sys.rs` and `src/openssl_sys.rs`.
