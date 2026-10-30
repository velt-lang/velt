//! The runtime side of native packages (docs/internals/contracts/native_abi.md): the versioned
//! function table a package's native library receives in its `velt_native_init_<pkg>`, and the
//! futures it creates through that table.
//!
//! Native libraries never link `velt_rt_*` symbols: they carry their own copy of Rust std (and
//! cannot share this runtime's tokio), are `dlopen`ed into `velt dev`'s host whose runtime symbols
//! are not dynamically exported, or are DLLs that cannot import from an executable. Every entry of
//! [`VeltRtApi`] is therefore a pointer handed over at start-up. The table is append-only:
//! [`NATIVE_ABI_VERSION`] grows by one whenever fields are added at the end.
//!
//! Hot-reload rule (rt_abi_async.md §13.5): the code pointers stored here (`work`, `drop_ctx`,
//! `drop_result`) belong to native libraries, which are loaded once and never unloaded; no Velt
//! code pointer is ever stored.

use std::ffi::c_void;

use crate::bytes::VeltBytes;
use crate::str::VeltStr;
use crate::task::VeltFut;

mod aio;
#[cfg(test)]
mod aio_tests;
mod futures;
#[cfg(test)]
mod redis_tests;
#[cfg(test)]
use crate::task::{FUT_RESULT_OFFSET, PENDING, READY};
pub use aio::{DropStateFn, ForeignVTable, NativePollFn, WakerArg};
pub use futures::{complete, fut_blocking, fut_completer};
#[cfg(test)]
use futures::{native_drop, native_poll, take_completer};
#[cfg(test)]
use std::task::Waker;

/// The table version this runtime provides.
pub const NATIVE_ABI_VERSION: u32 = 3;

/// The work of a [`fut_blocking`] future: runs on the blocking pool with its context and the result
/// slot to fill.
pub type WorkFn = unsafe extern "C" fn(ctx: *mut c_void, out: *mut u8);
/// Frees a context whose work never ran.
pub type DropCtxFn = unsafe extern "C" fn(ctx: *mut c_void);
/// Drops a finished result nobody claimed (the future was dropped first).
pub type DropResultFn = unsafe extern "C" fn(result: *mut u8);

/// `VeltRtApi`, ABI version 3 (native_abi.md "The function table").
#[repr(C)]
pub struct VeltRtApi {
    /// [`NATIVE_ABI_VERSION`].
    pub abi_version: u32,
    /// `size_of::<VeltRtApi>()`: a library compiled against a newer table checks both.
    pub size: u32,
    /// `(ptr, len, out)`: an owned string copied from `len` UTF-8 bytes.
    pub str_new: unsafe extern "C" fn(*const u8, usize, *mut VeltStr),
    /// `(s, len_out) -> ptr`: the bytes of a string (valid while `s` is).
    pub str_bytes: unsafe extern "C" fn(*const VeltStr, *mut usize) -> *const u8,
    /// `velt_rt_str_drop`.
    pub str_drop: unsafe extern "C" fn(*mut VeltStr),
    /// `(ptr, len, out)`: an owned `u8[]` copied from `len` bytes.
    pub bytes_new: unsafe extern "C" fn(*const u8, usize, *mut VeltBytes),
    /// `velt_rt_bytes_drop`.
    pub bytes_drop: unsafe extern "C" fn(*mut VeltBytes),
    /// `(b, len_out) -> ptr`: the bytes of a `u8[]` (valid while `b` is). The array layout
    /// stays private to the runtime.
    pub bytes_data: unsafe extern "C" fn(*const VeltBytes, *mut usize) -> *const u8,
    /// `(work, ctx, drop_ctx, result_size, drop_result) -> VeltFut*`: see [`fut_blocking`].
    pub fut_blocking: unsafe extern "C" fn(
        WorkFn,
        *mut c_void,
        DropCtxFn,
        usize,
        Option<DropResultFn>,
    ) -> *mut VeltFut,
    /// `(result_size, drop_result, handle_out) -> VeltFut*`: see [`fut_completer`].
    pub fut_completer: unsafe extern "C" fn(usize, Option<DropResultFn>, *mut u64) -> *mut VeltFut,
    /// `(handle, result)`: see [`complete`].
    pub complete: unsafe extern "C" fn(u64, *const u8),
    /// `(msg, len)`: report like a Velt panic (`panic: <msg>`, exit 101).
    pub fatal: unsafe extern "C" fn(*const u8, usize) -> !,
    // Async I/O (native_abi.md "Async I/O", `aio`).
    /// `(poll, state, drop_state, result_size) -> VeltFut*`: a future the library polls inline.
    pub fut_poll:
        unsafe extern "C" fn(NativePollFn, *mut c_void, DropStateFn, usize) -> *mut VeltFut,
    /// `(cx) -> waker`: the task's runtime waker, borrowed for the poll.
    pub cx_waker: unsafe extern "C" fn(*mut c_void) -> *const c_void,
    /// `(waker) -> owned waker`: a boxed copy of a runtime waker.
    pub waker_clone: unsafe extern "C" fn(*const c_void) -> *mut c_void,
    /// `(waker)`: wake and free.
    pub waker_wake: unsafe extern "C" fn(*mut c_void),
    /// `(waker)`: wake, keep.
    pub waker_wake_by_ref: unsafe extern "C" fn(*const c_void),
    /// `(waker)`: free.
    pub waker_drop: unsafe extern "C" fn(*mut c_void),
    /// `(VeltFut*, waker) -> READY/PENDING`: poll a runtime future from library code.
    pub fut_poll_ext: unsafe extern "C" fn(*mut VeltFut, *const WakerArg) -> u32,
    /// `(VeltFut*)`: free a runtime future not handed to Velt.
    pub fut_drop: unsafe extern "C" fn(*mut VeltFut),
    /// `(VeltFut*)`: run a future (result size 0) as a detached task.
    pub spawn: unsafe extern "C" fn(*mut VeltFut),
    /// `(addr, len) -> VeltFut*` of `IoResult<u64>`: connect to `"host:port"`.
    pub tcp_connect: unsafe extern "C" fn(*const u8, usize) -> *mut VeltFut,
    /// `(socket, out) -> err`: a stream of a connected socket the library opened.
    pub stream_from_socket: unsafe extern "C" fn(i64, *mut u64) -> i32,
    /// `(h, cx, buf, cap, n, err) -> READY/PENDING`.
    pub stream_poll_read:
        unsafe extern "C" fn(u64, *const WakerArg, *mut u8, usize, *mut usize, *mut i32) -> u32,
    /// `(h, cx, buf, len, n, err) -> READY/PENDING`.
    pub stream_poll_write:
        unsafe extern "C" fn(u64, *const WakerArg, *const u8, usize, *mut usize, *mut i32) -> u32,
    /// `(h, cx, err) -> READY/PENDING`.
    pub stream_poll_flush: unsafe extern "C" fn(u64, *const WakerArg, *mut i32) -> u32,
    /// `(h, cx, err) -> READY/PENDING`: shut down the writing half.
    pub stream_poll_shutdown: unsafe extern "C" fn(u64, *const WakerArg, *mut i32) -> u32,
    /// `(h) -> socket`: the stream's socket, for socket options.
    pub stream_socket: unsafe extern "C" fn(u64) -> i64,
    /// `(h)`: close and free a stream.
    pub stream_close: unsafe extern "C" fn(u64),
    /// `() -> ns`: monotonic nanoseconds.
    pub now_ns: extern "C" fn() -> u64,
    /// `(deadline_ns) -> sleep`.
    pub sleep_new: extern "C" fn(u64) -> *mut c_void,
    /// `(sleep, cx) -> READY/PENDING`.
    pub sleep_poll: unsafe extern "C" fn(*mut c_void, *const WakerArg) -> u32,
    /// `(sleep, deadline_ns)`.
    pub sleep_reset: unsafe extern "C" fn(*mut c_void, u64),
    /// `(sleep)`: free.
    pub sleep_drop: unsafe extern "C" fn(*mut c_void),
    /// `() -> i64`: this thread's worker (-1: none).
    pub worker_index: extern "C" fn() -> i64,
    // Memory for values Velt frees (arrays a library returns).
    /// `velt_rt_alloc(size, align)`.
    pub alloc: extern "C" fn(u64, u64) -> *mut u8,
    /// `velt_rt_realloc(p, old_size, align, new_size)`.
    pub realloc: unsafe extern "C" fn(*mut u8, u64, u64, u64) -> *mut u8,
    /// `velt_rt_free(p, size, align)`.
    pub free: unsafe extern "C" fn(*mut u8, u64, u64),
    // Strings without extra passes or copies (native_abi.md "Strings").
    /// `(ptr, len, out)`: a string of UTF-8 bytes (trusted: no lossy check).
    pub str_from_utf8: unsafe extern "C" fn(*const u8, usize, *mut VeltStr),
    /// `(s, out)`: another reference to `s` (a count, no copy).
    pub str_retain: unsafe extern "C" fn(*const VeltStr, *mut VeltStr),
    /// `velt_rt_strbuf_spare(buf, min, avail) -> ptr`: room to write at the end of a builder.
    pub strbuf_spare: unsafe extern "C" fn(*mut VeltStr, u64, *mut u64) -> *mut u8,
    /// `velt_rt_strbuf_commit(buf, n, ascii)`: the bytes written become text (counted unless
    /// `ascii` is 1).
    pub strbuf_commit: unsafe extern "C" fn(*mut VeltStr, u64, u8),
    /// `(s, len_out) -> ptr`: a string's bytes when they are UTF-8 (no lone surrogate; the
    /// string knows), else null: a library borrows it as text without checking the bytes.
    pub str_utf8: unsafe extern "C" fn(*const VeltStr, *mut usize) -> *const u8,
    /// `aio::fut_poll_in`: `fut_poll` with the state in the future's block.
    pub fut_poll_in: unsafe extern "C" fn(
        aio::NativePollFn,
        aio::DropStateFn,
        usize,
        usize,
        usize,
        *mut *mut c_void,
    ) -> *mut VeltFut,
    // ABI 3: the row cursors and parameter sinks drivers hand to std (native_abi.md "Row cursors").
    /// `db_handles::db_open(kind, data, vtable, tag) -> handle`.
    pub db_open: unsafe extern "C" fn(u32, *mut c_void, *const c_void, u64) -> u64,
    /// `db_handles::db_close(kind, handle, tag) -> data` (null: not open with that tag).
    pub db_close: extern "C" fn(u32, u64, u64) -> *mut c_void,
}

/// The table every native library receives.
pub static VELT_RT_API: VeltRtApi = VeltRtApi {
    abi_version: NATIVE_ABI_VERSION,
    size: std::mem::size_of::<VeltRtApi>() as u32,
    str_new,
    str_bytes,
    str_drop: crate::str::velt_rt_str_drop,
    bytes_new,
    bytes_drop: crate::bytes::velt_rt_bytes_drop,
    bytes_data,
    fut_blocking,
    fut_completer,
    complete,
    fatal,
    fut_poll: aio::fut_poll,
    cx_waker: aio::cx_waker,
    waker_clone: aio::waker_clone,
    waker_wake: aio::waker_wake,
    waker_wake_by_ref: aio::waker_wake_by_ref,
    waker_drop: aio::waker_drop,
    fut_poll_ext: aio::fut_poll_ext,
    fut_drop: aio::fut_drop,
    spawn: aio::spawn,
    tcp_connect: aio::tcp_connect,
    stream_from_socket: aio::stream_from_socket,
    stream_poll_read: aio::stream_poll_read,
    stream_poll_write: aio::stream_poll_write,
    stream_poll_flush: aio::stream_poll_flush,
    stream_poll_shutdown: aio::stream_poll_shutdown,
    stream_socket: aio::stream_socket,
    stream_close: aio::stream_close,
    now_ns: aio::now_ns,
    sleep_new: aio::sleep_new,
    sleep_poll: aio::sleep_poll,
    sleep_reset: aio::sleep_reset,
    sleep_drop: aio::sleep_drop,
    worker_index: aio::worker_index,
    alloc: crate::mem::velt_rt_alloc,
    realloc: crate::mem::velt_rt_realloc,
    free: crate::mem::velt_rt_free,
    str_from_utf8,
    str_retain,
    strbuf_spare: crate::strbuf::velt_rt_strbuf_spare,
    strbuf_commit: crate::strbuf::velt_rt_strbuf_commit,
    str_utf8,
    fut_poll_in: aio::fut_poll_in,
    db_open: crate::db_handles::db_open,
    db_close: crate::db_handles::db_close,
};

/// `const VeltRtApi* velt_rt_native_api(void)`: the table, for generated code to pass to each
/// native package's `velt_native_init_<pkg>` before `main` runs.
#[no_mangle]
pub extern "C" fn velt_rt_native_api() -> *const VeltRtApi {
    &VELT_RT_API
}

/// Hands the table to the SDK the drivers linked into the runtime are built on (std's
/// `velt_std_*` crates): what a package's `velt_native_init_<pkg>` does, at start-up.
pub fn embed() {
    // SAFETY: the static table, whose layout the SDK's mirrors (`sdk_layouts_match_the_runtime`).
    unsafe { velt_native::__embed(&VELT_RT_API as *const VeltRtApi as *const velt_native::Api) };
}

/// `void velt_rt_native_check(int32_t rc, const VeltStr* package)`: after a native package's init
/// returned `rc`; a non-zero `rc` stops the program (exit 1) naming the package. The library has
/// usually printed why already (e.g. it needs a newer table).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_native_check(rc: i32, package: *const VeltStr) {
    if rc == 0 {
        return;
    }
    let name = (*package).text_lossy().into_owned();
    crate::io::try_flush_stdout();
    eprintln!("error: the native library of package `{name}` failed to start (code {rc})");
    std::process::exit(1)
}

/// `str_new`: a string of the library's bytes, decoded as UTF-8 from outside the program: an
/// invalid sequence, and that includes a surrogate encoded on its own (`ED A0..BF xx`), becomes
/// U+FFFD (the WHATWG rule `String::from_utf8_lossy` follows), so the result is canonical and
/// two halves of a pair never join.
unsafe extern "C" fn str_new(ptr: *const u8, len: usize, out: *mut VeltStr) {
    let bytes = if len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(ptr, len)
    };
    out.write(VeltStr::from_bytes(
        String::from_utf8_lossy(bytes).as_bytes(),
    ));
}

/// `str_utf8`: the bytes of `s` when it is well-formed (UTF-8), else null. Whether it is comes
/// from the string's own counts (a scan only for a static string with non-ASCII text).
unsafe extern "C" fn str_utf8(s: *const VeltStr, len: *mut usize) -> *const u8 {
    if !(*s).is_well_formed() {
        len.write(0);
        return std::ptr::null();
    }
    let b = (*s).as_bytes();
    len.write(b.len());
    b.as_ptr()
}

/// `str_from_utf8`: a string of a library's UTF-8 text (a Rust `&str`): counted, not checked
/// (the debug runtime checks it).
unsafe extern "C" fn str_from_utf8(ptr: *const u8, len: usize, out: *mut VeltStr) {
    let bytes = if len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(ptr, len)
    };
    debug_assert!(
        std::str::from_utf8(bytes).is_ok(),
        "ICE: str_from_utf8 of invalid UTF-8"
    );
    out.write(VeltStr::from_text(std::str::from_utf8_unchecked(bytes)));
}

/// `str_retain`: another reference to `s` (strings are immutable and counted atomically, so a
/// library may keep it on any thread).
unsafe extern "C" fn str_retain(s: *const VeltStr, out: *mut VeltStr) {
    out.write((*s).share());
}

unsafe extern "C" fn str_bytes(s: *const VeltStr, len: *mut usize) -> *const u8 {
    let b = (*s).as_bytes();
    len.write(b.len());
    b.as_ptr()
}

unsafe extern "C" fn bytes_new(ptr: *const u8, len: usize, out: *mut VeltBytes) {
    let v = if len == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(ptr, len).to_vec()
    };
    out.write(VeltBytes::from_vec(v));
}

unsafe extern "C" fn bytes_data(b: *const VeltBytes, len: *mut usize) -> *const u8 {
    let bytes = (*b).as_bytes();
    len.write(bytes.len());
    bytes.as_ptr()
}

unsafe extern "C" fn fatal(msg: *const u8, len: usize) -> ! {
    let msg = String::from_utf8_lossy(std::slice::from_raw_parts(msg, len)).into_owned();
    crate::panic::fatal(&msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};

    fn poll_once(f: *mut VeltFut) -> u32 {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        // SAFETY: a live future; `cx` outlives the call.
        unsafe { native_poll(f, crate::task::raw_cx(&mut cx).ptr()) }
    }

    async fn await_fut(f: *mut VeltFut) -> u64 {
        let f = crate::task::SendPtr(f);
        std::future::poll_fn(move |cx| {
            match unsafe { native_poll(f.0, crate::task::raw_cx(cx).ptr()) } {
                READY => Poll::Ready(()),
                _ => Poll::Pending,
            }
        })
        .await;
        // SAFETY: READY: the slot holds the u64 result.
        unsafe {
            let v = *((f.0 as *mut u8).add(FUT_RESULT_OFFSET) as *const u64);
            native_drop(f.0);
            v
        }
    }

    unsafe extern "C" fn double(ctx: *mut c_void, out: *mut u8) {
        let n = Box::from_raw(ctx as *mut u64);
        (out as *mut u64).write(*n * 2);
    }

    static DROPPED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn drop_ctx(ctx: *mut c_void) {
        drop(Box::from_raw(ctx as *mut u64));
        DROPPED.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn blocking_future_runs_on_the_pool() {
        let ctx = Box::into_raw(Box::new(21u64)) as *mut c_void;
        // SAFETY: valid callbacks and an owned context.
        let f = unsafe { fut_blocking(double, ctx, drop_ctx, 8, None) };
        let v = crate::task::runtime::runtime().block_on(await_fut(f));
        assert_eq!(v, 42);
    }

    #[test]
    fn unpolled_blocking_future_frees_its_context() {
        let before = DROPPED.load(Ordering::SeqCst);
        let ctx = Box::into_raw(Box::new(1u64)) as *mut c_void;
        // SAFETY: as above; dropped without polling.
        unsafe { native_drop(fut_blocking(double, ctx, drop_ctx, 8, None)) };
        assert_eq!(DROPPED.load(Ordering::SeqCst), before + 1);
    }

    static RESULTS_DROPPED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn drop_result(_: *mut u8) {
        RESULTS_DROPPED.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn completer_delivers_or_drops() {
        let mut h = 0u64;
        // SAFETY: valid out-pointer; completed exactly once below.
        let f = unsafe { fut_completer(8, Some(drop_result), &mut h) };
        assert_eq!(poll_once(f), PENDING);
        let value = 7u64;
        std::thread::spawn(move || unsafe { complete(h, &value as *const u64 as *const u8) })
            .join()
            .unwrap();
        assert_eq!(crate::task::runtime::runtime().block_on(await_fut(f)), 7);

        // Dropped before completion: the late result is handed to `drop_result`.
        let before = RESULTS_DROPPED.load(Ordering::SeqCst);
        let f = unsafe { fut_completer(8, Some(drop_result), &mut h) };
        unsafe { native_drop(f) };
        unsafe { complete(h, &value as *const u64 as *const u8) };
        assert_eq!(RESULTS_DROPPED.load(Ordering::SeqCst), before + 1);

        // The handle is gone after its completion: a second `complete` finds nothing (and
        // `complete` reports it as fatal instead of touching freed memory).
        assert!(take_completer(h).is_none());
        assert!(take_completer(u64::MAX).is_none());
    }

    /// The SDK (`velt_native`, which has no runtime code) must describe exactly this table and
    /// these value layouts.
    #[test]
    fn sdk_layouts_match_the_runtime() {
        use std::mem::{align_of, offset_of, size_of};
        use velt_native as sdk;
        assert_eq!(size_of::<VeltRtApi>(), size_of::<sdk::Api>());
        assert_eq!(align_of::<VeltRtApi>(), align_of::<sdk::Api>());
        macro_rules! same_offsets {
            ($($f:ident),*) => {$(
                assert_eq!(offset_of!(VeltRtApi, $f), offset_of!(sdk::Api, $f), stringify!($f));
            )*};
        }
        same_offsets!(
            abi_version,
            size,
            str_new,
            str_bytes,
            str_drop,
            bytes_new,
            bytes_drop,
            bytes_data,
            fut_blocking,
            fut_completer,
            complete,
            fatal,
            fut_poll,
            cx_waker,
            waker_clone,
            waker_wake,
            waker_wake_by_ref,
            waker_drop,
            fut_poll_ext,
            fut_drop,
            spawn,
            tcp_connect,
            stream_from_socket,
            stream_poll_read,
            stream_poll_write,
            stream_poll_flush,
            stream_poll_shutdown,
            stream_socket,
            stream_close,
            now_ns,
            sleep_new,
            sleep_poll,
            sleep_reset,
            sleep_drop,
            worker_index,
            alloc,
            realloc,
            free,
            str_from_utf8,
            str_retain,
            strbuf_spare,
            strbuf_commit,
            str_utf8,
            fut_poll_in
        );
        assert_eq!(NATIVE_ABI_VERSION, sdk::ABI_VERSION);
        assert_eq!(size_of::<VeltStr>(), size_of::<sdk::VeltStr>());
        assert_eq!(align_of::<VeltStr>(), align_of::<sdk::VeltStr>());
        assert_eq!(size_of::<VeltBytes>(), size_of::<sdk::VeltBytes>());
        assert_eq!(align_of::<VeltBytes>(), align_of::<sdk::VeltBytes>());
        type RtErr = crate::result::VeltErr;
        assert_eq!(size_of::<RtErr>(), size_of::<sdk::VeltErr>());
        assert_eq!(offset_of!(RtErr, code), offset_of!(sdk::VeltErr, code));
        assert_eq!(
            offset_of!(RtErr, message),
            offset_of!(sdk::VeltErr, message)
        );
        type RtRes = crate::result::IoResult<u64>;
        assert_eq!(
            offset_of!(RtRes, value),
            offset_of!(sdk::IoResultSlot<u64>, value)
        );
        assert_eq!(size_of::<RtRes>(), size_of::<sdk::IoResultSlot<u64>>());
        type RtResStr = crate::result::IoResult<VeltStr>;
        assert_eq!(
            size_of::<RtResStr>(),
            size_of::<sdk::IoResultSlot<sdk::VeltStr>>()
        );
    }

    #[test]
    fn str_new_decodes_lossily_and_never_joins_halves() {
        let api = unsafe { &*velt_rt_native_api() };
        let cases: [(&[u8], &str); 5] = [
            ("héllo 😀".as_bytes(), "héllo 😀"),
            (b"a\xffb", "a\u{FFFD}b"),
            (
                b"\xed\xa0\xbd\xed\xb8\x80",
                "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}",
            ),
            (b"x\xe2\x82", "x\u{FFFD}"),
            (b"\xf0\x9f\x98", "\u{FFFD}"),
        ];
        for (input, want) in cases {
            let mut s = VeltStr::empty();
            unsafe { (api.str_new)(input.as_ptr(), input.len(), &mut s) };
            assert_eq!(unsafe { s.as_bytes() }, want.as_bytes(), "{input:?}");
            assert_eq!(s.units(), want.encode_utf16().count());
            unsafe { (api.str_drop)(&mut s) };
        }
    }

    #[test]
    fn table_strings_and_bytes() {
        let api = unsafe { &*velt_rt_native_api() };
        assert_eq!(api.abi_version, NATIVE_ABI_VERSION);
        assert_eq!(api.size as usize, std::mem::size_of::<VeltRtApi>());
        let text = "a native string longer than the inline form";
        let mut s = VeltStr::empty();
        let mut len = 0usize;
        unsafe {
            (api.str_new)(text.as_ptr(), text.len(), &mut s);
            let p = (api.str_bytes)(&s, &mut len);
            assert_eq!(std::slice::from_raw_parts(p, len), text.as_bytes());
            (api.str_drop)(&mut s);
            let mut b = VeltBytes::from_vec(vec![]);
            (api.bytes_new)([1u8, 2, 3].as_ptr(), 3, &mut b);
            assert_eq!(b.as_bytes(), &[1, 2, 3]);
            let p = (api.bytes_data)(&b, &mut len);
            assert_eq!(std::slice::from_raw_parts(p, len), &[1, 2, 3]);
            (api.bytes_drop)(&mut b);
        }
    }
}
