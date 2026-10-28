//! The TCP operations the global `fetch` reaches (its client is Velt code on `std/net`,
//! std/fetch/client/), without a network: WebAssembly programs have no sockets, so connecting
//! fails with `ENOTSUP` and no stream ever exists. Programs that only build responses
//! (`new Response(...)`, `Response.json(...)`, as in tests) link and run; an operation on a
//! stream finds none (`EBADF`, as for a closed handle).

use crate::bytes::VeltBytes;
use crate::result::{code, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::leaf::ready_leaf;
use crate::task::VeltFut;

fn no_network() -> VeltErr {
    VeltErr::new(
        code::UNSUPPORTED,
        "fetch failed: WebAssembly programs have no network access",
    )
}

fn no_stream() -> VeltErr {
    VeltErr::new(code::BAD_HANDLE, "handle is closed")
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_connect(_addr: *const VeltStr) -> *mut VeltFut {
    ready_leaf(|| IoResult::<u64>::err(no_network()))
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_connect_tls(
    _addr: *const VeltStr,
    _server_name: *const VeltStr,
    _ca: *const VeltStr,
    _alpn: *const VeltStr,
) -> *mut VeltFut {
    ready_leaf(|| IoResult::<u64>::err(no_network()))
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_try_read_into(
    _s: u64,
    _data: *const VeltBytes,
    _start: u64,
    _end: u64,
    out: *mut IoResult<i64>,
) {
    out.write(IoResult::err(no_stream()));
}

#[no_mangle]
pub extern "C" fn velt_rt_tcp_readable(_s: u64) -> *mut VeltFut {
    ready_leaf(|| IoResult::<()>::err(no_stream()))
}

#[no_mangle]
pub extern "C" fn velt_rt_tcp_readable_within(_s: u64, _ms: i64, _signal: u64) -> *mut VeltFut {
    ready_leaf(|| IoResult::<u8>::err(no_stream()))
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_write_range(
    _s: u64,
    _data: *const VeltBytes,
    _start: u64,
    _end: u64,
) -> *mut VeltFut {
    ready_leaf(|| IoResult::<()>::err(no_stream()))
}

/// Writes nothing (the next `write` reports the error).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_try_write_range(
    _s: u64,
    _data: *const VeltBytes,
    _start: u64,
    _end: u64,
) -> u64 {
    0
}

#[no_mangle]
pub extern "C" fn velt_rt_tcp_abort(_s: u64) {}

#[no_mangle]
pub extern "C" fn velt_rt_tcp_close(_s: u64) {}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_park(_s: u64, _key: *const VeltStr) {}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_unpark(_key: *const VeltStr) -> u64 {
    0
}
