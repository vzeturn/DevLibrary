# dl-tls

A TLS 1.2/1.3 client and server over the operating system's TLS stack, for any
`Read + Write` stream. No dependencies and no bundled cryptography: certificates are
checked against the OS trust store, and security fixes arrive with OS updates.

| Platform | Backend | Versions |
|----------|---------|----------|
| Windows 10 1809+ / Server 2019+ | SChannel (`secur32.dll`) | TLS 1.2, TLS 1.3 where the OS supports it |
| Older Windows | SChannel, legacy credentials | TLS 1.2 |
| Linux and other non-Apple Unix | system OpenSSL 3 or 1.1, loaded at run time (`libssl.so.3`) | TLS 1.2, TLS 1.3 |
| macOS, WebAssembly | none yet: `connect` and `accept` return `Error::Unsupported` | |

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
| Go        | `crypto/tls` |

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

## Server

`TlsStream::accept` runs the server side of the handshake over an accepted stream and
returns the same `TlsStream`. A `ServerConfig` holds the certificate chain (leaf first,
DER) and the leaf's private key (unencrypted PKCS#8 DER: ECDSA P-256 or RSA 2048+).

```rust,no_run
use std::io::{Read, Write};
use std::net::TcpListener;
use dl_tls::{PrivateKey, ServerConfig, TlsStream};

// PEM converts with `openssl x509 -outform der` and `openssl pkcs8 -topk8 -nocrypt -outform der`.
let chain = vec![std::fs::read("leaf.der")?, std::fs::read("intermediate.der")?];
let key = PrivateKey::from_pkcs8_der(&std::fs::read("key.p8.der")?)?;
let cfg = ServerConfig::new(chain, key).alpn(&[b"http/1.1"]);
let listener = TcpListener::bind("0.0.0.0:8443")?;
for tcp in listener.incoming() {
    let tcp = tcp?;
    let cfg = cfg.clone(); // clones share the OS credential
    std::thread::spawn(move || -> Result<(), dl_tls::Error> {
        let mut tls = TlsStream::accept(&cfg, tcp)?;
        println!("{:?} {:?} {:?}", tls.version(), tls.server_name(), tls.alpn_protocol());
        let mut req = [0u8; 1024];
        let n = tls.read(&mut req)?;
        println!("{n} bytes");
        tls.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")?;
        tls.shutdown()?;
        Ok(())
    });
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

- The OS credential is built on the first `accept` and reused by later connections and by
  clones of the `ServerConfig`. An empty chain or a certificate the OS cannot parse fails
  there with `Io` / `InvalidInput` before anything is read; OpenSSL also refuses a key that
  does not match the leaf there, SChannel only during the handshake.
- `server_name()` is the host name the client sent with SNI (`None` without SNI, and on
  client sessions). Every client gets the same certificate.
- ALPN: the server picks its most preferred protocol the client offers. A client that
  offers ALPN but none of the server's protocols gets the fatal `no_application_protocol`
  alert and `accept` returns `Handshake { code: 120, .. }` (RFC 7301 §3.2); a client that
  offers no ALPN is accepted without a protocol.
- SNI and ALPN are read from the ClientHello by dl-tls before the OS stack runs, so both
  backends behave the same; the bytes are then handed to the OS stack unchanged.
- `ServerConfig::min_version` works as `Config::min_version` (default TLS 1.2).
- Windows: the key is imported with CNG as an ephemeral key; when SChannel refuses it (it
  does its private-key work in LSASS), as a persisted key named `dl-tls-<pid>-<n>-<time>`
  in the user's key store, deleted when the credential is dropped. A process that is killed
  can leave one behind: `certutil -csp "Microsoft Software Key Storage Provider" -key -user`
  lists them and `-delkey` removes one. Intermediates of the chain are added to the
  machine's `CA` store (the user's when that is not allowed), because SChannel sends the
  chain it builds from the system stores; they stay there.
- `tests/server.rs` checks the server against the dl-tls client and, when installed, `curl`
  and `openssl s_client` validating the chain. The certificates and keys in `testdata/` are
  public and test-only; `testdata/gen.sh` regenerates them.

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

- No client certificates (on either side), session resumption control, custom roots or
  per-SNI certificates.
- Windows: revocation is not checked (as with Go and most clients). With an unreachable
  revocation or AIA URL SChannel can wait during validation; set a read timeout on the
  socket.
- macOS (Security.framework) is planned.
- All `unsafe` code is in `src/schannel_sys.rs`, `src/schannel_server_sys.rs` and
  `src/openssl_sys.rs`.
