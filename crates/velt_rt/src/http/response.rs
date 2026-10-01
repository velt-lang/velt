//! Response builder (`VeltResp*`): created by the handler, returned as its result (ownership goes
//! back to the runtime), or freed with `velt_rt_http_resp_drop` if abandoned.
//!
//! Body setters take ownership of a string/bytes value (the caller's value is left empty; heap
//! buffers are handed to hyper without copying) and set a default `content-type` unless one was
//! already set. Header values a server sends on every response are interned (`interned.rs`).

use super::interned::header_value;
use super::{take_bytes, take_text};
use crate::bytes::VeltBytes;
use crate::handle::Handle;
use crate::str::VeltStr;
use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{HeaderName, HeaderValue, CONTENT_TYPE};
use hyper::{Response, StatusCode};

/// Opaque response (`VeltResp` in the ABI docs).
pub type RespObj = Response<Full<Bytes>>;

/// New response with `status` (invalid codes become 500) and an empty body.
#[no_mangle]
pub extern "C" fn velt_rt_http_resp_new(status: u32) -> Handle<RespObj> {
    let mut r = Response::new(Full::new(Bytes::new()));
    *r.status_mut() = u16::try_from(status)
        .ok()
        .and_then(|s| StatusCode::from_u16(s).ok())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    Handle::from_box(Box::new(r))
}

/// Append a header. Returns 0 (and ignores it) if the name or value is not valid HTTP.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_header(
    r: Handle<RespObj>,
    name: *const VeltStr,
    value: *const VeltStr,
) -> u8 {
    let (Ok(n), Ok(v)) = (
        HeaderName::from_bytes((*name).as_bytes()),
        header_value((*value).as_bytes()),
    ) else {
        return 0;
    };
    r.obj_mut().headers_mut().append(n, v);
    1
}

/// Set a header, replacing every earlier value of it (e.g. the default `content-type` a body
/// setter added). Returns 0 (and ignores it) if the name or value is not valid HTTP.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_set_header(
    r: Handle<RespObj>,
    name: *const VeltStr,
    value: *const VeltStr,
) -> u8 {
    let (Ok(n), Ok(v)) = (
        HeaderName::from_bytes((*name).as_bytes()),
        header_value((*value).as_bytes()),
    ) else {
        return 0;
    };
    r.obj_mut().headers_mut().insert(n, v);
    1
}

unsafe fn set_body(r: Handle<RespObj>, body: Bytes, default_type: &'static str) {
    let r = r.obj_mut();
    *r.body_mut() = Full::new(body);
    r.headers_mut()
        .entry(CONTENT_TYPE)
        .or_insert(HeaderValue::from_static(default_type));
}

/// Text body (takes `body`); default `content-type: text/plain; charset=utf-8`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_body_text(r: Handle<RespObj>, body: *mut VeltStr) {
    set_body(r, take_text(body), "text/plain; charset=utf-8");
}

/// Bytes body (takes `body`, a `VeltBytes`); default `content-type: application/octet-stream`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_body_bytes(r: Handle<RespObj>, body: *mut VeltBytes) {
    set_body(r, take_bytes(body), "application/octet-stream");
}

/// JSON body (takes `body`, already serialized); sets `content-type: application/json`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_json(r: Handle<RespObj>, body: *mut VeltStr) {
    r.obj_mut()
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    set_body(r, take_text(body), "application/json");
}

/// Free a response that was not returned from a handler.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_drop(r: Handle<RespObj>) {
    drop(r.into_box());
}
