//! Connection strings: a `postgres://` / `postgresql://` URL or a libpq `key=value` string, as
//! tokio-postgres parses them (host, port, user, password, dbname, `connect_timeout`,
//! `application_name`, `options`, several hosts …), plus the TLS settings it does not know.
//!
//! `sslmode` follows libpq: `disable`; `prefer` (the default: TLS when the server offers it);
//! `require` (TLS, the certificate is not checked); `verify-ca` (the certificate must chain to
//! a trusted root); `verify-full` (and name the host). Trusted roots are Mozilla's
//! (`webpki-roots`) plus the PEM file named by `sslrootcert`, which also turns `require`
//! into `verify-ca`, as in libpq. Those two options are removed before tokio-postgres sees the
//! string.

use super::error::PgError;
use std::str::FromStr;
use tokio_postgres::config::SslMode;
use tokio_postgres::Config;

/// How the server certificate is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verify {
    /// Encrypted but unauthenticated (`prefer`, `require`).
    None,
    /// The chain must reach a trusted root; the host name is not checked (`verify-ca`).
    Chain,
    /// Chain and host name (`verify-full`).
    Full,
}

/// A parsed connection string.
#[derive(Debug, Clone)]
pub struct PgConfig {
    /// Everything tokio-postgres handles (including disable / prefer / require).
    pub inner: Config,
    /// Certificate checking when TLS is used.
    pub verify: Verify,
    /// Path of extra trusted CA certificates (PEM), `sslrootcert`.
    pub root_cert: Option<String>,
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b.get(i + 1..i + 3).and_then(|h| {
            std::str::from_utf8(h)
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        });
        match (b[i], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Remove the TLS options from `s` (URL query or `key=value` words), returning the rest and
/// the `(key, value)` pairs taken out.
fn split_tls_options(s: &str) -> (String, Vec<(String, String)>) {
    let is_tls = |k: &str| k == "sslmode" || k == "sslrootcert";
    let mut taken = Vec::new();
    if s.starts_with("postgres://") || s.starts_with("postgresql://") {
        let Some((base, query)) = s.split_once('?') else {
            return (s.to_string(), taken);
        };
        let mut kept = Vec::new();
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            if is_tls(k) {
                taken.push((k.to_string(), percent_decode(v)));
            } else {
                kept.push(pair);
            }
        }
        let rest = if kept.is_empty() {
            base.to_string()
        } else {
            format!("{base}?{}", kept.join("&"))
        };
        return (rest, taken);
    }
    let mut kept = Vec::new();
    for word in s.split_whitespace() {
        match word.split_once('=') {
            Some((k, v)) if is_tls(k.trim()) => {
                taken.push((k.trim().to_string(), v.trim_matches('\'').to_string()))
            }
            _ => kept.push(word),
        }
    }
    (kept.join(" "), taken)
}

/// Parse a connection string (see the module docs).
pub fn parse(s: &str) -> Result<PgConfig, PgError> {
    let (rest, tls) = split_tls_options(s.trim());
    let mut inner = Config::from_str(&rest)
        .map_err(|e| PgError::invalid(format!("invalid connection string: {}", source_text(&e))))?;
    let mut verify = Verify::None;
    let mut root_cert = None;
    let mut mode = "prefer".to_string();
    for (k, v) in tls {
        if k == "sslrootcert" {
            root_cert = Some(v);
        } else {
            mode = v;
        }
    }
    let ssl = match mode.as_str() {
        "disable" => SslMode::Disable,
        "prefer" => SslMode::Prefer,
        "require" => SslMode::Require,
        "verify-ca" | "verify-full" => SslMode::Require,
        other => {
            return Err(PgError::invalid(format!(
                "unsupported sslmode \"{other}\" (use disable, prefer, require, verify-ca or \
                 verify-full)"
            )))
        }
    };
    if mode == "verify-full" {
        verify = Verify::Full;
    } else if mode == "verify-ca" || (mode == "require" && root_cert.is_some()) {
        verify = Verify::Chain;
    }
    inner.ssl_mode(ssl);
    Ok(PgConfig {
        inner,
        verify,
        root_cert,
    })
}

/// A tokio-postgres error with its cause (its `Display` shows only the kind).
fn source_text(e: &tokio_postgres::Error) -> String {
    match std::error::Error::source(e) {
        Some(cause) => cause.to_string(),
        None => e.to_string(),
    }
}
