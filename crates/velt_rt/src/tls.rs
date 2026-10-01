//! TLS configuration shared by HTTPS (`fetch`, `serve({ tls })`) and secure WebSockets: rustls
//! with the `ring` provider and Mozilla's root certificates (`webpki-roots`, compiled in, so the
//! result does not depend on the machine's trust store); servers offer HTTP/2 and HTTP/1.1 (ALPN).
//!
//! Certificates and keys are PEM text. A client may trust extra PEM CA certificates on top of
//! the built-in roots (private CAs, self-signed test certificates).

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// ALPN protocols, most preferred first.
const ALPN: [&[u8]; 2] = [b"h2", b"http/1.1"];

/// The process-wide rustls `ring` crypto provider.
pub(crate) fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    static PROVIDER: OnceLock<Arc<rustls::crypto::CryptoProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| Arc::new(rustls::crypto::ring::default_provider()))
        .clone()
}

/// Every certificate in `pem` (at least one), or a message naming `what` was being read.
pub(crate) fn certificates(pem: &[u8], what: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    let certs = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("invalid {what} PEM: {e}"))?;
    if certs.is_empty() {
        return Err(format!("no certificate found in the {what} PEM"));
    }
    Ok(certs)
}

/// A server configuration from a PEM certificate chain and a PEM private key (PKCS#8, PKCS#1
/// or SEC1), offering h2 and http/1.1.
pub fn server_config(cert_pem: &[u8], key_pem: &[u8]) -> Result<Arc<ServerConfig>, String> {
    let certs = certificates(cert_pem, "certificate")?;
    let key =
        PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| format!("invalid key PEM: {e}"))?;
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("certificate and key do not match: {e}"))?;
    config.alpn_protocols = ALPN.iter().map(|p| p.to_vec()).collect();
    Ok(Arc::new(config))
}

fn build_client(extra_ca_pem: &[u8]) -> Result<Arc<ClientConfig>, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if !extra_ca_pem.is_empty() {
        for cert in certificates(extra_ca_pem, "CA")? {
            roots
                .add(cert)
                .map_err(|e| format!("invalid CA certificate: {e}"))?;
        }
    }
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// A client configuration trusting the built-in roots plus `extra_ca_pem` (may be empty),
/// cached per CA text. ALPN is left empty: the HTTP connector sets h2/http1.1 itself, and
/// WebSockets need HTTP/1.1.
pub fn client_config(extra_ca_pem: &[u8]) -> Result<Arc<ClientConfig>, String> {
    type Cache = Mutex<HashMap<Vec<u8>, Arc<ClientConfig>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Cache::default);
    if let Some(c) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(extra_ca_pem)
    {
        return Ok(c.clone());
    }
    let config = build_client(extra_ca_pem)?;
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(extra_ca_pem.to_vec(), config.clone());
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_pem() {
        assert!(server_config(b"nope", b"nope")
            .unwrap_err()
            .contains("certificate"));
        assert!(
            client_config(b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n")
                .is_err()
        );
        assert!(client_config(b"").is_ok());
    }
}
