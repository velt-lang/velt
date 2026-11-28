//! The client: one request per call, over `std::net`. `https://` goes through rustls with
//! Mozilla's root certificates ([`crate::tls`]), the TLS stack the Velt runtime uses too, so
//! nothing depends on a system `curl` or the machine's trust store. A request goes through the
//! proxy the environment sets ([`crate::proxy`]).

use std::io::{BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::message::{read_response, Response};
use crate::proxy::{split_port, Proxy};

/// Largest response body accepted (packages and API answers are far smaller).
const MAX_RESPONSE: usize = 256 << 20;

/// How long a request may take, and how large its answer may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Connecting to one address of the host.
    pub connect: Duration,
    /// Waiting for the server in one read or write.
    pub idle: Duration,
    /// The whole exchange, from connecting to the last byte of the answer: a server that sends
    /// a byte now and then can't hold a fetch forever.
    pub total: Duration,
    /// The largest response body (bytes): a larger `Content-Length` is refused before anything
    /// is allocated, and a body without one stops there.
    pub max_body: usize,
}

impl Limits {
    /// The limits of [`fetch`]: 10 s to connect, 60 s of silence, 10 minutes in all, 256 MiB.
    pub const DEFAULT: Limits = Limits {
        connect: Duration::from_secs(10),
        idle: Duration::from_secs(60),
        total: Duration::from_secs(600),
        max_body: MAX_RESPONSE,
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
    let via = crate::proxy::from_env(url).map_err(|e| format!("{method} {url}: {e}"))?;
    fetch_via(method, url, headers, body, limits, via.as_ref())
}

/// [`fetch_within`] through `via` (`None`: directly), whatever the environment says.
pub fn fetch_via(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    limits: Limits,
    via: Option<&Proxy>,
) -> Result<Response, String> {
    check_request(method, url, headers)?;
    let budget = Budget {
        limits,
        deadline: Instant::now() + limits.total,
    };
    if let Some(rest) = url.strip_prefix("http://") {
        return plain(method, rest, headers, body, budget, via)
            .map_err(|e| format!("{method} {url}: {e}"));
    }
    if let Some(rest) = url.strip_prefix("https://") {
        return crate::tls::client_config()
            .and_then(|config| https_within(method, rest, headers, body, config, budget, via))
            .map_err(|e| format!("{method} {url}: {e}"));
    }
    Err(format!(
        "unsupported URL `{url}` (expected http:// or https://)"
    ))
}

/// The limits of a request and the moment its time is up.
#[derive(Clone, Copy)]
struct Budget {
    limits: Limits,
    deadline: Instant,
}

/// The request line and headers are written as text: a CR or LF in any part (a token read from
/// a file with a trailing `\r`, a crafted URL) would end a line early and inject headers or a
/// second request, so control characters are refused before anything is sent. The message names
/// the header, never its value (which may be a token).
///
/// The host is everything between the scheme and the first `/` ([`split`]). An `@`, `?`, `#` or
/// `\` there would make other URL parsers (the registry token's loopback rule, a browser) see a
/// different host than the one connected to: `http://localhost?.attacker.example/` is
/// `localhost` to them and `localhost?.attacker.example` here. Such URLs are refused.
fn check_request(method: &str, url: &str, headers: &[(&str, &str)]) -> Result<(), String> {
    let control = |s: &str| s.chars().any(|c| c.is_control());
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(format!(
            "invalid HTTP method {method:?}: a line break or another character that is not an uppercase letter in the request line"
        ));
    }
    if control(url) || url.contains(' ') {
        return Err(format!(
            "{method} {url:?}: a line break, another control character or a space in the request line"
        ));
    }
    if let Err(e) = url_host(url) {
        // An unknown scheme is reported when the request is dispatched.
        if !e.starts_with("unsupported URL") {
            return Err(format!("{method} {url}: {e}"));
        }
    }
    for (name, value) in headers {
        let token_char = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b);
        if name.is_empty() || !name.bytes().all(token_char) {
            return Err(format!(
                "{method} {url}: invalid HTTP header name {name:?} (it contains a line break or another character a header name can't have)"
            ));
        }
        if control(value) {
            return Err(format!(
                "{method} {url}: the `{name}` header contains a line break or another control character"
            ));
        }
    }
    Ok(())
}

/// Whether `url` is `https://`, or plain `http://` to this machine (`localhost` or a loopback
/// address): where the tools send credentials and download releases from.
pub fn is_tls_or_loopback(url: &str) -> bool {
    url_host(url).is_ok_and(|h| h.tls || h.is_loopback())
}

/// The host part of a URL as this client reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UrlHost<'a> {
    /// `https://` (TLS) rather than `http://`.
    pub tls: bool,
    /// `host`, `host:port`, `[v6]` or `[v6]:port`.
    pub authority: &'a str,
}

impl UrlHost<'_> {
    /// The host name or address without port and brackets.
    pub fn host(&self) -> &str {
        match self.authority.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or(v6),
            None => self.authority.split(':').next().unwrap_or(self.authority),
        }
    }

    /// Whether the host is this machine: `localhost` or a loopback address.
    pub fn is_loopback(&self) -> bool {
        let host = self.host();
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    }
}

/// The host part of an `http://` or `https://` URL (scheme in any case): everything between the
/// scheme and the first `/`, which is what this client connects to. It is refused when empty or
/// when it contains `@`, `?`, `#` or `\`: other parsers (a token's loopback rule, a browser)
/// would read another host there (`localhost` in `http://localhost?.attacker.example/`). The one
/// parser of a registry URL's host, for this client and for vpm's token rules.
pub fn url_host(url: &str) -> Result<UrlHost<'_>, String> {
    let scheme_is = |scheme: &str| {
        url.get(..scheme.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(scheme))
    };
    let (tls, rest) = if scheme_is("https://") {
        (true, &url["https://".len()..])
    } else if scheme_is("http://") {
        (false, &url["http://".len()..])
    } else {
        return Err(format!(
            "unsupported URL `{url}` (expected http:// or https://)"
        ));
    };
    let (authority, _) = split(rest);
    if authority.is_empty() || authority.contains(['@', '?', '#', '\\']) {
        return Err(
            "the host part may not be empty or contain `@`, `?`, `#` or `\\` (write the URL with a `/` after the host)"
                .into(),
        );
    }
    Ok(UrlHost { tls, authority })
}

/// `host[:port]` and `/path?query` of a URL without its scheme.
fn split(rest: &str) -> (&str, &str) {
    match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    }
}

/// `host` with `default_port` unless it names a port.
fn with_port(host: &str, default_port: u16) -> String {
    if split_port(host).is_some() {
        host.to_string()
    } else {
        format!("{host}:{default_port}")
    }
}

/// A connection to `host` (each of its addresses in turn, `limits.connect` each but never past
/// the deadline), whose reads and writes fail once the deadline has passed.
fn connect(host: &str, default_port: u16, budget: Budget) -> Result<Deadlined, String> {
    let Budget { limits, deadline } = budget;
    let authority = with_port(host, default_port);
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
    budget: Budget,
    via: Option<&Proxy>,
) -> Result<Response, String> {
    let (host, path) = split(rest);
    let max = budget.limits.max_body;
    let Some(proxy) = via else {
        let head = request_head(method, host, path, headers, body.len(), None);
        return exchange(connect(host, 80, budget)?, &head, body, max);
    };
    let target = absolute_form(host, path);
    let auth = proxy.authorization.as_deref();
    let head = request_head(method, host, &target, headers, body.len(), auth);
    exchange(to_proxy(proxy, budget)?, &head, body, max)
}

/// The whole URL as the request target for a proxy, which connects to the origin: `http://`, the
/// host and port, and the path. A user and password the URL names are never part of it (RFC 9110
/// §4.2.4): the proxy is a third party. ([`url_host`] already refuses a URL with `@` in its host
/// part; this holds without that check too.)
fn absolute_form(authority: &str, path: &str) -> String {
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    format!("http://{host}{path}")
}

/// One request over TLS, verified with `config`, within the default limits and directly.
#[cfg(test)]
pub(crate) fn https(
    method: &str,
    rest: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    config: Arc<ClientConfig>,
) -> Result<Response, String> {
    https_via(method, rest, headers, body, config, None)
}

/// [`https`] through `via`.
#[cfg(test)]
pub(crate) fn https_via(
    method: &str,
    rest: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    config: Arc<ClientConfig>,
    via: Option<&Proxy>,
) -> Result<Response, String> {
    let limits = Limits::DEFAULT;
    let budget = Budget {
        limits,
        deadline: Instant::now() + limits.total,
    };
    https_within(method, rest, headers, body, config, budget, via)
}

/// One request over TLS, verified with `config`; through a `CONNECT` tunnel of `via`.
fn https_within(
    method: &str,
    rest: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    config: Arc<ClientConfig>,
    budget: Budget,
    via: Option<&Proxy>,
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
    let socket = match via {
        Some(proxy) => tunnel(to_proxy(proxy, budget)?, proxy, &with_port(host, 443))?,
        None => connect(host, 443, budget)?,
    };
    let head = request_head(method, host, path, headers, body.len(), None);
    exchange(
        StreamOwned::new(conn, socket),
        &head,
        body,
        budget.limits.max_body,
    )
}

/// A connection to `proxy`.
fn to_proxy(proxy: &Proxy, budget: Budget) -> Result<Deadlined, String> {
    connect(&proxy.authority, 80, budget).map_err(|e| format!("proxy {}: {e}", proxy.authority))
}

/// The longest answer to `CONNECT` read.
const MAX_TUNNEL_HEAD: usize = 16 << 10;

/// `stream` to `proxy` turned into a tunnel to `authority` (`host:port`): `CONNECT`, and a 2xx
/// answer, read byte by byte so nothing of the origin's first bytes is taken from the stream.
fn tunnel(mut stream: Deadlined, proxy: &Proxy, authority: &str) -> Result<Deadlined, String> {
    let at = &proxy.authority;
    let mut head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(auth) = &proxy.authorization {
        head.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| format!("proxy {at}: cannot send: {e}"))?;
    let mut got = Vec::new();
    let mut byte = [0u8; 1];
    while !got.ends_with(b"\r\n\r\n") {
        if got.len() > MAX_TUNNEL_HEAD {
            return Err(format!("proxy {at}: the answer to CONNECT is too long"));
        }
        match stream.read(&mut byte) {
            Ok(0) => return Err(format!("proxy {at}: closed the connection on CONNECT")),
            Ok(_) => got.push(byte[0]),
            Err(e) => return Err(format!("proxy {at}: {e}")),
        }
    }
    let text = String::from_utf8_lossy(&got);
    let line = text.lines().next().unwrap_or("");
    let status = line.split(' ').nth(1).and_then(|s| s.parse::<u16>().ok());
    match status {
        Some(200..=299) => Ok(stream),
        _ => Err(format!(
            "proxy {at} refused the tunnel to {authority}: {}",
            line.chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect::<String>()
        )),
    }
}

/// The request line and headers: `target` is the path, or the whole URL for a proxy.
fn request_head(
    method: &str,
    host: &str,
    target: &str,
    headers: &[(&str, &str)],
    body_len: usize,
    proxy_auth: Option<&str>,
) -> String {
    let mut head = format!(
        "{method} {target} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {body_len}\r\nConnection: close\r\n"
    );
    if let Some(auth) = proxy_auth {
        head.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
    }
    for (n, v) in headers {
        head.push_str(&format!("{n}: {v}\r\n"));
    }
    head.push_str("\r\n");
    head
}

fn exchange(
    mut stream: impl Read + Write,
    head: &str,
    body: &[u8],
    max_body: usize,
) -> Result<Response, String> {
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body))
        .and_then(|()| stream.flush())
        .map_err(|e| format!("cannot send: {e}"))?;
    read_response(&mut BufReader::new(stream), max_body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_absolute_form_never_names_the_urls_user() {
        assert_eq!(
            absolute_form("bob:TARGETPW@origin.test:8080", "/a?b=c"),
            "http://origin.test:8080/a?b=c"
        );
        assert_eq!(absolute_form("a@b@origin.test", "/"), "http://origin.test/");
        assert_eq!(absolute_form("[::1]:80", "/x"), "http://[::1]:80/x");
    }

    #[test]
    fn url_hosts() {
        let h = url_host("HTTPS://Reg.example.com:8443/api?q=1").unwrap();
        assert!(h.tls);
        assert_eq!(
            (h.authority, h.host()),
            ("Reg.example.com:8443", "Reg.example.com")
        );
        let h = url_host("http://[::1]:8091/").unwrap();
        assert!(!h.tls && h.is_loopback());
        assert_eq!(h.host(), "::1");
        assert!(url_host("http://127.1.2.3").unwrap().is_loopback());
        assert!(url_host("http://LOCALHOST:1/x").unwrap().is_loopback());
        assert!(!url_host("http://localhost.example.com")
            .unwrap()
            .is_loopback());
        for bad in [
            "http://localhost?.a.example/",
            "http://a@b/",
            "http:///x",
            "http://a\\b",
        ] {
            assert!(url_host(bad).unwrap_err().contains("host part"), "{bad}");
        }
        assert!(url_host("ftp://x").unwrap_err().contains("unsupported"));
    }

    #[test]
    fn control_characters_and_ambiguous_hosts_never_reach_the_request() {
        // Refused before connecting: port 1 would fail with "cannot connect" otherwise.
        let url = "http://127.0.0.1:1/x";
        for token in ["abc\r\nX-Evil: 1", "abc\n", "abc\r", "a\0b", "tab\there"] {
            let auth = format!("Bearer {token}");
            let err = fetch("PUT", url, &[("Authorization", &auth)], b"").unwrap_err();
            assert_eq!(
                err,
                "PUT http://127.0.0.1:1/x: the `Authorization` header contains a line break or another control character"
            );
        }
        for bad in [
            "http://127.0.0.1:1/a\r\nHost: evil",
            "http://127.0.0.1:1/a b",
        ] {
            let err = fetch("GET", bad, &[], b"").unwrap_err();
            assert!(err.contains("in the request line"), "{err}");
        }
        let err = fetch("GET\r\n", url, &[], b"").unwrap_err();
        assert!(err.starts_with("invalid HTTP method"), "{err}");
        let err = fetch("GET", url, &[("X-A\r\nB", "v")], b"").unwrap_err();
        assert!(err.contains("invalid HTTP header name"), "{err}");
        // The host ends at the first `/`: `?`, `#`, `@` or `\` before it would let another parser
        // (the token's loopback rule) see `localhost` where this client connects elsewhere.
        for bad in [
            "http://localhost?.attacker.example/",
            "http://localhost#.attacker.example/",
            "http://localhost@attacker.example/",
            "http://127.0.0.1:1@attacker.example/",
            "http://localhost\\.attacker.example/",
            "https://registry.example.com?x/",
            "http:///path",
        ] {
            let err = fetch("GET", bad, &[], b"").unwrap_err();
            assert!(err.contains("the host part may not"), "{bad}: {err}");
        }
        // Ordinary requests pass the check (and then fail to connect), `?` after the path too.
        for ok in [url, "http://127.0.0.1:1/search?q=a#b", "http://127.0.0.1:1"] {
            let err = fetch("GET", ok, &[("Authorization", "Bearer 0123abcd")], b"").unwrap_err();
            assert!(err.contains("cannot connect"), "{ok}: {err}");
        }
    }

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
    /// The client gives up after 0.7 s, while the response (about 7 s of bytes) is still coming:
    /// checked by how much the server got to send, not by the clock, which a loaded machine
    /// stretches.
    #[test]
    fn a_dripping_server_hits_the_deadline() {
        const HEAD: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n";
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = conn.read(&mut request);
            let mut sent = 0;
            for b in HEAD.iter().cycle() {
                if conn.write_all(&[*b]).is_err() {
                    break;
                }
                sent += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            sent
        });
        let limits = Limits {
            connect: Duration::from_secs(5),
            idle: Duration::from_secs(5),
            total: Duration::from_millis(700),
            ..Limits::DEFAULT
        };
        let err = fetch_within("GET", &format!("http://{addr}/"), &[], b"", limits).unwrap_err();
        assert!(err.contains("took too long"), "{err}");
        // At most one byte per 50 ms, so 100 bytes take at least 5 s: far past the deadline,
        // however slowly a loaded machine runs either side.
        let sent = server.join().unwrap();
        assert!(
            sent < 100,
            "the server sent {sent} bytes before the client gave up"
        );
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
            assert!(
                !err.contains('\n'),
                "the message stays on one line: {err:?}"
            );
        }
        let err = fetch("GET", &format!("{url}x\r\nX: y"), &[], b"").unwrap_err();
        assert!(err.contains("a line break"), "{err}");
        assert!(err.contains("in the request line"), "{err}");
        assert!(listener.accept().is_err(), "no connection was made");
    }

    #[test]
    fn a_body_over_the_limit_is_refused() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (len, header) in [(1000, true), (1000, false), (50, true)] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0; 1024];
                let _ = stream.read(&mut buf);
                let head = if header {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\n\r\n")
                } else {
                    "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_string()
                };
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&vec![b'x'; len]);
            }
        });
        let limits = Limits {
            max_body: 100,
            ..Limits::DEFAULT
        };
        let url = format!("http://{addr}/");
        let err = fetch_within("GET", &url, &[], b"", limits).unwrap_err();
        assert!(err.contains("exceeds the limit of 100"), "{err}");
        let err = fetch_within("GET", &url, &[], b"", limits).unwrap_err();
        assert!(err.contains("exceeds the limit of 100"), "{err}");
        let ok = fetch_within("GET", &url, &[], b"", limits).unwrap();
        assert_eq!(ok.body.len(), 50);
        server.join().unwrap();
    }
}
