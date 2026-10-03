//! The client's TLS configuration: rustls with the `ring` provider and Mozilla's root
//! certificates (`webpki-roots`, compiled in), plus the PEM certificates in the file named by
//! `$VELT_CA_FILE` for registries behind a private CA.

use std::sync::{Arc, OnceLock};

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};

/// Environment variable naming a PEM file of extra CA certificates to trust.
pub const CA_FILE_VAR: &str = "VELT_CA_FILE";

/// The process's client configuration (built once; `$VELT_CA_FILE` is read then).
pub fn client_config() -> Result<Arc<ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let extra = match std::env::var_os(CA_FILE_VAR) {
                Some(path) if !path.is_empty() => std::fs::read(&path).map_err(|e| {
                    format!(
                        "cannot read ${CA_FILE_VAR} `{}`: {e}",
                        path.to_string_lossy()
                    )
                })?,
                _ => Vec::new(),
            };
            config_with(&extra)
        })
        .clone()
}

/// A configuration trusting the built-in roots plus the CA certificates in `extra_pem`.
pub fn config_with(extra_pem: &[u8]) -> Result<Arc<ClientConfig>, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if !extra_pem.is_empty() {
        let certs = CertificateDer::pem_slice_iter(extra_pem)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("invalid CA certificate PEM: {e}"))?;
        if certs.is_empty() {
            return Err("no certificate found in the CA PEM".into());
        }
        for cert in certs {
            roots
                .add(cert)
                .map_err(|e| format!("invalid CA certificate: {e}"))?;
        }
    }
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests;
