//! The client ABI of the global `fetch` (rt_abi_async.md §7) without a network: WebAssembly
//! programs have no sockets, so `velt_rt_http_fetch_send` fails with `ENOTSUP` and no fetched
//! response ever exists. Programs that only build responses (`new Response(...)`,
//! `Response.json(...)`, as in tests) link and run; the accessors of a fetched response can't be
//! reached, and stop the program if a forged handle reaches them.

use crate::bytes::VeltBytes;
use crate::result::{code, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use crate::task::leaf::ready_leaf;
use crate::task::VeltFut;

fn no_network() -> VeltErr {
    VeltErr::new(
        code::UNSUPPORTED,
        "fetch failed: WebAssembly programs have no network access",
    )
}

fn no_response() -> ! {
    crate::panic::fatal("a fetch response was used after it was released")
}

/// `fetch(...)`: always fails with `ENOTSUP` (the arguments stay with the caller).
#[no_mangle]
#[allow(clippy::too_many_arguments)] // the native signature, which must match exactly
pub unsafe extern "C" fn velt_rt_http_fetch_send(
    _method: *const VeltStr,
    _url: *const VeltStr,
    _headers: *const VeltStrArray,
    _kind: u32,
    _text: *mut VeltStr,
    _bytes: *mut VeltBytes,
    _redirect: u32,
    _signal: u64,
    _ca: *const VeltStr,
) -> *mut VeltFut {
    ready_leaf(|| IoResult::<u64>::err(no_network()))
}

#[no_mangle]
pub extern "C" fn velt_rt_http_fetch_resp_status(_r: u64) -> u32 {
    no_response()
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_fetch_resp_status_text(_r: u64, _out: *mut VeltStr) {
    no_response()
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_fetch_resp_url(_r: u64, _out: *mut VeltStr) {
    no_response()
}

#[no_mangle]
pub extern "C" fn velt_rt_http_fetch_resp_redirected(_r: u64) -> bool {
    no_response()
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_fetch_resp_headers(_r: u64, _out: *mut VeltStrArray) {
    no_response()
}

#[no_mangle]
pub extern "C" fn velt_rt_http_fetch_resp_text(_r: u64) -> *mut VeltFut {
    no_response()
}

#[no_mangle]
pub extern "C" fn velt_rt_http_fetch_resp_bytes(_r: u64) -> *mut VeltFut {
    no_response()
}

#[no_mangle]
pub extern "C" fn velt_rt_http_fetch_resp_chunk(_r: u64) -> *mut VeltFut {
    no_response()
}

/// Releasing a response: there is none to release (handle 0 or forged).
#[no_mangle]
pub extern "C" fn velt_rt_http_fetch_resp_drop(_r: u64) {}
