//! A handler's response with a complete body, handed straight to its request
//! (`velt_rt_http_req_respond`): built as `velt_rt_http_resp_build` builds one, then left in
//! the frame of the handler being polled (`context.rs`) instead of a registry, so the runtime
//! sends it when the handler returns. A response's first header comes as its own pair, as
//! std's `Headers` keeps a first header (as written; a new response usually has one at most),
//! so a response with one header needs no list.

use super::context;
use super::interned::{header_name, header_value};
use super::response::{append_headers, body_of, build, kind, register, RespObj};
use super::take_text;
use crate::bytes::VeltBytes;
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use bytes::Bytes;
use hyper::header::HeaderMap;

/// The first header of `velt_rt_http_req_respond`, as std passes it (`name` "" for none): the
/// name as written (parsing lowercases it) and the value with surrounding HTTP whitespace
/// removed. `None` if the name or value is not valid HTTP.
unsafe fn append_first(name: &VeltStr, value: &VeltStr, map: &mut HeaderMap) -> Option<()> {
    let name = name.as_bytes();
    if name.is_empty() {
        return Some(());
    }
    let value = value.text_lossy();
    let http_space = |b: &u8| matches!(b, b'\t' | b'\n' | b'\r' | b' ');
    let value = value.as_bytes();
    let start = value
        .iter()
        .position(|b| !http_space(b))
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|b| !http_space(b))
        .map_or(start, |e| e + 1);
    map.append(
        header_name(name).ok()?,
        header_value(&value[start..end]).ok()?,
    );
    Some(())
}

/// The response a handler returns for request `req`, with no body or a text one (`kind` 0 or 1;
/// `text` is taken, as by `velt_rt_http_resp_build`) and at most one header of its own (as
/// given; `name` "" for none; `append_first`), handed straight to the request: what the handler
/// returns next.
/// That is `context::RESPONDED` when the response was left in the handler's frame (the handler
/// is being polled for `req`, as it is when it returns), else a registered response's key (a
/// handler that runs on in a task of its own after its client left), or 0 if a header name or
/// value is not valid HTTP.
///
/// # Safety
/// The pointers must be valid Velt values.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_respond(
    req: u64,
    status: u32,
    reason: *const VeltStr,
    name: *const VeltStr,
    value: *const VeltStr,
    kind: u32,
    text: *mut VeltStr,
    implied: u32,
) -> u64 {
    let (kind, body) = match kind {
        kind::TEXT => (kind::TEXT, take_text(text)),
        _ => (kind::NONE, Bytes::new()),
    };
    let add = |m: &mut HeaderMap| append_first(&*name, &*value, m);
    hand_over(
        req,
        build(status, (*reason).as_bytes(), kind, body, implied, add),
    )
}

/// `velt_rt_http_req_respond` with more headers: the first one as given, then the flat list
/// `[name, value, …]` (lowercase names, trimmed values).
///
/// # Safety
/// The pointers must be valid Velt values.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_respond_list(
    req: u64,
    status: u32,
    reason: *const VeltStr,
    name: *const VeltStr,
    value: *const VeltStr,
    headers: *const VeltStrArray,
    kind: u32,
    text: *mut VeltStr,
    bytes: *const VeltBytes,
    implied: u32,
) -> u64 {
    let body = body_of(kind, text, &*bytes);
    let add = |m: &mut HeaderMap| {
        append_first(&*name, &*value, m)?;
        append_headers(&*headers, m)
    };
    hand_over(
        req,
        build(status, (*reason).as_bytes(), kind, body, implied, add),
    )
}

/// What a handler that built `resp` for request `req` returns (`velt_rt_http_req_respond`).
fn hand_over(req: u64, resp: Option<RespObj>) -> u64 {
    let Some(r) = resp else {
        return 0;
    };
    match context::respond(req, r) {
        None => context::RESPONDED,
        Some(r) => register(r).bits(),
    }
}
