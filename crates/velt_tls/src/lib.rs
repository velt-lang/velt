//! Velt's TLS client setup, shared by the runtime (`fetch`, WebSockets) and native libraries
//! (std's database drivers, any package): rustls with the `ring` provider and Mozilla's root
//! certificates (`webpki-roots`, compiled in, so the result does not depend on the machine's
//! trust store), plus extra PEM CA certificates (private CAs, self-signed test certificates).
//! One provider and one cache per process, whoever connects.

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub use rustls;

/// The process-wide rustls `ring` crypto provider.
pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    static PROVIDER: OnceLock<Arc<rustls::crypto::CryptoProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| Arc::new(rustls::crypto::ring::default_provider()))
        .clone()
}

/// Every certificate in `pem` (at least one), or a message naming `what` was being read.
pub fn certificates(pem: &[u8], what: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    let certs = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("invalid {what} PEM: {e}"))?;
    if certs.is_empty() {
        return Err(format!("no certificate found in the {what} PEM"));
    }
    Ok(certs)
}

/// The built-in roots plus the certificates of `extra_ca_pem` (may be empty); `what` names the
/// PEM in messages ("CA", "sslrootcert").
pub fn roots(extra_ca_pem: &[u8], what: &str) -> Result<RootCertStore, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if !extra_ca_pem.is_empty() {
        for cert in certificates(extra_ca_pem, what)? {
            roots
                .add(cert)
                .map_err(|e| format!("invalid CA certificate in the {what} PEM: {e}"))?;
        }
    }
    Ok(roots)
}

/// A client configuration with rustls' standard verification against [`roots`], cached per CA
/// text (connections open often; building the root store is not cheap). No ALPN: a caller that
/// needs it clones the configuration and sets `alpn_protocols`.
pub fn client_config(extra_ca_pem: &[u8], what: &str) -> Result<Arc<ClientConfig>, String> {
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
    let config = Arc::new(
        ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots(extra_ca_pem, what)?)
            .with_no_client_auth(),
    );
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
    fn bad_pem_is_reported_by_name() {
        let e = client_config(b"not a certificate", "sslrootcert").unwrap_err();
        assert!(e.contains("sslrootcert"), "{e}");
        assert!(client_config(b"", "CA").is_ok());
        // The same CA text is the same configuration.
        let (a, b) = (
            client_config(b"", "CA").unwrap(),
            client_config(b"", "CA").unwrap(),
        );
        assert!(Arc::ptr_eq(&a, &b));
    }
}
