//! The client: one request per call, over `std::net`. `https://` goes through rustls with
//! Mozilla's root certificates ([`crate::tls`]), the TLS stack the Velt runtime uses too, so
//! nothing depends on a system `curl` or the machine's trust store.

use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::message::{read_response, Response};

/// Largest response body accepted (packages and API answers are far smaller).
const MAX_RESPONSE: usize = 256 << 20;
const TIMEOUT: Duration = Duration::from_secs(60);

/// Send `method url` with extra `headers` and `body`; any status is returned as a response.
pub fn fetch(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<Response, String> {
    if let Some(rest) = url.strip_prefix("http://") {
        return plain(method, rest, headers, body).map_err(|e| format!("{method} {url}: {e}"));
    }
    if let Some(rest) = url.strip_prefix("https://") {
        return crate::tls::client_config()
            .and_then(|config| https(method, rest, headers, body, config))
            .map_err(|e| format!("{method} {url}: {e}"));
    }
    Err(format!(
        "unsupported URL `{url}` (expected http:// or https://)"
    ))
}

/// `host[:port]` and `/path?query` of a URL without its scheme.
fn split(rest: &str) -> (&str, &str) {
    match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    }
}

fn connect(host: &str, default_port: u16) -> Result<TcpStream, String> {
    let authority = if host.rsplit_once(':').is_some_and(|(_, p)| !p.contains(']')) {
        host.to_string()
    } else {
        format!("{host}:{default_port}")
    };
    let stream = TcpStream::connect(&authority).map_err(|e| format!("cannot connect: {e}"))?;
    let _ = stream.set_read_timeout(Some(TIMEOUT));
    let _ = stream.set_write_timeout(Some(TIMEOUT));
    Ok(stream)
}

fn plain(
    method: &str,
    rest: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<Response, String> {
    let (host, path) = split(rest);
    let stream = connect(host, 80)?;
    exchange(stream, method, host, path, headers, body)
}

/// One request over TLS, verified with `config`.
pub(crate) fn https(
    method: &str,
    rest: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    config: Arc<ClientConfig>,
) -> Result<Response, String> {
    let (host, path) = split(rest);
    // `[::1]:8443` → `::1`, `example.com:8443` → `example.com`.
    let name = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(v6),
        None => host.split(':').next().unwrap_or(host),
    };
    let server_name =
        ServerName::try_from(name.to_string()).map_err(|_| format!("invalid host `{name}`"))?;
    let conn = ClientConnection::new(config, server_name).map_err(|e| format!("TLS: {e}"))?;
    let stream = StreamOwned::new(conn, connect(host, 443)?);
    exchange(stream, method, host, path, headers, body)
}

fn exchange(
    mut stream: impl Read + Write,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<Response, String> {
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (n, v) in headers {
        head.push_str(&format!("{n}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body))
        .and_then(|()| stream.flush())
        .map_err(|e| format!("cannot send: {e}"))?;
    read_response(&mut BufReader::new(stream), MAX_RESPONSE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_and_unreachable_urls() {
        assert!(fetch("GET", "ftp://x", &[], b"")
            .unwrap_err()
            .contains("unsupported"));
        let err = fetch("GET", "http://127.0.0.1:1/", &[], b"").unwrap_err();
        assert!(err.contains("cannot connect"), "{err}");
        let err = fetch("GET", "https://127.0.0.1:1/", &[], b"").unwrap_err();
        assert!(err.contains("cannot connect"), "{err}");
    }

    #[test]
    fn urls_split_into_host_and_path() {
        assert_eq!(split("h:8080/a?b"), ("h:8080", "/a?b"));
        assert_eq!(split("h"), ("h", "/"));
    }
}
