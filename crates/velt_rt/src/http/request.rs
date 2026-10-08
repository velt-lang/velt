//! Incoming HTTP request object (`VeltReq*`) and its accessors: what std/fetch's global `Request`
//! reads, lazily, for a request a server received.
//!
//! The head (method, URL, headers) is there when the handler starts, so its accessors are
//! synchronous; the body is received while the handler reads it (`req_body.rs`), so its readers
//! are async. A request is a key into a registry (`crate::registry`): the handler releases it
//! with `velt_rt_http_req_drop`, and any later use (a `Request` captured by a streamed body or
//! a spawned task that outlives its handler) is a clear runtime error instead of a read of freed
//! memory. Accessors return owned copies, so their results never dangle.

use super::client::text_of;
use super::owned_str;
use super::req_body::ReqBody;
use crate::bytes::VeltBytes;
use crate::registry::{Key, Registry};
use crate::result::IoResult;
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use hyper::body::Incoming;
use hyper::header::HOST;
use hyper::Request;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

/// A request handle (a registry key), owned by the handler once it starts.
pub type ReqHandle = Key<ReqObj>;

static REQUESTS: Registry<ReqObj> = Registry::new();

/// Register a request read by the server; the handler receives the returned key.
pub fn register(req: ReqObj) -> ReqHandle {
    REQUESTS.insert(req)
}

/// The request behind `req`, or a fatal error when it was already released.
fn obj(req: ReqHandle) -> Arc<ReqObj> {
    REQUESTS.get(req).unwrap_or_else(|| {
        crate::panic::fatal(concat!(
            "a Request was used after its handler finished (e.g. in a streamed body or a ",
            "spawned task): read what you need first, as in `const url = req.url`"
        ))
    })
}

/// The connection a request came in on.
#[derive(Clone, Copy, Debug)]
pub struct Conn {
    /// The client's address (`info.remoteAddr`).
    pub remote: SocketAddr,
    /// The server's address on this connection: the URL's host when the request names none.
    pub local: SocketAddr,
    /// Served over TLS (`https:` URLs).
    pub tls: bool,
}

/// Opaque request (`VeltReq` in the ABI docs).
pub struct ReqObj {
    parts: hyper::http::request::Parts,
    /// `None` once a read took it for good, or when the request has none.
    body: Mutex<Option<ReqBody>>,
    /// Whether the request arrived with a body (`req.body` is null without one).
    has_body: bool,
    /// Key of the parked HTTP upgrade (`upgrade.rs`); 0 = not an upgrade request.
    upgrade: u64,
    conn: Conn,
}

/// HTTP/2 may split `cookie` into one field per crumb (RFC 9113 §8.2.3); join them back with
/// `"; "` into one field, as Node does, so `headers.get("cookie")` reads as over HTTP/1.1
/// (where the general `", "` join would corrupt the cookie list).
fn join_cookies(headers: &mut hyper::HeaderMap) {
    let crumbs: Vec<&[u8]> = headers
        .get_all(hyper::header::COOKIE)
        .iter()
        .map(|v| v.as_bytes())
        .collect();
    if crumbs.len() < 2 {
        return;
    }
    let joined = crumbs.join(&b"; "[..]);
    if let Ok(v) = hyper::header::HeaderValue::from_bytes(&joined) {
        headers.insert(hyper::header::COOKIE, v);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ReqObj {
    /// A request whose head has arrived; its body is received when the handler reads it.
    /// `upgrade` is the key of its parked upgrade, 0 if none. Not boxed: it moves into the
    /// registry's allocation (`register`) without a box of its own on the way.
    pub fn new(req: Request<Incoming>, upgrade: u64, conn: Conn) -> ReqObj {
        let (mut parts, body) = req.into_parts();
        if parts.version == hyper::Version::HTTP_2 {
            join_cookies(&mut parts.headers);
        }
        let body = ReqBody::new(body);
        ReqObj {
            parts,
            has_body: body.is_some(),
            body: Mutex::new(body),
            upgrade,
            conn,
        }
    }
}

/// A request's absolute URL: `http(s)://`, the `host` header (HTTP/2: `:authority`; without
/// either, the server's address), then the path and query, as Deno and Bun give it. A host
/// that is not UTF-8 is decoded lossily.
fn url_of(parts: &hyper::http::request::Parts, conn: &Conn) -> VeltStr {
    let uri = &parts.uri;
    let path = uri.path_and_query().map_or("/", |p| p.as_str()).as_bytes();
    if let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) {
        // Absolute form (HTTP/2, or a proxy request over HTTP/1.1).
        let parts = [
            scheme.as_bytes(),
            b"://",
            authority.as_str().as_bytes(),
            path,
        ];
        return joined(&parts);
    }
    let scheme: &[u8] = if conn.tls { b"https://" } else { b"http://" };
    // A scan, not a hashed lookup: `host` is a request's first header, or close to it.
    let host = parts.headers.iter().find(|(name, _)| *name == HOST);
    match host.map(|(_, h)| h.as_bytes()) {
        Some(host) if !host.is_empty() => joined(&[scheme, host, path]),
        _ => joined(&[scheme, conn.local.to_string().as_bytes(), path]),
    }
}

/// `parts` joined into one string, built on the stack when short (a URL usually is), so the
/// string's own buffer is the only allocation. ASCII (the scheme, path and query always are, a
/// host nearly always) is taken as it is; anything else is decoded lossily.
fn joined(parts: &[&[u8]]) -> VeltStr {
    let len = parts.iter().map(|p| p.len()).sum();
    let mut stack = [0u8; 256];
    let mut heap = Vec::new();
    let buf: &mut [u8] = if len <= stack.len() {
        &mut stack[..len]
    } else {
        heap.resize(len, 0);
        &mut heap
    };
    let mut at = 0;
    for p in parts {
        buf[at..at + p.len()].copy_from_slice(p);
        at += p.len();
    }
    if buf.is_ascii() {
        // SAFETY: ASCII is UTF-8 with one UTF-16 unit per byte.
        return unsafe { VeltStr::from_text_counted(std::str::from_utf8_unchecked(buf), len) };
    }
    text_of(buf)
}

/// The key std/websocket passes to `velt_rt_ws_accept`; 0 if the request asked for no upgrade.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_upgrade(req: ReqHandle) -> u64 {
    obj(req).upgrade
}

/// `req.method` (`GET`, `POST`...).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_method(req: ReqHandle, out: *mut VeltStr) {
    out.write(owned_str(obj(req).parts.method.as_str()));
}

/// `req.url`: the absolute URL (`http://host/path?query`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_url(req: ReqHandle, out: *mut VeltStr) {
    let r = obj(req);
    out.write(url_of(&r.parts, &r.conn));
}

/// `req.headers.get(name)` (case-insensitive): writes `out` (repeated fields joined with
/// `", "`) and returns 1, or returns 0 if absent. Non-UTF-8 header bytes are decoded lossily.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_header(
    req: ReqHandle,
    name: *const VeltStr,
    out: *mut VeltStr,
) -> u8 {
    let name = (*name).text_lossy();
    let r = obj(req);
    let mut values = r.parts.headers.get_all(name.as_ref()).iter();
    let Some(first) = values.next() else {
        return 0;
    };
    let mut joined = first.as_bytes().to_vec();
    for v in values {
        joined.extend_from_slice(b", ");
        joined.extend_from_slice(v.as_bytes());
    }
    out.write(text_of(&joined));
    1
}

/// Every header as the flat list `[name, value, …]` (lowercase names, received order; values
/// that are not UTF-8 decoded lossily): `req.headers` (the global `Headers`) in one call.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_headers(req: ReqHandle, out: *mut VeltStrArray) {
    let r = obj(req);
    let mut flat = Vec::with_capacity(r.parts.headers.len() * 2);
    for (name, value) in r.parts.headers.iter() {
        flat.push(owned_str(name.as_str()));
        flat.push(text_of(value.as_bytes()));
    }
    out.write(VeltStrArray::from_vec(flat));
}

/// Whether the request arrived with a body (a GET or an empty POST has none).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_has_body(req: ReqHandle) -> u8 {
    obj(req).has_body as u8
}

/// `info.remoteAddr.hostname`: the client's IP address.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_remote_host(req: ReqHandle, out: *mut VeltStr) {
    out.write(owned_str(&obj(req).conn.remote.ip().to_string()));
}

/// `info.remoteAddr.port`: the client's port.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_remote_port(req: ReqHandle) -> u32 {
    obj(req).conn.remote.port() as u32
}

/// The whole body (empty without one; std reads each body once).
async fn receive(r: Arc<ReqObj>) -> Result<Vec<u8>, crate::result::VeltErr> {
    let body = lock(&r.body).take();
    match body {
        Some(body) => body.read_all().await,
        None => Ok(Vec::new()),
    }
}

/// `await req.text()` → result slot `IoResult<VeltStr>`: the body as UTF-8, invalid bytes
/// decoded to U+FFFD and a leading byte order mark dropped (as JS decodes).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_text(req: ReqHandle) -> *mut VeltFut {
    let r = obj(req);
    new_leaf(async move {
        match receive(r).await {
            Ok(bytes) => {
                let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
                IoResult::ok(text_of(text))
            }
            Err(e) => IoResult::<VeltStr>::err(e),
        }
    })
}

/// `await req.bytes()` → result slot `IoResult<VeltBytes>` (the received buffer, not copied
/// again).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_bytes(req: ReqHandle) -> *mut VeltFut {
    let r = obj(req);
    new_leaf(async move {
        match receive(r).await {
            Ok(bytes) => IoResult::ok(VeltBytes::from_vec(bytes)),
            Err(e) => IoResult::<VeltBytes>::err(e),
        }
    })
}

/// `req.body`'s next chunk → result slot `IoResult<VeltBytes>`: the next bytes, never empty; an
/// empty array once the body is complete (and on every read after that).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_chunk(req: ReqHandle) -> *mut VeltFut {
    let r = obj(req);
    new_leaf(async move {
        let Some(mut body) = lock(&r.body).take() else {
            return IoResult::ok(VeltBytes::from_vec(vec![]));
        };
        match body.next().await {
            Ok(chunk) => {
                if !body.is_done() {
                    *lock(&r.body) = Some(body);
                }
                IoResult::ok(VeltBytes::from_vec(chunk.unwrap_or_default()))
            }
            Err(e) => IoResult::<VeltBytes>::err(e),
        }
    })
}

/// Free a request.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_drop(req: ReqHandle) {
    REQUESTS.remove(req);
}

#[cfg(test)]
mod tests {
    use super::{join_cookies, url_of, Conn};
    use hyper::header::{HeaderMap, HeaderValue, COOKIE};

    fn url(builder: hyper::http::request::Builder, tls: bool) -> String {
        let (parts, ()) = builder.body(()).unwrap().into_parts();
        let local = "10.0.0.1:8080".parse().unwrap();
        let conn = Conn {
            remote: "10.0.0.2:5000".parse().unwrap(),
            local,
            tls,
        };
        let mut url = url_of(&parts, &conn);
        let text = unsafe { url.as_bytes() }.to_vec();
        unsafe { url.release() };
        String::from_utf8(text).unwrap()
    }

    #[test]
    fn urls_are_absolute() {
        let get = |uri: &str| hyper::Request::get(uri);
        assert_eq!(
            url(get("/a/b?x=1").header("host", "example.com:81"), false),
            "http://example.com:81/a/b?x=1"
        );
        assert_eq!(
            url(get("/").header("host", "example.com"), true),
            "https://example.com/"
        );
        // HTTP/2 and proxy requests carry the scheme and authority in the request target.
        assert_eq!(
            url(get("https://h2.example/p?q").header("host", "other"), false),
            "https://h2.example/p?q"
        );
        // HTTP/1.0 may name no host: the server's address stands in.
        assert_eq!(url(get("/x"), false), "http://10.0.0.1:8080/x");
    }

    #[test]
    fn http2_cookie_crumbs_are_joined_with_semicolons() {
        let mut h = HeaderMap::new();
        h.append(COOKIE, HeaderValue::from_static("a=1"));
        h.append(COOKIE, HeaderValue::from_static("b=2"));
        join_cookies(&mut h);
        assert_eq!(h.get_all(COOKIE).iter().count(), 1);
        assert_eq!(h.get(COOKIE).unwrap(), "a=1; b=2");
        let mut one = HeaderMap::new();
        one.insert(COOKIE, HeaderValue::from_static("a=1"));
        join_cookies(&mut one);
        assert_eq!(one.get(COOKIE).unwrap(), "a=1");
    }
}
