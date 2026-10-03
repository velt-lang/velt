//! The client: one request per call, over `std::net`. `https://` goes through rustls with
//! Mozilla's root certificates ([`crate::tls`]), the TLS stack the Velt runtime uses too, so
//! nothing depends on a system `curl` or the machine's trust store.

use std::io::{BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::message::{read_response, Response};

/// Largest response body accepted (packages and API answers are far smaller).
const MAX_RESPONSE: usize = 256 << 20;

/// How long a request may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Connecting to one address of the host.
    pub connect: Duration,
    /// Waiting for the server in one read or write.
    pub idle: Duration,
    /// The whole exchange, from connecting to the last byte of the answer: a server that sends
    /// a byte now and then can't hold a fetch forever.
    pub total: Duration,
}

impl Limits {
    /// The limits of [`fetch`]: 10 s to connect, 60 s of silence, 10 minutes in all.
    pub const DEFAULT: Limits = Limits {
        connect: Duration::from_secs(10),
        idle: Duration::from_secs(60),
        total: Duration::from_secs(600),
    };
}

/// Send `method url` with extra `headers` and `body`; any status is returned as a response.
/// Connecting may take 10 s per address, the server may be silent for 60 s at a time, and the
/// whole exchange may take 10 minutes.
pub fn fetch(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<Response, String> {
    fetch_within(method, url, headers, body, Limits::DEFAULT)
}

/// [`fetch`] with other [`Limits`] (an editor wants an answer in seconds, not minutes).
pub fn fetch_within(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    limits: Limits,
) -> Result<Response, String> {
    // A line break in the request line or a header would end it early: what follows would be
    // read as headers of its own (a token from a file or the environment must not inject any).
    let line_break = |s: &str| s.contains(['\r', '\n']);
    if line_break(method) || line_break(url) {
        return Err(format!(
            "{method:?} {url:?}: a line break in the request line"
        ));
    }
    if let Some((name, _)) = headers.iter().find(|(n, v)| line_break(n) || line_break(v)) {
        return Err(format!(
            "{method} {url}: the `{}` header contains a line break",
            name.trim()
        ));
    }
    let deadline = Instant::now() + limits.total;
    if let Some(rest) = url.strip_prefix("http://") {
        return plain(method, rest, headers, body, limits, deadline)
            .map_err(|e| format!("{method} {url}: {e}"));
    }
    if let Some(rest) = url.strip_prefix("https://") {
        return crate::tls::client_config()
            .and_then(|config| https_within(method, rest, headers, body, config, limits, deadline))
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

/// A connection to `host` (each of its addresses in turn, `limits.connect` each but never past
/// `deadline`), whose reads
/// and writes fail once `deadline` has passed.
fn connect(
    host: &str,
    default_port: u16,
    limits: Limits,
    deadline: Instant,
) -> Result<Deadlined, String> {
    let authority = if host.rsplit_once(':').is_some_and(|(_, p)| !p.contains(']')) {
        host.to_string()
    } else {
        format!("{host}:{default_port}")
    };
    let addrs = authority
        .to_socket_addrs()
        .map_err(|e| format!("cannot connect: {e}"))?;
    let mut last = None;
    for addr in addrs {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!("cannot connect: {}", too_long()));
        }
        match TcpStream::connect_timeout(&addr, limits.connect.min(left)) {
            Ok(stream) => {
                return Ok(Deadlined {
                    stream,
                    idle: limits.idle,
                    deadline,
                })
            }
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => format!("cannot connect: {e}"),
        None => format!("cannot connect: `{host}` has no address"),
    })
}

/// A TCP stream whose every read and write waits at most `idle`, and fails once `deadline`
/// has passed.
struct Deadlined {
    stream: TcpStream,
    idle: Duration,
    deadline: Instant,
}

impl Deadlined {
    /// The time the next read or write may wait, or the error once the deadline has passed.
    fn wait(&self) -> std::io::Result<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(too_long());
        }
        Ok(left.min(self.idle))
    }

    /// A read or write that timed out because the deadline passed says so.
    fn explain<T>(&self, result: std::io::Result<T>) -> std::io::Result<T> {
        match result {
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) && Instant::now() >= self.deadline =>
            {
                Err(too_long())
            }
            other => other,
        }
    }
}

fn too_long() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, "the request took too long")
}

impl Read for Deadlined {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let wait = self.wait()?;
        self.stream.set_read_timeout(Some(wait))?;
        let read = self.stream.read(buf);
        self.explain(read)
    }
}

impl Write for Deadlined {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let wait = self.wait()?;
        self.stream.set_write_timeout(Some(wait))?;
        let written = self.stream.write(buf);
        self.explain(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

fn plain(
    method: &str,
    rest: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    limits: Limits,
    deadline: Instant,
) -> Result<Response, String> {
    let (host, path) = split(rest);
    let stream = connect(host, 80, limits, deadline)?;
    exchange(stream, method, host, path, headers, body)
}

/// One request over TLS, verified with `config`, within the default limits.
#[cfg(test)]
pub(crate) fn https(
    method: &str,
    rest: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    config: Arc<ClientConfig>,
) -> Result<Response, String> {
    let limits = Limits::DEFAULT;
    let deadline = Instant::now() + limits.total;
    https_within(method, rest, headers, body, config, limits, deadline)
}

/// One request over TLS, verified with `config`.
fn https_within(
    method: &str,
    rest: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    config: Arc<ClientConfig>,
    limits: Limits,
    deadline: Instant,
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
    let stream = StreamOwned::new(conn, connect(host, 443, limits, deadline)?);
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

    /// A server that answers one byte at a time, never pausing long enough for the idle timeout.
    #[test]
    fn a_dripping_server_hits_the_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = conn.read(&mut request);
            for b in b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n"
                .iter()
                .cycle()
            {
                if conn.write_all(&[*b]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let limits = Limits {
            connect: Duration::from_secs(5),
            idle: Duration::from_secs(5),
            total: Duration::from_millis(700),
        };
        let started = Instant::now();
        let err = fetch_within("GET", &format!("http://{addr}/"), &[], b"", limits).unwrap_err();
        assert!(err.contains("took too long"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
        server.join().unwrap();
    }

    #[test]
    fn urls_split_into_host_and_path() {
        assert_eq!(split("h:8080/a?b"), ("h:8080", "/a?b"));
        assert_eq!(split("h"), ("h", "/"));
    }

    /// A CR or LF in a header (a token, say) would inject headers: nothing is sent at all.
    #[test]
    fn line_breaks_never_reach_the_wire() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let injected = "t0ken\r\nX-Admin: yes";
        for (name, value) in [("Authorization", injected), ("X\nY", "v")] {
            let err = fetch("GET", &url, &[(name, value)], b"").unwrap_err();
            assert!(err.contains("contains a line break"), "{err}");
        }
        let err = fetch("GET", &format!("{url}x\r\nX: y"), &[], b"").unwrap_err();
        assert!(err.contains("a line break in the request line"), "{err}");
        assert!(listener.accept().is_err(), "no connection was made");
    }
}
