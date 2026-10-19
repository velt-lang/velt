//! Server responses (`VeltResp*`): a handler's global `Response` (std/fetch/response.vlt) is
//! built into one with `velt_rt_http_resp_build` and returned as the handler's result (ownership
//! goes back to the runtime), or freed with `velt_rt_http_resp_drop` if abandoned.
//!
//! A response is a key into a registry (`crate::registry`), not an address: a forged or stale
//! key (a response already returned or dropped) is ignored instead of touching freed memory.
//! `stream.rs` mutates a response to stream its body, so the table holds it behind a mutex
//! (never contended: one handler builds one response).
//!
//! The text body is handed to hyper without copying (the caller's value is left empty), and a
//! default `content-type` is added unless one was set. Header names and values a server sends on
//! every response are interned (`interned.rs`).

use super::body::RespBody;
use super::interned::{header_name, header_value};
use super::take_text;
use crate::bytes::VeltBytes;
use crate::registry::{Key, Registry};
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use bytes::Bytes;
use hyper::header::{HeaderMap, HeaderValue, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::{Response, StatusCode};
use std::sync::{Arc, Mutex};

/// Opaque response (`VeltResp` in the ABI docs).
pub type RespObj = Response<RespBody>;

/// A response handle (a registry key).
pub type RespHandle = Key<RespCell>;

/// A response while Velt code builds it.
pub struct RespCell(Mutex<RespObj>);

static RESPONSES: Registry<RespCell> = Registry::new();

fn empty() -> RespObj {
    Response::new(RespBody::full(Bytes::new()))
}

/// Register a response built by the runtime (the `101` of a WebSocket upgrade).
pub fn register(resp: RespObj) -> RespHandle {
    RESPONSES.insert(RespCell(Mutex::new(resp)))
}

/// Take the response back from Velt code (a handler returned it); `None` for a dead key.
pub fn take(r: RespHandle) -> Option<RespObj> {
    let cell = RESPONSES.remove(r)?;
    Some(match Arc::try_unwrap(cell) {
        Ok(cell) => cell.0.into_inner().unwrap_or_else(|e| e.into_inner()),
        // A call on another thread still holds it (only possible with a forged key): take the
        // contents and leave an empty response behind.
        Err(cell) => std::mem::replace(&mut *lock(&cell), empty()),
    })
}

fn lock(cell: &RespCell) -> std::sync::MutexGuard<'_, RespObj> {
    cell.0.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `f` on the response behind `r`; `None` (nothing done) for a dead key.
pub fn with<R>(r: RespHandle, f: impl FnOnce(&mut RespObj) -> R) -> Option<R> {
    let cell = RESPONSES.get(r)?;
    let mut resp = lock(&cell);
    Some(f(&mut resp))
}

/// Whether a response with `status` has no body (and so no body headers): 1xx, 204 and 304.
pub(crate) fn bodiless(status: StatusCode) -> bool {
    status.is_informational()
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED
}

/// How `velt_rt_http_resp_build` receives the body (std/fetch/response.vlt says the same).
pub(super) mod kind {
    /// No body.
    pub const NONE: u32 = 0;
    /// `text` (taken).
    pub const TEXT: u32 = 1;
    /// `bytes` (copied).
    pub const BYTES: u32 = 2;
    /// Streamed: std opens the stream (`velt_rt_http_resp_stream_open`) and fills it.
    pub const STREAM: u32 = 3;
    /// Streamed from a fetched response's body, which arrives decoded: the `content-encoding`
    /// the client decoded and the `content-length` of the encoded body no longer apply.
    pub const FETCHED: u32 = 4;
}

/// Appends the headers of the flat list `[name, value, …]`; `None` if a name or value is not
/// valid HTTP.
pub(super) unsafe fn append_headers(list: &VeltStrArray, map: &mut HeaderMap) -> Option<()> {
    let len = list.len as usize;
    for i in (0..len.saturating_sub(1)).step_by(2) {
        let name = header_name((*list.ptr.add(i)).as_bytes()).ok()?;
        let value = header_value((*list.ptr.add(i + 1)).text_lossy().as_bytes()).ok()?;
        map.append(name, value);
    }
    Some(())
}

/// Drops what a streamed body makes wrong: its length is not known, and a fetched body arrived
/// decoded.
fn strip_for_stream(headers: &mut HeaderMap, fetched: bool) {
    headers.remove(CONTENT_LENGTH);
    let decoded = headers
        .get(CONTENT_ENCODING)
        .is_some_and(|v| super::client::decodes(v.to_str().unwrap_or("")));
    if fetched && decoded {
        headers.remove(CONTENT_ENCODING);
    }
}

/// The `content-type` a body implies (`implied` of `velt_rt_http_resp_build`; std/fetch/body.vlt
/// numbers them the same): 0 none, 1 a string's, 2 JSON's, 3 form fields'. The values are made
/// at compile time (`HeaderValue::from_static` checks every byte when it runs, about 200
/// instructions for a text response); cloning one over static bytes is a copy.
fn implied_type(code: u32) -> Option<HeaderValue> {
    static TEXT: HeaderValue = HeaderValue::from_static("text/plain;charset=UTF-8");
    static JSON: HeaderValue = HeaderValue::from_static("application/json");
    static FORM: HeaderValue =
        HeaderValue::from_static("application/x-www-form-urlencoded;charset=UTF-8");
    match code {
        1 => Some(TEXT.clone()),
        2 => Some(JSON.clone()),
        3 => Some(FORM.clone()),
        _ => None,
    }
}

/// The body `kind` says: `text` (taken without a copy: the caller's value is left empty),
/// `bytes` (copied: the array may be borrowed), or none (also for a stream std opens next).
pub(super) unsafe fn body_of(kind: u32, text: *mut VeltStr, bytes: &VeltBytes) -> Bytes {
    match kind {
        kind::TEXT => take_text(text),
        kind::BYTES => Bytes::copy_from_slice(bytes.as_bytes()),
        _ => Bytes::new(),
    }
}

/// A `Response` returned from a handler (std/fetch/response.vlt), as one response: `status`
/// (invalid ⇒ 500), the status text (`reason`; empty for the standard one), the headers
/// `add_headers` appends, and `body` as `kind` says (`body_of`; a stream std opens next drops
/// `content-length`). `implied` is the `content-type` the body implies (`implied_type`), added
/// unless the headers have one. A 1xx, 204 or 304 status gets no body and no `content-type`.
/// `None` if a header name or value is not valid HTTP.
pub(super) fn build(
    status: u32,
    reason: &[u8],
    kind: u32,
    body: Bytes,
    implied: u32,
    add_headers: impl FnOnce(&mut HeaderMap) -> Option<()>,
) -> Option<RespObj> {
    // Made with its body (empty for a stream std opens next): most responses keep it.
    let mut r = Response::new(RespBody::full(body));
    *r.status_mut() = u16::try_from(status)
        .ok()
        .and_then(|s| StatusCode::from_u16(s).ok())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    add_headers(r.headers_mut())?;
    if !reason.is_empty() {
        if let Ok(p) = hyper::ext::ReasonPhrase::try_from(reason.to_vec()) {
            r.extensions_mut().insert(p);
        }
    }
    if bodiless(r.status()) {
        *r.body_mut() = RespBody::full(Bytes::new());
        return Some(r);
    }
    if kind == kind::STREAM || kind == kind::FETCHED {
        strip_for_stream(r.headers_mut(), kind == kind::FETCHED);
    }
    if let Some(v) = implied_type(implied) {
        let headers = r.headers_mut();
        // Without headers of its own (most responses) a plain insert does: there is nothing
        // to look for.
        if headers.is_empty() {
            headers.insert(CONTENT_TYPE, v);
        } else {
            headers.entry(CONTENT_TYPE).or_insert(v);
        }
    }
    Some(r)
}

/// [`build`] as a registered response (std opens a streamed body on it next, or a handler
/// returns it): the headers as the flat list `[name, value, …]`, the body by `kind` (0 none, 1
/// `text`, 2 `bytes`, 3 a stream, 4 a fetched response's body); the null key (nothing built)
/// if a header name or value is not valid HTTP.
///
/// # Safety
/// The pointers must be valid Velt values.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_build(
    status: u32,
    reason: *const VeltStr,
    headers: *const VeltStrArray,
    kind: u32,
    text: *mut VeltStr,
    bytes: *const VeltBytes,
    implied: u32,
) -> RespHandle {
    let body = body_of(kind, text, &*bytes);
    build(status, (*reason).as_bytes(), kind, body, implied, |m| {
        append_headers(&*headers, m)
    })
    .map_or(RespHandle::NULL, register)
}

/// Free a response that was not returned from a handler (a dead key is ignored).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_drop(r: RespHandle) {
    drop(take(r));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a response; `headers` are `[name, value, …]`.
    pub(crate) unsafe fn build(
        status: u32,
        headers: &[&str],
        kind: u32,
        text: &str,
        implied: u32,
    ) -> RespHandle {
        let list: Vec<VeltStr> = headers.iter().map(|h| VeltStr::from_text(h)).collect();
        let list = VeltStrArray::from_vec(list);
        let mut text = VeltStr::from_text(text);
        let bytes = VeltBytes::from_vec(b"\x01\x02".to_vec());
        let r = velt_rt_http_resp_build(
            status,
            &VeltStr::empty(),
            &list,
            kind,
            &mut text,
            &bytes,
            implied,
        );
        assert!(text.is_empty(), "the text body is taken");
        r
    }

    #[test]
    fn dead_and_forged_keys_are_ignored() {
        let r = unsafe { build(201, &[], kind::TEXT, "hi", 0) };
        let resp = take(r).expect("live response");
        assert_eq!(resp.status(), StatusCode::CREATED);
        // The key is dead now: taking or dropping it again is harmless.
        assert!(take(r).is_none());
        assert!(with(r, |_| ()).is_none());
        unsafe { velt_rt_http_resp_drop(r) };
        for forged in [4096u64, 1 << 40, u64::MAX] {
            let k = RespHandle::from_bits(forged);
            assert!(with(k, |_| ()).is_none());
            unsafe { velt_rt_http_resp_drop(k) };
        }
    }

    fn headers_and_len(r: RespHandle) -> (usize, usize) {
        let resp = take(r).expect("live response");
        let len = hyper::body::Body::size_hint(resp.body()).exact();
        (resp.headers().len(), len.unwrap_or(u64::MAX) as usize)
    }

    #[test]
    fn bodiless_statuses_get_no_body_headers() {
        unsafe {
            let ct = 1;
            assert_eq!(
                headers_and_len(build(200, &[], kind::TEXT, "hi", ct)),
                (1, 2)
            );
            assert_eq!(
                headers_and_len(build(204, &[], kind::TEXT, "hi", ct)),
                (0, 0)
            );
            assert_eq!(
                headers_and_len(build(304, &[], kind::TEXT, "hi", ct)),
                (0, 0)
            );
            assert_eq!(headers_and_len(build(200, &[], kind::BYTES, "", 0)), (0, 2));
        }
    }

    #[test]
    fn headers_win_over_the_implied_content_type() {
        unsafe {
            let given = ["Content-Type", "text/html", "x-a", "1", "x-a", "2"];
            let resp = take(build(200, &given, kind::TEXT, "<p>", 1)).unwrap();
            assert_eq!(resp.headers()[CONTENT_TYPE], "text/html");
            assert_eq!(resp.headers().get_all("x-a").iter().count(), 2);
            let resp = take(build(200, &[], kind::TEXT, "x", 2)).unwrap();
            assert_eq!(resp.headers()[CONTENT_TYPE], "application/json");
        }
    }

    #[test]
    fn invalid_headers_build_nothing() {
        unsafe {
            assert!(build(200, &["bad name", "x"], kind::TEXT, "x", 0).bits() == 0);
            assert!(build(200, &["x-a", "bad\nvalue"], kind::TEXT, "x", 0).bits() == 0);
        }
    }

    #[test]
    fn streamed_bodies_drop_their_length_and_decoded_coding() {
        unsafe {
            let given = ["content-length", "10", "content-encoding", "gzip"];
            let resp = take(build(200, &given, kind::STREAM, "", 0)).unwrap();
            assert!(!resp.headers().contains_key(CONTENT_LENGTH));
            assert_eq!(resp.headers()[CONTENT_ENCODING], "gzip");
            let resp = take(build(200, &given, kind::FETCHED, "", 0)).unwrap();
            assert!(resp.headers().is_empty());
        }
    }
}
