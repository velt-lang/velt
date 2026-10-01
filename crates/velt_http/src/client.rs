//! The client: one request per call. `http://` goes over `std::net`; `https://` runs the system
//! `curl` (present on macOS, Windows 10+ and practically every Linux), which brings TLS and the
//! platform's certificate store without a TLS dependency here.

use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::process::Command;
use std::time::Duration;

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
    if url.starts_with("https://") {
        return curl(method, url, headers, body).map_err(|e| format!("{method} {url}: {e}"));
    }
    Err(format!(
        "unsupported URL `{url}` (expected http:// or https://)"
    ))
}

fn plain(
    method: &str,
    rest: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<Response, String> {
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let authority = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:80")
    };
    let mut stream = TcpStream::connect(&authority).map_err(|e| format!("cannot connect: {e}"))?;
    let _ = stream.set_read_timeout(Some(TIMEOUT));
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
        .map_err(|e| format!("cannot send: {e}"))?;
    read_response(&mut BufReader::new(stream), MAX_RESPONSE)
}

fn curl(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<Response, String> {
    let dir = tempfile_dir()?;
    let (body_in, head_out, body_out) =
        (dir.join("request"), dir.join("head"), dir.join("response"));
    std::fs::write(&body_in, body).map_err(|e| format!("cannot write a temp file: {e}"))?;
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-X", method, "--max-time", "120"])
        .arg("--data-binary")
        .arg(format!("@{}", body_in.display()))
        .arg("-D")
        .arg(&head_out)
        .arg("-o")
        .arg(&body_out);
    for (n, v) in headers {
        cmd.arg("-H").arg(format!("{n}: {v}"));
    }
    let out = cmd
        .arg(url)
        .output()
        .map_err(|e| format!("cannot run curl (needed for https:// URLs): {e}"))?;
    let result = if out.status.success() {
        let mut wire = std::fs::read(&head_out).map_err(|e| e.to_string())?;
        let body = std::fs::read(&body_out).unwrap_or_default();
        wire.extend_from_slice(b"\r\n");
        last_response_head(&wire, body)
    } else {
        Err(format!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// curl's `-D` file holds every response head (redirects, `100 Continue`); use the last one.
fn last_response_head(wire: &[u8], body: Vec<u8>) -> Result<Response, String> {
    let text = String::from_utf8_lossy(wire).replace("\r\n", "\n");
    let last = text
        .split("\n\n")
        .filter(|h| h.starts_with("HTTP/"))
        .last()
        .ok_or("curl returned no HTTP response head")?;
    let head = format!("{}\r\n\r\n", last.replace('\n', "\r\n"));
    let mut resp = read_response(&mut head.as_bytes(), 0)?;
    resp.body = body;
    Ok(resp)
}

fn tempfile_dir() -> Result<std::path::PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "velt-http-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create a temp dir: {e}"))?;
    Ok(dir)
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
    }

    #[test]
    fn curl_heads_use_the_last_response() {
        let wire = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/2 201\r\ncontent-type: x\r\n\r\n";
        let resp = last_response_head(wire, b"ok".to_vec()).unwrap();
        assert_eq!((resp.status, resp.header("Content-Type")), (201, Some("x")));
        assert_eq!(resp.body, b"ok");
    }
}
