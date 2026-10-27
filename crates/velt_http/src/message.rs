//! Requests and responses, and their HTTP/1.1 wire format.

use std::io::{BufRead, Read, Write};

/// Largest request/response head (request line + headers) accepted.
const MAX_HEAD: usize = 64 * 1024;

/// A parsed request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// `GET`, `PUT`, ... (as sent).
    pub method: String,
    /// The path without the query string, percent-decoding not applied.
    pub path: String,
    /// The query string without `?` (empty if none).
    pub query: String,
    /// Header names lowercased, in order.
    pub headers: Vec<(String, String)>,
    /// The body (`Content-Length` bytes).
    pub body: Vec<u8>,
}

impl Request {
    /// The first header named `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }
}

/// A response to send (server) or that was received (client).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// Status code.
    pub status: u16,
    /// Headers (names as given; `Content-Length` and `Connection` are added when writing).
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl Response {
    /// A response with `body` of type `content_type`.
    pub fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Response {
        Response {
            status,
            headers: vec![("Content-Type".into(), content_type.into())],
            body,
        }
    }

    /// A `text/plain` response.
    pub fn text(status: u16, body: impl Into<String>) -> Response {
        Response::bytes(
            status,
            "text/plain; charset=utf-8",
            body.into().into_bytes(),
        )
    }

    /// Add a header.
    pub fn with_header(mut self, name: &str, value: &str) -> Response {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// The first header named `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }

    /// The body as (lossy) UTF-8 text.
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Read the head lines up to the blank line.
fn read_head(r: &mut impl BufRead) -> Result<Vec<String>, String> {
    let mut lines = vec![];
    let mut total = 0;
    loop {
        let mut line = String::new();
        let n = r
            .read_line(&mut line)
            .map_err(|e| format!("cannot read HTTP head: {e}"))?;
        total += n;
        if n == 0 {
            return Err("connection closed before the end of the HTTP head".into());
        }
        if total > MAX_HEAD {
            return Err("HTTP head too large".into());
        }
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            return Ok(lines);
        }
        lines.push(line);
    }
}

fn parse_headers(lines: &[String]) -> Result<Vec<(String, String)>, String> {
    lines
        .iter()
        .map(|l| match l.split_once(':') {
            Some((n, v)) => Ok((n.trim().to_ascii_lowercase(), v.trim().to_string())),
            None => Err(format!("malformed header line `{l}`")),
        })
        .collect()
}

fn read_body(
    r: &mut impl BufRead,
    headers: &[(String, String)],
    max: usize,
) -> Result<Vec<u8>, String> {
    let len = match find_header(headers, "content-length") {
        Some(v) => v
            .parse::<usize>()
            .map_err(|_| format!("invalid Content-Length `{v}`"))?,
        None => 0,
    };
    if len > max {
        return Err(format!("body of {len} bytes exceeds the limit of {max}"));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)
        .map_err(|e| format!("cannot read HTTP body: {e}"))?;
    Ok(body)
}

/// Read one request; bodies larger than `max_body` bytes are rejected.
pub fn read_request(r: &mut impl BufRead, max_body: usize) -> Result<Request, String> {
    let head = read_head(r)?;
    let (line, rest) = head.split_first().ok_or("empty HTTP request")?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target), Some(_version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(format!("malformed request line `{line}`"));
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let headers = parse_headers(rest)?;
    let body = read_body(r, &headers, max_body)?;
    Ok(Request {
        method: method.into(),
        path: path.into(),
        query: query.into(),
        headers,
        body,
    })
}

/// A body in chunked transfer encoding (RFC 9112 §7.1): hex-sized chunks up to a zero-sized one,
/// then trailer lines, which are skipped. A server may answer this way whatever the request.
fn read_chunked(r: &mut impl BufRead, max: usize) -> Result<Vec<u8>, String> {
    let mut body = vec![];
    loop {
        let mut line = String::new();
        r.read_line(&mut line)
            .map_err(|e| format!("cannot read HTTP body: {e}"))?;
        let size = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size, 16)
            .map_err(|_| format!("invalid chunk size `{}`", line.trim()))?;
        if size == 0 {
            break;
        }
        if body.len() + size > max {
            return Err(format!("body exceeds the limit of {max} bytes"));
        }
        let start = body.len();
        body.resize(start + size, 0);
        r.read_exact(&mut body[start..])
            .map_err(|e| format!("cannot read HTTP body: {e}"))?;
        let mut end = [0u8; 2];
        r.read_exact(&mut end)
            .map_err(|e| format!("cannot read HTTP body: {e}"))?;
        if &end != b"\r\n" {
            return Err("malformed chunked body: a chunk does not end with CRLF".into());
        }
    }
    loop {
        let mut trailer = String::new();
        let n = r
            .read_line(&mut trailer)
            .map_err(|e| format!("cannot read HTTP body: {e}"))?;
        if n == 0 || trailer.trim().is_empty() {
            return Ok(body);
        }
    }
}

/// Read one response (the client side); bodies larger than `max_body` bytes are rejected.
pub fn read_response(r: &mut impl BufRead, max_body: usize) -> Result<Response, String> {
    let head = read_head(r)?;
    let (line, rest) = head.split_first().ok_or("empty HTTP response")?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("malformed status line `{line}`"))?;
    let headers = parse_headers(rest)?;
    let chunked = find_header(&headers, "transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let body = if chunked {
        read_chunked(r, max_body)?
    } else if find_header(&headers, "content-length").is_some() {
        read_body(r, &headers, max_body)?
    } else {
        let mut body = vec![];
        r.take(max_body as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|e| format!("cannot read HTTP body: {e}"))?;
        if body.len() > max_body {
            return Err(format!("body exceeds the limit of {max_body} bytes"));
        }
        body
    };
    Ok(Response {
        status,
        headers,
        body,
    })
}

/// The reason phrase for the status codes the tools use.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        _ => "",
    }
}

/// Write `resp` (with `Content-Length` and `Connection: close`).
pub fn write_response(w: &mut impl Write, resp: &Response) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", resp.status, reason(resp.status));
    for (n, v) in &resp.headers {
        head.push_str(&format!("{n}: {v}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        resp.body.len()
    ));
    w.write_all(head.as_bytes())?;
    w.write_all(&resp.body)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip() {
        let raw = b"PUT /api/x?y=1 HTTP/1.1\r\nHost: h\r\nContent-Length: 3\r\nX-Token: t\r\n\r\nabcEXTRA";
        let req = read_request(&mut &raw[..], 10).unwrap();
        assert_eq!((req.method.as_str(), req.path.as_str()), ("PUT", "/api/x"));
        assert_eq!(req.query, "y=1");
        assert_eq!(req.header("x-TOKEN"), Some("t"));
        assert_eq!(req.body, b"abc");
        let err = read_request(&mut &raw[..], 2).unwrap_err();
        assert!(err.contains("exceeds"), "{err}");
        assert!(read_request(&mut &b"GET\r\n\r\n"[..], 1).is_err());
        assert!(read_request(&mut &b"GET / HTTP/1.1\r\nHost"[..], 1).is_err());
    }

    #[test]
    fn response_round_trip() {
        let resp = Response::text(404, "nope").with_header("X-A", "b");
        let mut wire = vec![];
        write_response(&mut wire, &resp).unwrap();
        let text = String::from_utf8(wire.clone()).unwrap();
        assert!(text.starts_with("HTTP/1.1 404 Not Found\r\n"));
        let back = read_response(&mut &wire[..], 100).unwrap();
        assert_eq!(back.status, 404);
        assert_eq!(back.body_text(), "nope");
        assert_eq!(back.header("x-a"), Some("b"));
    }

    #[test]
    fn chunked_response_bodies_are_decoded() {
        let wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                     4;ext=1\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\nX-Trailer: t\r\n\r\n";
        let back = read_response(&mut &wire[..], 100).unwrap();
        assert_eq!(back.body_text(), "{\"a\":1}");
        let err = read_response(&mut &wire[..], 5).unwrap_err();
        assert!(err.contains("exceeds the limit"), "{err}");
        let bad = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n";
        assert!(read_response(&mut &bad[..], 100).is_err());
    }
}
