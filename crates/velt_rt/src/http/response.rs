//! Response builder (`VeltResp*`): created by the handler, returned as its result (ownership goes
//! back to the runtime), or freed with `velt_rt_http_resp_drop` if abandoned.
//!
//! A response is a key into a registry (`crate::registry`), not an address: a forged or stale
//! key (a response already returned or dropped) is ignored by the builders instead of touching
//! freed memory. The builders mutate the response, so the table holds it behind a mutex (never
//! contended: one handler builds one response).
//!
//! Body setters take ownership of a string/bytes value (the caller's value is left empty; heap
//! buffers are handed to hyper without copying) and set a default `content-type` unless one was
//! already set. Header values a server sends on every response are interned (`interned.rs`).
//! `stream.rs` turns the body into a stream instead (`Response.stream`).

use super::body::RespBody;
use super::interned::header_value;
use super::{take_bytes, take_text};
use crate::bytes::VeltBytes;
use crate::registry::{Key, Registry};
use crate::str::VeltStr;
use bytes::Bytes;
use hyper::header::{HeaderName, HeaderValue, CONTENT_TYPE};
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
        // A builder call on another thread still holds it (only possible with a forged key):
        // take the contents and leave an empty response behind.
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

/// New response with `status` (invalid codes become 500) and an empty body.
#[no_mangle]
pub extern "C" fn velt_rt_http_resp_new(status: u32) -> RespHandle {
    let mut r = empty();
    *r.status_mut() = u16::try_from(status)
        .ok()
        .and_then(|s| StatusCode::from_u16(s).ok())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    register(r)
}

/// Append a header. Returns 0 (and ignores it) if the name or value is not valid HTTP, or the
/// response is gone.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_header(
    r: RespHandle,
    name: *const VeltStr,
    value: *const VeltStr,
) -> u8 {
    let (Ok(n), Ok(v)) = (
        HeaderName::from_bytes((*name).as_bytes()),
        header_value((*value).as_bytes()),
    ) else {
        return 0;
    };
    with(r, |r| r.headers_mut().append(n, v)).is_some() as u8
}

/// Set a header, replacing every earlier value of it (e.g. the default `content-type` a body
/// setter added). Returns 0 (and ignores it) if the name or value is not valid HTTP, or the
/// response is gone.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_set_header(
    r: RespHandle,
    name: *const VeltStr,
    value: *const VeltStr,
) -> u8 {
    let (Ok(n), Ok(v)) = (
        HeaderName::from_bytes((*name).as_bytes()),
        header_value((*value).as_bytes()),
    ) else {
        return 0;
    };
    with(r, |r| r.headers_mut().insert(n, v)).is_some() as u8
}

/// Whether a response with `status` has no body (and so no body headers): 1xx, 204 and 304.
pub(crate) fn bodiless(status: StatusCode) -> bool {
    status.is_informational()
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED
}

/// Sets the body and its default `content-type`; neither for a bodiless status (the body is
/// dropped, as HTTP forbids one).
fn set_body(r: RespHandle, body: Bytes, default_type: &'static str) {
    with(r, |r| {
        if bodiless(r.status()) {
            return;
        }
        *r.body_mut() = RespBody::full(body);
        r.headers_mut()
            .entry(CONTENT_TYPE)
            .or_insert(HeaderValue::from_static(default_type));
    });
}

/// Text body (takes `body`; dropped for a bodiless status: 1xx, 204, 304); default
/// `content-type: text/plain; charset=utf-8`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_body_text(r: RespHandle, body: *mut VeltStr) {
    set_body(r, take_text(body), "text/plain; charset=utf-8");
}

/// Bytes body (takes `body`, a `VeltBytes`); default `content-type: application/octet-stream`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_body_bytes(r: RespHandle, body: *mut VeltBytes) {
    set_body(r, take_bytes(body), "application/octet-stream");
}

/// JSON body (takes `body`, already serialized); sets `content-type: application/json`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_json(r: RespHandle, body: *mut VeltStr) {
    let body = take_text(body);
    with(r, |r| {
        if bodiless(r.status()) {
            return;
        }
        r.headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        *r.body_mut() = RespBody::full(body);
    });
}

/// Free a response that was not returned from a handler (a dead key is ignored).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_drop(r: RespHandle) {
    drop(take(r));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_and_forged_keys_are_ignored() {
        let r = velt_rt_http_resp_new(201);
        let mut body = VeltStr::from_static(b"hi");
        unsafe { velt_rt_http_resp_body_text(r, &mut body) };
        let resp = take(r).expect("live response");
        assert_eq!(resp.status(), StatusCode::CREATED);
        // The key is dead now: every builder ignores it, and dropping it again is harmless.
        let (name, value) = (VeltStr::from_static(b"x-a"), VeltStr::from_static(b"1"));
        assert_eq!(unsafe { velt_rt_http_resp_header(r, &name, &value) }, 0);
        assert!(take(r).is_none());
        unsafe { velt_rt_http_resp_drop(r) };
        for forged in [4096u64, 1 << 40, u64::MAX] {
            let k = RespHandle::from_bits(forged);
            assert_eq!(unsafe { velt_rt_http_resp_set_header(k, &name, &value) }, 0);
            unsafe { velt_rt_http_resp_drop(k) };
        }
    }

    unsafe fn headers_and_len(status: u32, set: impl Fn(RespHandle)) -> (usize, usize) {
        let r = velt_rt_http_resp_new(status);
        set(r);
        let resp = take(r).expect("live response");
        let len = hyper::body::Body::size_hint(resp.body()).exact();
        (resp.headers().len(), len.unwrap_or(u64::MAX) as usize)
    }

    #[test]
    fn bodiless_statuses_get_no_body_headers() {
        unsafe {
            let text = |r| velt_rt_http_resp_body_text(r, &mut VeltStr::from_vec(b"hi".to_vec()));
            let json = |r| velt_rt_http_resp_json(r, &mut VeltStr::from_vec(b"{}".to_vec()));
            assert_eq!(headers_and_len(200, text), (1, 2));
            assert_eq!(headers_and_len(204, text), (0, 0));
            assert_eq!(headers_and_len(304, text), (0, 0));
            assert_eq!(headers_and_len(200, json), (1, 2));
            assert_eq!(headers_and_len(204, json), (0, 0));
        }
    }
}
