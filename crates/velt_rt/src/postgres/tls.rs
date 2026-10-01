//! TLS for PostgreSQL connections: tokio-postgres' `TlsConnect` over tokio-rustls, with
//! the runtime's rustls `ring` provider and roots (`crate::tls`), so no second TLS stack or
//! crypto provider is linked.
//!
//! The certificate check follows `sslmode` ([`Verify`]): `verify-full` uses rustls' standard
//! WebPKI verifier; `verify-ca` the same but accepts a certificate issued for another name;
//! `prefer` / `require` accept any certificate (libpq semantics: encryption without
//! authentication) but still check the handshake signatures. Channel binding
//! (SCRAM-SHA-256-PLUS) is not offered, so SCRAM falls back to plain SCRAM-SHA-256.
//!
//! The handshake runs on the socket under the connection's wire (`super::wire`), which then
//! wraps the encrypted stream instead.

use super::config::{PgConfig, Verify};
use super::socket::RawSocket;
use super::wire::WireStream;
use crate::tls::{certificates, provider};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore};
use rustls::{Error as TlsError, SignatureScheme};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_postgres::tls::{ChannelBinding, TlsConnect, TlsStream};
use tokio_rustls::TlsConnector;

/// Checks a server certificate per [`Verify`] (the `Full` case uses the WebPKI verifier
/// directly).
#[derive(Debug)]
struct LenientVerifier {
    /// The chain check (`verify-ca`); `None` accepts any certificate (`require`).
    chain: Option<Arc<WebPkiServerVerifier>>,
}

impl ServerCertVerifier for LenientVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let Some(chain) = &self.chain else {
            return Ok(ServerCertVerified::assertion());
        };
        match chain.verify_server_cert(end_entity, intermediates, server_name, ocsp, now) {
            Err(TlsError::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        let algs = &provider().signature_verification_algorithms;
        rustls::crypto::verify_tls12_signature(message, cert, dss, algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        let algs = &provider().signature_verification_algorithms;
        rustls::crypto::verify_tls13_signature(message, cert, dss, algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn roots(extra_ca_pem: &[u8]) -> Result<Arc<RootCertStore>, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if !extra_ca_pem.is_empty() {
        for cert in certificates(extra_ca_pem, "sslrootcert")? {
            roots
                .add(cert)
                .map_err(|e| format!("invalid CA certificate in sslrootcert: {e}"))?;
        }
    }
    Ok(Arc::new(roots))
}

/// The rustls client configuration for `verify` with extra roots `extra_ca_pem` (may be empty).
pub fn client_config(verify: Verify, extra_ca_pem: &[u8]) -> Result<Arc<ClientConfig>, String> {
    if verify == Verify::Full {
        return crate::tls::client_config(extra_ca_pem);
    }
    let chain = match verify {
        Verify::Chain => Some(
            WebPkiServerVerifier::builder_with_provider(roots(extra_ca_pem)?, provider())
                .build()
                .map_err(|e| e.to_string())?,
        ),
        _ => None,
    };
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(LenientVerifier { chain }))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// The TLS connector of one connection attempt (reads `sslrootcert` now, so a missing file
/// fails the connect with a clear message).
pub fn connector(config: &PgConfig) -> Result<PgTls, String> {
    let pem = match &config.root_cert {
        Some(path) => {
            std::fs::read(path).map_err(|e| format!("cannot read sslrootcert \"{path}\": {e}"))?
        }
        None => Vec::new(),
    };
    Ok(PgTls {
        config: client_config(config.verify, &pem)?,
    })
}

/// The TLS settings of a connection string, for each connection opened with it.
#[derive(Clone)]
pub struct PgTls {
    config: Arc<ClientConfig>,
}

impl PgTls {
    /// The handshake for a connection to `host` (the name the certificate is checked
    /// against; empty for a Unix socket, where the server never offers TLS).
    pub fn for_host(&self, host: &str) -> PgTlsConnect {
        PgTlsConnect {
            config: self.config.clone(),
            name: ServerName::try_from(host.to_string()).map_err(|e| e.to_string()),
        }
    }
}

/// The handshake of one connection.
pub struct PgTlsConnect {
    config: Arc<ClientConfig>,
    name: Result<ServerName<'static>, String>,
}

type Handshake = Pin<Box<dyn Future<Output = io::Result<WireStream<PgTlsStream>>> + Send>>;

impl TlsConnect<WireStream<RawSocket>> for PgTlsConnect {
    type Stream = WireStream<PgTlsStream>;
    type Error = io::Error;
    type Future = Handshake;

    fn connect(self, stream: WireStream<RawSocket>) -> Handshake {
        Box::pin(async move {
            let name = self
                .name
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            let (socket, shared) = stream.into_parts();
            let tls = TlsConnector::from(self.config)
                .connect(name, socket)
                .await?;
            Ok(WireStream::new(PgTlsStream(Box::new(tls)), shared))
        })
    }
}

/// An encrypted connection.
pub struct PgTlsStream(Box<tokio_rustls::client::TlsStream<RawSocket>>);
impl AsyncRead for PgTlsStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for PgTlsStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_shutdown(cx)
    }
}

impl TlsStream for WireStream<PgTlsStream> {
    fn channel_binding(&self) -> ChannelBinding {
        ChannelBinding::none()
    }
}
