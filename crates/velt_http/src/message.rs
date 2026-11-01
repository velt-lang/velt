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

/// Largest chunk-size or trailer line accepted in a chunked body.
const MAX_CHUNK_LINE: usize = 8 * 1024;

/// Whether the transfer codings are exactly `chunked` (the client asks for no others, and a body
/// with another coding left on it after the chunks would be unreadable).
fn is_chunked_only(codings: &str) -> bool {
    let mut parts = codings.split(',').map(str::trim).filter(|c| !c.is_empty());
    parts
        .next()
        .is_some_and(|c| c.eq_ignore_ascii_case("chunked"))
        && parts.next().is_none()
}

/// Read one line of a chunked body, at most [`MAX_CHUNK_LINE`] bytes, without its CRLF (or bare
/// LF). `Ok(None)` at the end of the input.
fn read_chunk_line(r: &mut impl BufRead) -> Result<Option<Vec<u8>>, String> {
    let mut line = vec![];
    let n = r
        .by_ref()
        .take(MAX_CHUNK_LINE as u64 + 1)
        .read_until(b'\n', &mut line)
        .map_err(|e| format!("cannot read HTTP body: {e}"))?;
    if n == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(if line.len() > MAX_CHUNK_LINE {
            "malformed chunked body: a chunk-size or trailer line is too long".into()
        } else {
            "connection closed in the middle of a chunked body".into()
        });
    }
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(Some(line))
}

/// The size on a chunk-size line: hex digits, then optional whitespace and `;name=value`
/// extensions, which are ignored.
fn parse_chunk_size(line: &[u8]) -> Result<usize, String> {
    let digits = line.iter().take_while(|b| b.is_ascii_hexdigit()).count();
    let rest = &line[digits..];
    let rest_ok = rest
        .iter()
        .position(|&b| b == b';')
        .map_or(rest, |i| &rest[..i])
        .iter()
        .all(|&b| b == b' ' || b == b'\t');
    let invalid = || {
        format!(
            "malformed chunked body: invalid chunk size `{}`",
            String::from_utf8_lossy(line)
        )
    };
    if digits == 0 || !rest_ok {
        return Err(invalid());
    }
    let hex = std::str::from_utf8(&line[..digits]).map_err(|_| invalid())?;
    usize::from_str_radix(hex, 16).map_err(|_| invalid())
}

/// A body in chunked transfer encoding (RFC 9112 §7.1): hex-sized chunks up to a zero-sized one,
/// then trailer fields up to a blank line. Chunk extensions and trailers are skipped; the decoded
/// body is capped at `max` bytes.
fn read_chunked(r: &mut impl BufRead, max: usize) -> Result<Vec<u8>, String> {
    let mut body = vec![];
    loop {
        let line =
            read_chunk_line(r)?.ok_or("connection closed in the middle of a chunked body")?;
        let size = parse_chunk_size(&line)?;
        if size == 0 {
            break;
        }
        if body.len().checked_add(size).is_none_or(|total| total > max) {
            return Err(format!("body exceeds the limit of {max} bytes"));
        }
        let start = body.len();
        body.resize(start + size, 0);
        r.read_exact(&mut body[start..])
            .map_err(|e| format!("cannot read HTTP body: {e}"))?;
        match read_chunk_line(r)? {
            Some(end) if end.is_empty() => {}
            Some(_) => return Err("malformed chunked body: a chunk is longer than its size".into()),
            None => return Err("connection closed in the middle of a chunked body".into()),
        }
    }
    // Trailer fields. A server that closes the connection right after the last chunk is
    // tolerated: the body is complete.
    let mut trailers = 0;
    while let Some(line) = read_chunk_line(r)? {
        if line.is_empty() {
            break;
        }
        trailers += line.len();
        if trailers > MAX_HEAD {
            return Err("malformed chunked body: trailer fields too large".into());
        }
    }
    Ok(body)
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
    // `Transfer-Encoding` overrides `Content-Length` (RFC 9112 §6.3).
    let body = if let Some(codings) = find_header(&headers, "transfer-encoding") {
        if !is_chunked_only(codings) {
            return Err(format!("unsupported Transfer-Encoding `{codings}`"));
        }
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

    fn chunked(body: &str) -> Vec<u8> {
        format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{body}").into_bytes()
    }

    #[test]
    fn chunked_response_bodies_are_decoded() {
        let wire = chunked("4;ext=1;q=\"a;b\"\r\n{\"a\"\r\n3 ; x\r\n:1}\r\nA\r\n0123456789\r\n0\r\nX-Trailer: t\r\n\r\nNEXT");
        let back = read_response(&mut &wire[..], 100).unwrap();
        assert_eq!(back.body_text(), "{\"a\":1}0123456789");
        assert_eq!(back.status, 200);
        // Bare LF line ends, upper-case hex, and a connection closed right after the last chunk.
        let wire = chunked("1E\n0123456789abcdef0123456789abcd\n0\n");
        let back = read_response(&mut &wire[..], 100).unwrap();
        assert_eq!(back.body.len(), 30);
        // Transfer-Encoding wins over Content-Length.
        let wire = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nTransfer-Encoding: Chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n";
        assert_eq!(read_response(&mut &wire[..], 100).unwrap().body, b"abc");
        // An empty body.
        let back = read_response(&mut &chunked("0\r\n\r\n")[..], 0).unwrap();
        assert!(back.body.is_empty());
    }

    #[test]
    fn malformed_chunked_bodies_are_errors() {
        let err = |body: &str, max: usize| read_response(&mut &chunked(body)[..], max).unwrap_err();
        let full = "4\r\nabcd\r\n3\r\nefg\r\n0\r\n\r\n";
        assert!(err(full, 5).contains("exceeds the limit"));
        assert!(err(full, 6).contains("exceeds the limit"));
        assert!(read_response(&mut &chunked(full)[..], 7).is_ok());
        for bad in [
            "zz\r\n",
            "\r\nabc\r\n0\r\n\r\n",
            "+4\r\nabcd\r\n0\r\n\r\n",
            "-1\r\nx\r\n0\r\n\r\n",
            "4x\r\nabcd\r\n0\r\n\r\n",
            "0x4\r\nabcd\r\n0\r\n\r\n",
        ] {
            assert!(err(bad, 100).contains("invalid chunk size"), "{bad:?}");
        }
        // Sizes that overflow a usize, or the running total.
        assert!(err("fffffffffffffffffffffff\r\n", 100).contains("invalid chunk size"));
        assert!(err("1\r\na\r\nffffffffffffffff\r\n", usize::MAX).contains("exceeds the limit"));
        // A chunk longer than its size, or not ended by CRLF.
        assert!(err("2\r\nabcd\r\n0\r\n\r\n", 100).contains("longer than its size"));
        // Truncated: in a size line, in the data, after the data, before the last chunk.
        assert!(err("4", 100).contains("closed"));
        assert!(err("4\r\nab", 100).contains("cannot read HTTP body"));
        assert!(err("4\r\nabcd", 100).contains("closed"));
        assert!(err("4\r\nabcd\r\n", 100).contains("closed"));
        // Over-long size and trailer lines.
        let long = format!("1;{}\r\na\r\n0\r\n\r\n", "x".repeat(MAX_CHUNK_LINE));
        assert!(err(&long, 100).contains("too long"));
        let many = format!("0\r\n{}\r\n", "X-T: aaaaaaaa\r\n".repeat(MAX_HEAD / 8));
        assert!(err(&many, 100).contains("trailer fields too large"));
        // Codings other than chunked alone.
        let gz = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n0\r\n\r\n";
        let e = read_response(&mut &gz[..], 100).unwrap_err();
        assert!(e.contains("unsupported Transfer-Encoding"), "{e}");
    }
}
