//! Throw-away self-signed PKI for the spike (generated in memory at start, never written).

use crate::Result;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::sync::Arc;

/// Server name in the certificate.
pub const SERVER_NAME: &str = "localhost";

/// Certificate and key of the test server.
#[derive(Debug)]
pub struct TestPki {
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

impl TestPki {
    /// Fresh self-signed certificate for [`SERVER_NAME`].
    pub fn generate() -> Result<Self> {
        let ck = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_string()])?;
        Ok(Self {
            cert: ck.cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der()),
        })
    }

    /// TLS 1.3 server config announcing `alpn`.
    pub fn server(&self, alpn: &[u8]) -> Result<rustls::ServerConfig> {
        let mut c = rustls::ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(
                vec![self.cert.clone()],
                PrivateKeyDer::Pkcs8(self.key.clone_key()),
            )?;
        c.alpn_protocols = vec![alpn.to_vec()];
        Ok(c)
    }

    /// TLS 1.3 client config trusting only this certificate.
    pub fn client(&self, alpn: &[u8]) -> Result<rustls::ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(self.cert.clone())?;
        let mut c = rustls::ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_root_certificates(roots)
            .with_no_client_auth();
        c.alpn_protocols = vec![alpn.to_vec()];
        Ok(c)
    }
}
