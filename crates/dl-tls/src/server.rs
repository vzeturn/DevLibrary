//! The server side: the certificate chain and private key a server presents, and the work
//! done before the OS TLS stack sees a connection (reading the ClientHello for SNI and ALPN).

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, PoisonError};

use crate::client_hello::{self, Hello};
use crate::{Error, Version, alpn, imp, pkcs8};

/// The algorithm of a [`PrivateKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyAlgorithm {
    /// ECDSA on the NIST P-256 curve (`id-ecPublicKey` with `secp256r1`, RFC 5480).
    EcdsaP256,
    /// RSA (`rsaEncryption`, RFC 8017). Use 2048 bits or more.
    Rsa,
}

/// A server's private key.
///
/// The bytes are overwritten with zeros when the key is dropped, and [`fmt::Debug`] shows
/// only the algorithm.
pub struct PrivateKey {
    der: Vec<u8>,
    algorithm: KeyAlgorithm,
}

impl PrivateKey {
    /// Reads an unencrypted PKCS#8 `PrivateKeyInfo` (RFC 5208 §5; the
    /// `-----BEGIN PRIVATE KEY-----` PEM form once Base64-decoded) holding an ECDSA P-256 or
    /// an RSA key.
    ///
    /// Only the algorithm header is checked here; the OS reads the key material when the
    /// first connection is accepted. Malformed DER, encrypted PKCS#8, other algorithms and
    /// other curves are [`Error::Io`] with [`io::ErrorKind::InvalidInput`].
    pub fn from_pkcs8_der(der: &[u8]) -> Result<PrivateKey, Error> {
        let algorithm = pkcs8::algorithm(der).map_err(|m| Error::Io(alpn::invalid(m)))?;
        Ok(PrivateKey {
            der: der.to_vec(),
            algorithm,
        })
    }

    /// The key's algorithm.
    pub fn algorithm(&self) -> KeyAlgorithm {
        self.algorithm
    }

    #[cfg_attr(
        not(any(windows, all(unix, not(target_vendor = "apple")))),
        allow(dead_code)
    )]
    pub(crate) fn pkcs8_der(&self) -> &[u8] {
        &self.der
    }
}

impl Drop for PrivateKey {
    fn drop(&mut self) {
        self.der.fill(0);
        // Keeps the zeroing from being removed as a dead store.
        std::hint::black_box(&self.der);
    }
}

impl fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrivateKey")
            .field("algorithm", &self.algorithm)
            .finish_non_exhaustive()
    }
}

/// Server configuration: the certificate chain, its private key, ALPN and versions.
///
/// The OS credential is built on the first [`TlsStream::accept`](crate::TlsStream::accept)
/// and reused by later connections and by clones, so share one `ServerConfig` (it is
/// `Send + Sync`; clone it or put it in an `Arc`) instead of building one per connection.
/// The builder methods return a configuration with a fresh credential.
#[derive(Clone)]
pub struct ServerConfig {
    chain: Vec<Vec<u8>>,
    key: Arc<PrivateKey>,
    alpn: Vec<Vec<u8>>,
    min_version: Version,
    cred: Arc<Mutex<Option<Arc<imp::ServerCred>>>>,
}

impl ServerConfig {
    /// Presents `cert_chain_der` (the leaf certificate first, then its intermediates, each in
    /// DER) with `private_key`, the leaf's key. Accepts TLS 1.2 and 1.3 and selects no ALPN
    /// protocol.
    ///
    /// Nothing is checked until the first accept: an empty chain, a certificate the OS cannot
    /// parse or a key that does not match the leaf fails there.
    pub fn new(cert_chain_der: Vec<Vec<u8>>, private_key: PrivateKey) -> ServerConfig {
        ServerConfig {
            chain: cert_chain_der,
            key: Arc::new(private_key),
            alpn: Vec::new(),
            min_version: Version::Tls12,
            cred: Arc::default(),
        }
    }

    /// Sets the ALPN protocols this server accepts, most preferred first (e.g.
    /// `b"http/1.1"`). The server picks its most preferred protocol the client offers; a
    /// client offering ALPN with none of these gets the fatal `no_application_protocol`
    /// alert (RFC 7301 §3.2). Clients that offer no ALPN are accepted without one.
    pub fn alpn(mut self, protocols: &[&[u8]]) -> ServerConfig {
        self.alpn = protocols.iter().map(|p| p.to_vec()).collect();
        self.cred = Arc::default();
        self
    }

    /// Sets the oldest protocol version accepted (default [`Version::Tls12`]). The caveats
    /// of [`Config::min_version`](crate::Config::min_version) apply.
    pub fn min_version(mut self, version: Version) -> ServerConfig {
        self.min_version = version;
        self.cred = Arc::default();
        self
    }

    /// The certificate chain, leaf first.
    pub fn cert_chain_der(&self) -> &[Vec<u8>] {
        &self.chain
    }

    /// The private key.
    pub fn private_key(&self) -> &PrivateKey {
        &self.key
    }

    /// The ALPN protocols accepted, most preferred first.
    pub fn alpn_protocols(&self) -> &[Vec<u8>] {
        &self.alpn
    }

    /// The oldest protocol version accepted.
    pub fn minimum_version(&self) -> Version {
        self.min_version
    }

    /// The OS credential, built on first use. A failure is not cached.
    pub(crate) fn credential(&self) -> Result<Arc<imp::ServerCred>, Error> {
        let mut slot = self.cred.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(cred) = slot.as_ref() {
            return Ok(Arc::clone(cred));
        }
        if self.chain.is_empty() || self.chain.iter().any(Vec::is_empty) {
            return Err(Error::Io(alpn::invalid(
                "the certificate chain is empty or holds an empty certificate",
            )));
        }
        alpn::encode_wire(&self.alpn)?;
        let cred = Arc::new(imp::ServerCred::new(self)?);
        *slot = Some(Arc::clone(&cred));
        Ok(cred)
    }
}

impl fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerConfig")
            .field("certificates", &self.chain.len())
            .field("key", &self.key)
            .field(
                "alpn",
                &self
                    .alpn
                    .iter()
                    .map(|p| String::from_utf8_lossy(p))
                    .collect::<Vec<_>>(),
            )
            .field("min_version", &self.min_version)
            .finish_non_exhaustive()
    }
}

/// What the server knows before the OS TLS stack runs.
#[cfg_attr(
    not(any(windows, all(unix, not(target_vendor = "apple")))),
    allow(dead_code)
)]
pub(crate) struct Prelude {
    /// Bytes read from the stream, starting with the ClientHello.
    pub(crate) recv: Vec<u8>,
    /// The SNI host name the client sent.
    pub(crate) server_name: Option<String>,
    /// The ALPN protocols the TLS stack may select for this connection: the server's choice
    /// alone, nothing when the client offered no ALPN, or the whole server list when the
    /// hello could not be read here (the stack then decides).
    pub(crate) alpn: Vec<Vec<u8>>,
}

const READ_CHUNK: usize = 18 * 1024;

/// A plaintext fatal `no_application_protocol` alert record (RFC 7301 §3.2, RFC 8446 §6).
const NO_APPLICATION_PROTOCOL: [u8; 7] = [21, 3, 3, 0, 2, 2, 120];

/// Reads until the whole ClientHello is buffered and settles ALPN as the server's choice.
pub(crate) fn read_client_hello<S: Read + Write>(
    cfg: &ServerConfig,
    s: &mut S,
) -> Result<Prelude, Error> {
    let mut recv = Vec::new();
    let offer = loop {
        if let Hello::Done(offer) = client_hello::parse(&recv) {
            break offer;
        }
        let start = recv.len();
        recv.resize(start + READ_CHUNK, 0);
        let n = loop {
            match s.read(&mut recv[start..]) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::Io(e)),
            }
        };
        recv.truncate(start + n);
        if n == 0 {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed during the TLS handshake",
            )));
        }
    };
    let Some(offer) = offer else {
        return Ok(Prelude {
            recv,
            server_name: None,
            alpn: cfg.alpn.clone(),
        });
    };
    let alpn = match (&offer.alpn, cfg.alpn.is_empty()) {
        (None, _) | (Some(_), true) => Vec::new(),
        (Some(offered), false) => match alpn::select(&cfg.alpn, offered) {
            Some(p) => vec![p.to_vec()],
            None => {
                // Best effort: the error below is what the caller needs.
                let _ = s
                    .write_all(&NO_APPLICATION_PROTOCOL)
                    .and_then(|()| s.flush());
                return Err(Error::Handshake {
                    code: 120,
                    message: "the client offers no ALPN protocol this server accepts \
                              (no_application_protocol, RFC 7301)"
                        .into(),
                });
            }
        },
    };
    Ok(Prelude {
        recv,
        server_name: offer.server_name,
        alpn,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EC: &[u8] = include_bytes!("../testdata/ec-key.p8.der");

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn config_is_shareable_across_threads() {
        assert_send_sync::<ServerConfig>();
        assert_send_sync::<PrivateKey>();
    }

    #[test]
    fn key_debug_hides_the_key() {
        let k = PrivateKey::from_pkcs8_der(EC).unwrap();
        assert_eq!(k.algorithm(), KeyAlgorithm::EcdsaP256);
        assert_eq!(format!("{k:?}"), "PrivateKey { algorithm: EcdsaP256, .. }");
        let e = PrivateKey::from_pkcs8_der(b"not a key").unwrap_err();
        assert!(matches!(e, Error::Io(ref io) if io.kind() == io::ErrorKind::InvalidInput));
    }

    #[test]
    fn server_config_accessors() {
        let k = PrivateKey::from_pkcs8_der(EC).unwrap();
        let c = ServerConfig::new(vec![vec![1, 2, 3]], k)
            .alpn(&[b"http/1.1"])
            .min_version(Version::Tls13);
        assert_eq!(c.cert_chain_der(), &[vec![1, 2, 3]]);
        assert_eq!(c.private_key().algorithm(), KeyAlgorithm::EcdsaP256);
        assert_eq!(c.alpn_protocols(), &[b"http/1.1".to_vec()]);
        assert_eq!(c.minimum_version(), Version::Tls13);
        let dbg = format!("{c:?}");
        assert!(
            dbg.contains("certificates: 1") && dbg.contains("http/1.1"),
            "{dbg}"
        );
    }

    #[test]
    fn invalid_configs_fail_before_reading() {
        let k = || PrivateKey::from_pkcs8_der(EC).unwrap();
        for c in [
            ServerConfig::new(Vec::new(), k()),
            ServerConfig::new(vec![Vec::new()], k()),
            ServerConfig::new(vec![vec![0x30]], k()).alpn(&[b""]),
        ] {
            match c.credential() {
                Err(Error::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::InvalidInput),
                Err(e) => panic!("{e}"),
                Ok(_) => panic!("accepted"),
            }
        }
    }
}
