//! Incoming HTTP request object (`VeltReq*`) and its accessors.
//!
//! The body is read completely before the handler runs, so every accessor is synchronous. The
//! handler owns its `VeltReq*` and frees it with `velt_rt_http_req_drop`. Accessors return owned
//! copies, so their results never dangle after the request is dropped.

use super::owned_str;
use crate::bytes::VeltBytes;
use crate::handle::Handle;
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::Request;

/// A request handle (`Box<ReqObj>`), owned by the handler once it starts.
pub type ReqHandle = Handle<ReqObj>;

/// Opaque request (`VeltReq` in the ABI docs).
pub struct ReqObj {
    parts: hyper::http::request::Parts,
    body: Bytes,
    /// Key of the parked HTTP upgrade (`upgrade.rs`); 0 = not an upgrade request.
    upgrade: u64,
}

impl ReqObj {
    /// Read a hyper request including its whole body; `None` if the body could not be read.
    /// `upgrade` is the key of its parked upgrade, 0 if none.
    pub async fn read(req: Request<Incoming>, upgrade: u64) -> Option<ReqObj> {
        let (parts, body) = req.into_parts();
        let body = body.collect().await.ok()?.to_bytes();
        Some(ReqObj {
            parts,
            body,
            upgrade,
        })
    }
}

/// The key std/websocket passes to `velt_rt_ws_accept`; 0 if the request asked for no upgrade.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_upgrade(req: ReqHandle) -> u64 {
    req.obj().upgrade
}

/// `req.method` (`GET`, `POST`...).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_method(req: ReqHandle, out: *mut VeltStr) {
    out.write(owned_str(req.obj().parts.method.as_str()));
}

/// `req.path`: path without the query string (`/users/1`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_path(req: ReqHandle, out: *mut VeltStr) {
    out.write(owned_str(req.obj().parts.uri.path()));
}

/// `req.query`: raw query string without `?` (empty if none).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_query(req: ReqHandle, out: *mut VeltStr) {
    out.write(owned_str(req.obj().parts.uri.query().unwrap_or("")));
}

/// `req.headers.get(name)` (case-insensitive): returns 1 and writes `out`, or 0 if absent.
/// Non-UTF-8 header bytes are decoded lossily.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_header(
    req: ReqHandle,
    name: *const VeltStr,
    out: *mut VeltStr,
) -> u8 {
    let name = String::from_utf8_lossy((*name).as_bytes());
    match req.obj().parts.headers.get(name.as_ref()) {
        Some(v) => {
            out.write(owned_str(&String::from_utf8_lossy(v.as_bytes())));
            1
        }
        None => 0,
    }
}

/// Number of header fields (for iterating `req.headers`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_header_count(req: ReqHandle) -> u64 {
    req.obj().parts.headers.len() as u64
}

/// Header field `i` (`0 <= i < count`, in received order per name): lowercase name and value.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_header_at(
    req: ReqHandle,
    i: u64,
    name: *mut VeltStr,
    value: *mut VeltStr,
) {
    let Some((n, v)) = req.obj().parts.headers.iter().nth(i as usize) else {
        crate::panic::fatal("request header index out of range")
    };
    name.write(owned_str(n.as_str()));
    value.write(owned_str(&String::from_utf8_lossy(v.as_bytes())));
}

/// Every header name (lowercase), in received order per name: `req.headers` in one call (std
/// pairs it with [`velt_rt_http_req_header_values`]; `header_at` per index is O(n) each).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_header_names(req: ReqHandle, out: *mut VeltStrArray) {
    let headers = req.obj().parts.headers.iter();
    out.write(VeltStrArray::from_vec(
        headers.map(|(n, _)| owned_str(n.as_str())).collect(),
    ));
}

/// Every header value, in the order of [`velt_rt_http_req_header_names`] (non-UTF-8 bytes
/// decoded lossily).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_header_values(req: ReqHandle, out: *mut VeltStrArray) {
    let headers = req.obj().parts.headers.iter();
    out.write(VeltStrArray::from_vec(
        headers
            .map(|(_, v)| owned_str(&String::from_utf8_lossy(v.as_bytes())))
            .collect(),
    ));
}

/// `req.body` as text (invalid UTF-8 decoded lossily to U+FFFD).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_body(req: ReqHandle, out: *mut VeltStr) {
    out.write(owned_str(&String::from_utf8_lossy(&req.obj().body)));
}

/// `req.body` as bytes.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_body_bytes(req: ReqHandle, out: *mut VeltBytes) {
    out.write(VeltBytes::from_vec(req.obj().body.to_vec()));
}

/// Free a request.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_drop(req: ReqHandle) {
    drop(req.into_box());
}
