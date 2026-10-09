//! The server side of std/fetch's `Request` and `Response` (rt_abi_async.md §7) without a
//! network: WebAssembly programs serve nothing (`velt:http` is not available), so no request a
//! server received and no server response ever exists. `Request` and `Response` still link, and
//! these accessors stop the program if a forged handle reaches them.

use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use crate::task::VeltFut;

fn no_request() -> ! {
    crate::panic::fatal("a Request was used after its handler finished")
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_method(_r: u64, _out: *mut VeltStr) {
    no_request()
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_url(_r: u64, _out: *mut VeltStr) {
    no_request()
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_header(
    _r: u64,
    _name: *const VeltStr,
    _out: *mut VeltStr,
) -> u8 {
    no_request()
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_headers(_r: u64, _out: *mut VeltStrArray) {
    no_request()
}

#[no_mangle]
pub extern "C" fn velt_rt_http_req_has_body(_r: u64) -> u8 {
    no_request()
}

#[no_mangle]
pub extern "C" fn velt_rt_http_req_text(_r: u64) -> *mut VeltFut {
    no_request()
}

#[no_mangle]
pub extern "C" fn velt_rt_http_req_bytes(_r: u64) -> *mut VeltFut {
    no_request()
}

#[no_mangle]
pub extern "C" fn velt_rt_http_req_chunk(_r: u64) -> *mut VeltFut {
    no_request()
}

/// Releasing a server response: there is none to release (handle 0 or forged).
#[no_mangle]
pub extern "C" fn velt_rt_http_resp_drop(_r: u64) {}

/// Building a server response: only `velt:http` does, which WebAssembly programs can't use.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_build(
    _status: u32,
    _reason: *const VeltStr,
    _headers: *const VeltStrArray,
    _kind: u32,
    _text: *mut VeltStr,
    _bytes: *const crate::bytes::VeltBytes,
    _implied: u32,
) -> u64 {
    crate::panic::fatal("WebAssembly programs cannot serve HTTP")
}

/// Handing a response to a server request: there is none (only `velt:http` serves).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_respond(
    _req: u64,
    _status: u32,
    _reason: *const VeltStr,
    _name: *const VeltStr,
    _value: *const VeltStr,
    _kind: u32,
    _text: *mut VeltStr,
    _implied: u32,
) -> u64 {
    no_request()
}

/// As `velt_rt_http_req_respond`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_req_respond_list(
    _req: u64,
    _status: u32,
    _reason: *const VeltStr,
    _name: *const VeltStr,
    _value: *const VeltStr,
    _headers: *const VeltStrArray,
    _kind: u32,
    _text: *mut VeltStr,
    _bytes: *const crate::bytes::VeltBytes,
    _implied: u32,
) -> u64 {
    no_request()
}
