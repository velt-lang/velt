//! The Workers host (docs/internals/design/cloudflare-workers.md §2–3): `serve()` stores its
//! handler instead of listening, and the JS glue (`velt_worker.mjs`) hands each request in
//! through the exported `velt_fetch`. Reading the request and building the response are std
//! code over host imports (std/http/server_io.workers.vlt); this module only starts the
//! handler's task for each request, as the native server does, and tells the glue when it
//! finished. It also exports the helpers the glue reads and writes Velt strings and bytes with.
//!
//! The handler stays stored for the life of the instance: a Worker never closes its server.

use std::alloc::Layout;
use std::cell::Cell;
use std::ffi::c_void;

use crate::bytes::VeltBytes;
use crate::result::{code, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::leaf::ready_leaf;
use crate::task::{executor, DropFn, PollFn, VeltFut, READY};

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
mod imports {
    #[link(wasm_import_module = "velt")]
    extern "C" {
        /// The handler for request `req` finished with `result`: 1 when it responded through
        /// the host (std/http/server_io.workers.vlt), a streamed response's handle (above 1),
        /// or 0 when it failed (the glue answers 500).
        pub fn fetch_done(req: u32, result: u32);
    }
}

/// `fetch_done` for the native unit tests: records the last call.
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
mod imports {
    use std::cell::Cell;

    thread_local! {
        pub static LAST: Cell<Option<(u32, u32)>> = const { Cell::new(None) };
    }

    pub unsafe fn fetch_done(req: u32, result: u32) {
        LAST.with(|l| l.set(Some((req, result))));
    }
}

/// Builds a request's state: `(env, req, state)` (rt_abi_async.md §7, `VeltHandler.init`).
type InitFn = unsafe extern "C" fn(env: *mut c_void, req: *mut c_void, state: *mut u8);

/// `VeltHandler` as std/http hands it over: six 8-byte slots (function pointers widened).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawHandler {
    init: u64,
    poll: u64,
    drop: u64,
    state_size: u64,
    state_align: u64,
    env: u64,
}

thread_local! {
    static HANDLER: Cell<Option<RawHandler>> = const { Cell::new(None) };
}

/// A request's task: the handler's state machine, polled until it finishes.
#[repr(C)]
struct ReqFut {
    header: VeltFut,
    /// Offset 16: the result slot (the task has no result).
    _slot: u64,
    req: u32,
    state: *mut u8,
    layout: Layout,
    poll: PollFn,
    drop: DropFn,
    done: bool,
}

unsafe extern "C" fn req_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let r = &mut *(f as *mut ReqFut);
    if (r.poll)(r.state, cx) != READY {
        return 0;
    }
    r.done = true;
    // The handler's result: 1 when it responded through the host, a streamed response's
    // handle, or 0 when it failed.
    let result = (r.state as *const u64).read();
    imports::fetch_done(r.req, result as u32);
    READY
}

unsafe extern "C" fn req_drop(f: *mut VeltFut) {
    let r = Box::from_raw(f as *mut ReqFut);
    if !r.done {
        (r.drop)(r.state);
    }
    std::alloc::dealloc(r.state, r.layout);
}

/// Handle request `req` (a handle the glue keeps): start the handler's task and run a turn.
/// 0 if no handler was registered (`main` did not call `serve`), 1 otherwise; the glue learns
/// the outcome from `fetch_done`, during this call or a later turn.
#[no_mangle]
pub unsafe extern "C" fn velt_fetch(req: u32) -> u32 {
    let Some(h) = HANDLER.with(Cell::get) else {
        return 0;
    };
    let Ok(layout) =
        Layout::from_size_align((h.state_size as usize).max(1), h.state_align as usize)
    else {
        crate::panic::fatal("ICE: invalid request handler state layout");
    };
    let state = std::alloc::alloc(layout);
    if state.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    let init: InitFn = std::mem::transmute::<usize, InitFn>(h.init as usize);
    init(
        h.env as usize as *mut c_void,
        req as usize as *mut c_void,
        state,
    );
    let fut = Box::new(ReqFut {
        header: VeltFut::new(req_poll, req_drop),
        _slot: 0,
        req,
        state,
        layout,
        poll: std::mem::transmute::<usize, PollFn>(h.poll as usize),
        drop: std::mem::transmute::<usize, DropFn>(h.drop as usize),
        done: false,
    });
    executor::spawn(Box::into_raw(fut) as *mut VeltFut, 0, None);
    turn();
    1
}

/// Run a turn: the glue calls it when something the program waits for may have happened.
#[no_mangle]
pub unsafe extern "C" fn velt_turn() {
    turn();
}

/// A turn, then the output it wrote (stdout is buffered, and the host's log is per line), and a
/// wake-up for the next timer.
pub(crate) unsafe fn turn() {
    executor::run_turn();
    crate::io::flush_stdout();
    crate::host_promise::request_timer();
}

/// `serve(…)`: store the handler (the address is meaningless here). A second server is
/// `EADDRINUSE`, as a second bind of one port would be.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_serve(
    _addr: *const VeltStr,
    h: *const RawHandler,
) -> *mut VeltFut {
    let h = *h;
    let taken = HANDLER.with(|c| c.get().is_some());
    if !taken {
        HANDLER.with(|c| c.set(Some(h)));
    }
    ready_leaf(move || match taken {
        true => IoResult::<u64>::err(VeltErr::new(
            code::ADDR_IN_USE,
            "a Worker serves one handler: serve() was called already",
        )),
        false => IoResult::ok(1),
    })
}

/// `serve({ tls })`: TLS is the host's job on Workers, so this is [`velt_rt_http_serve`].
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_serve_tls(
    addr: *const VeltStr,
    h: *const RawHandler,
    _cert: *const VeltStr,
    _key: *const VeltStr,
) -> *mut VeltFut {
    velt_rt_http_serve(addr, h)
}

/// `Server.port`: there is no port.
#[no_mangle]
pub extern "C" fn velt_rt_http_server_port(_s: u64) -> u32 {
    0
}

/// `Server.close()`: a Worker keeps serving for the life of its instance.
#[no_mangle]
pub extern "C" fn velt_rt_http_server_close(_s: u64) {}

/// Dropping a `Server`: nothing to release.
#[no_mangle]
pub extern "C" fn velt_rt_http_server_detach(_s: u64) {}

/// `Server.shutdown()`: ready at once (see `close`).
#[no_mangle]
pub extern "C" fn velt_rt_http_server_shutdown(_s: u64) -> *mut VeltFut {
    ready_leaf(|| 0u8)
}

/// The bytes of string `s` (valid until the program runs again).
#[no_mangle]
pub unsafe extern "C" fn velt_host_str_ptr(s: *const VeltStr) -> *const u8 {
    (*s).as_bytes().as_ptr()
}

/// The length in bytes of string `s`.
#[no_mangle]
pub unsafe extern "C" fn velt_host_str_len(s: *const VeltStr) -> usize {
    (*s).as_bytes().len()
}

/// The bytes of array `b`.
#[no_mangle]
pub unsafe extern "C" fn velt_host_bytes_ptr(b: *const VeltBytes) -> *const u8 {
    (*b).as_bytes().as_ptr()
}

/// The length of array `b`.
#[no_mangle]
pub unsafe extern "C" fn velt_host_bytes_len(b: *const VeltBytes) -> usize {
    (*b).as_bytes().len()
}

/// A buffer of `len` bytes for the glue to write into ([`velt_host_free`] releases it).
#[no_mangle]
pub extern "C" fn velt_host_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// Release a buffer from [`velt_host_alloc`].
#[no_mangle]
pub unsafe extern "C" fn velt_host_free(p: *mut u8, len: usize) {
    drop(Vec::from_raw_parts(p, 0, len.max(1)));
}

/// Write the string of the `len` UTF-8 bytes at `p` to `out` (a host import's string result).
#[no_mangle]
pub unsafe extern "C" fn velt_host_str_new(out: *mut VeltStr, p: *const u8, len: usize) {
    out.write(VeltStr::from_bytes(std::slice::from_raw_parts(p, len)));
}

/// Write an array of the `len` bytes at `p` to `out` (a host import's `u8[]` result).
#[no_mangle]
pub unsafe extern "C" fn velt_host_bytes_new(out: *mut VeltBytes, p: *const u8, len: usize) {
    out.write(VeltBytes::from_vec(
        std::slice::from_raw_parts(p, len).to_vec(),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_cross_both_ways() {
        let text = "héllo";
        let buf = velt_host_alloc(text.len());
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), buf, text.len());
            let mut out = std::mem::MaybeUninit::<VeltStr>::uninit();
            velt_host_str_new(out.as_mut_ptr(), buf, text.len());
            velt_host_free(buf, text.len());
            let mut s = out.assume_init();
            let bytes = std::slice::from_raw_parts(velt_host_str_ptr(&s), velt_host_str_len(&s));
            assert_eq!(bytes, text.as_bytes());
            s.release();
        }
    }

    unsafe extern "C" fn init(_env: *mut c_void, req: *mut c_void, state: *mut u8) {
        (state as *mut u64).write(req as u64);
    }

    // Responds at once: the result is 1 ("responded") for even requests, 0 for odd ones.
    unsafe extern "C" fn poll(state: *mut u8, _cx: *mut c_void) -> u32 {
        let req = (state as *const u64).read();
        (state as *mut u64).write(u64::from(req.is_multiple_of(2)));
        READY
    }

    unsafe extern "C" fn drop_state(_state: *mut u8) {}

    #[test]
    fn requests_run_the_stored_handler() {
        unsafe {
            assert_eq!(velt_fetch(2), 0, "no handler yet");
            let h = RawHandler {
                init: init as *const () as usize as u64,
                poll: poll as *const () as usize as u64,
                drop: drop_state as *const () as usize as u64,
                state_size: 8,
                state_align: 8,
                env: 0,
            };
            let leaf = velt_rt_http_serve(std::ptr::null(), &h);
            ((*leaf).drop.0)(leaf);
            assert_eq!(velt_fetch(4), 1);
            assert_eq!(imports::LAST.with(Cell::get), Some((4, 1)));
            assert_eq!(velt_fetch(5), 1);
            assert_eq!(imports::LAST.with(Cell::get), Some((5, 0)));
        }
    }
}
