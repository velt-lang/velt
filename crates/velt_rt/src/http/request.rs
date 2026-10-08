//! Incoming HTTP request object (`VeltReq*`) and its accessors: what std/fetch's global `Request`
//! reads, lazily, for a request a server received.
//!
//! The head (method, URL, headers) is there when the handler starts, so its accessors are
//! synchronous; the body is received while the handler reads it (`req_body.rs`), so its readers
//! are async. A request is a key into a registry (`crate::registry`): the handler releases it
//! with `velt_rt_http_req_drop`, and any later use (a `Request` captured by a streamed body or
//! a spawned task that outlives its handler) is a clear runtime error instead of a read of freed
//! memory. Accessors return owned copies, so their results never dangle. While the handler is
//! polled, accessors find its request in the worker's frame (`context.rs`) without the registry.

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
use hyper::Request;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

/// A request handle (a registry key), owned by the handler once it starts.
pub type ReqHandle = Key<ReqObj>;

static REQUESTS: Registry<ReqObj> = Registry::new();

/// Register a request read by the server; the handler receives the returned key, and the
/// server keeps `req` to put in the handler's frame (`context.rs`).
pub fn register(req: Arc<ReqObj>) -> ReqHandle {
    REQUESTS.insert_shared(req)
}

/// A request an accessor reads: the one in this thread's frame, or a registry reference.
enum ReqRef {
    /// Kept alive by the handler's future while it is polled (`context.rs`).
    Current(*const ReqObj),
    Held(Arc<ReqObj>),
}

impl std::ops::Deref for ReqRef {
    type Target = ReqObj;

    fn deref(&self) -> &ReqObj {
        match self {
            // SAFETY: valid for the poll this accessor runs in (`context::request`).
            ReqRef::Current(r) => unsafe { &**r },
            ReqRef::Held(r) => r,
        }
    }
}

/// The request behind `req` for the duration of an accessor call, or a fatal error when it was
/// already released.
fn obj(req: ReqHandle) -> ReqRef {
    match super::context::request(req.bits()) {
        Some(r) => ReqRef::Current(r),
        None => ReqRef::Held(held(req)),
    }
}

/// A reference to the request behind `req` that a body read can keep.
fn shared(req: ReqHandle) -> Arc<ReqObj> {
    match super::context::request(req.bits()) {
        // SAFETY: the frame's request is an `Arc` the handler's future holds.
        Some(r) => unsafe {
            Arc::increment_strong_count(r);
            Arc::from_raw(r)
        },
        None => held(req),
    }
}

fn held(req: ReqHandle) -> Arc<ReqObj> {
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

/// The key std/websocket passes to `velt_rt_ws_accept`; 0 if the request asked for no upgrade.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_upgrade(req: ReqHandle) -> u64 {
    obj(req).upgrade
}

/// `req.method` (`GET`, `POST`...): the standard methods are static strings.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_method(req: ReqHandle, out: *mut VeltStr) {
    let r = obj(req);
    let m = &r.parts.method;
    let known: &'static [u8] = match m.as_str() {
        "GET" => b"GET",
        "POST" => b"POST",
        "PUT" => b"PUT",
        "DELETE" => b"DELETE",
        "HEAD" => b"HEAD",
        "OPTIONS" => b"OPTIONS",
        "PATCH" => b"PATCH",
        _ => return out.write(owned_str(m.as_str())),
    };
    out.write(VeltStr::from_static(known));
}

/// `req.url`: the absolute URL (`http://host/path?query`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_url(req: ReqHandle, out: *mut VeltStr) {
    let r = obj(req);
    out.write(super::request_url::url_of(&r.parts, &r.conn));
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
    let r = shared(req);
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
    let r = shared(req);
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
    let r = shared(req);
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
    super::context::forget(req.bits());
    REQUESTS.remove(req);
}

#[cfg(test)]
mod tests {
    use super::join_cookies;
    use hyper::header::{HeaderMap, HeaderValue, COOKIE};

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
