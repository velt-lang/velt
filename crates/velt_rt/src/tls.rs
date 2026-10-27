//! TLS configuration shared by HTTPS (`fetch`, `serve({ tls })`) and secure WebSockets: rustls
//! with the `ring` provider and Mozilla's root certificates (`webpki-roots`, compiled in, so the
//! result does not depend on the machine's trust store); servers offer HTTP/2 and HTTP/1.1 (ALPN).
//!
//! Certificates and keys are PEM text. A client may trust extra PEM CA certificates on top of
//! the built-in roots (private CAs, self-signed test certificates).

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::PrivateKeyDer;
use rustls::{ClientConfig, ServerConfig};
use std::sync::Arc;

/// ALPN protocols, most preferred first.
const ALPN: [&[u8]; 2] = [b"h2", b"http/1.1"];

pub(crate) use velt_tls::{certificates, provider};

/// A server configuration from a PEM certificate chain and a PEM private key (PKCS#8, PKCS#1
/// or SEC1), offering h2 and http/1.1.
pub fn server_config(cert_pem: &[u8], key_pem: &[u8]) -> Result<Arc<ServerConfig>, String> {
    server_config_alpn(cert_pem, key_pem, ALPN.iter().map(|p| p.to_vec()).collect())
}

/// [`server_config`] offering the protocols in `alpn` (none: no ALPN), for TLS streams that
/// carry something else than HTTP.
pub fn server_config_alpn(
    cert_pem: &[u8],
    key_pem: &[u8],
    alpn: Vec<Vec<u8>>,
) -> Result<Arc<ServerConfig>, String> {
    let certs = certificates(cert_pem, "certificate")?;
    let key =
        PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| format!("invalid key PEM: {e}"))?;
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("certificate and key do not match: {e}"))?;
    config.alpn_protocols = alpn;
    Ok(Arc::new(config))
}

/// A client configuration trusting the built-in roots plus `extra_ca_pem` (may be empty),
/// cached per CA text (`velt_tls`, shared with the native drivers). ALPN is left empty: the HTTP
/// connector sets h2/http1.1 itself, and WebSockets need HTTP/1.1.
pub fn client_config(extra_ca_pem: &[u8]) -> Result<Arc<ClientConfig>, String> {
    velt_tls::client_config(extra_ca_pem, "CA")
}

/// [`client_config`] offering the protocols in `alpn` (none: the cached configuration).
pub fn client_config_alpn(
    extra_ca_pem: &[u8],
    alpn: Vec<Vec<u8>>,
) -> Result<Arc<ClientConfig>, String> {
    let config = client_config(extra_ca_pem)?;
    if alpn.is_empty() {
        return Ok(config);
    }
    let mut config = (*config).clone();
    config.alpn_protocols = alpn;
    Ok(Arc::new(config))
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
