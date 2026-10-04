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

use std::alloc::Layout;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::Waker;

use crate::bytes::VeltBytes;
use crate::str::VeltStr;
use crate::task::{context, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};

/// The table version this runtime provides.
pub const NATIVE_ABI_VERSION: u32 = 1;

/// The work of a [`fut_blocking`] future: runs on the blocking pool with its context and the result
/// slot to fill.
pub type WorkFn = unsafe extern "C" fn(ctx: *mut c_void, out: *mut u8);
/// Frees a context whose work never ran.
pub type DropCtxFn = unsafe extern "C" fn(ctx: *mut c_void);
/// Drops a finished result nobody claimed (the future was dropped first).
pub type DropResultFn = unsafe extern "C" fn(result: *mut u8);

/// `VeltRtApi`, ABI version 1 (native_abi.md "The function table").
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
};

/// `const VeltRtApi* velt_rt_native_api(void)`: the table, for generated code to pass to each
/// native package's `velt_native_init_<pkg>` before `main` runs.
#[no_mangle]
pub extern "C" fn velt_rt_native_api() -> *const VeltRtApi {
    &VELT_RT_API
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

/// Result bytes, 16-aligned (results have align <= 8; the slot itself is at a 16-byte offset).
type Buf = Vec<u128>;

fn buf(size: usize) -> Buf {
    vec![0u128; size.div_ceil(16).max(1)]
}

enum Slot {
    /// Not finished; the waker of the last poll.
    Pending(Option<Waker>),
    /// Finished, result not yet moved into the future's slot.
    Ready(Buf),
    /// The result was moved out (the future completed).
    Taken,
    /// The future was dropped before the result arrived.
    Cancelled,
}

/// Shared between a native future and whoever completes it (the blocking task or a completion
/// handle).
struct Shared {
    slot: Mutex<Slot>,
    size: usize,
    drop_result: Option<DropResultFn>,
}

// SAFETY: `drop_result` is a plain function pointer into a native library that is never unloaded;
// result bytes are owned values the contract allows to move between threads.
unsafe impl Send for Shared {}
// SAFETY: as above; all mutable state is behind the mutex.
unsafe impl Sync for Shared {}

impl Shared {
    /// Deliver the result (`size` bytes at `src`, moved). After cancellation it is dropped.
    unsafe fn complete(&self, src: *const u8) {
        let mut b = buf(self.size);
        std::ptr::copy_nonoverlapping(src, b.as_mut_ptr() as *mut u8, self.size);
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        match std::mem::replace(&mut *slot, Slot::Taken) {
            Slot::Pending(waker) => {
                *slot = Slot::Ready(b);
                drop(slot);
                if let Some(w) = waker {
                    w.wake();
                }
            }
            Slot::Cancelled => {
                *slot = Slot::Cancelled;
                drop(slot);
                if let Some(d) = self.drop_result {
                    d(b.as_mut_ptr() as *mut u8);
                }
            }
            Slot::Ready(_) | Slot::Taken => crate::panic::fatal("native future completed twice"),
        }
    }
}

/// The blocking work of a [`fut_blocking`] future, until its first poll hands it to the pool.
struct Work {
    work: WorkFn,
    ctx: *mut c_void,
    drop_ctx: DropCtxFn,
}

// SAFETY: the contract requires `ctx` to be movable to the blocking pool's threads.
unsafe impl Send for Work {}

impl Drop for Work {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            // SAFETY: the work never ran, so the context is still owned here.
            unsafe { (self.drop_ctx)(self.ctx) };
        }
    }
}

/// Lives in front of the `VeltFut` header: `[Meta][VeltFut][result: size bytes]`.
#[repr(C, align(16))]
struct Meta {
    shared: Arc<Shared>,
    start: Option<Work>,
    layout: Layout,
}

const META: usize = std::mem::size_of::<Meta>();
const _: () = assert!(META.is_multiple_of(16));

unsafe fn meta<'a>(f: *mut VeltFut) -> &'a mut Meta {
    &mut *((f as *mut u8).sub(META) as *mut Meta)
}

fn new_fut(shared: Arc<Shared>, start: Option<Work>) -> *mut VeltFut {
    let size = META + FUT_RESULT_OFFSET + shared.size;
    let layout = Layout::from_size_align(size, 16).expect("ICE: native future layout");
    // SAFETY: non-zero size; every field is initialized below before use.
    unsafe {
        let base = std::alloc::alloc(layout);
        if base.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        (base as *mut Meta).write(Meta {
            shared,
            start,
            layout,
        });
        let f = base.add(META) as *mut VeltFut;
        f.write(VeltFut {
            poll: native_poll,
            drop: native_drop,
        });
        f
    }
}

unsafe extern "C" fn native_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let m = meta(f);
    if let Some(mut work) = m.start.take() {
        let shared = m.shared.clone();
        let size = shared.size;
        let ctx = std::mem::replace(&mut work.ctx, std::ptr::null_mut());
        let (run, ctx) = (work.work, crate::task::SendPtr(ctx));
        drop(work);
        crate::task::runtime::handle().spawn_blocking(move || {
            let ctx = ctx;
            let mut b = buf(size);
            // SAFETY: the library's work function fills `size` bytes and consumes `ctx`.
            unsafe {
                run(ctx.0, b.as_mut_ptr() as *mut u8);
                shared.complete(b.as_ptr() as *const u8);
            }
        });
    }
    let mut slot = m.shared.slot.lock().unwrap_or_else(|e| e.into_inner());
    match &mut *slot {
        Slot::Pending(waker) => {
            let w = context(cx).waker();
            if !waker.as_ref().is_some_and(|old| old.will_wake(w)) {
                *waker = Some(w.clone());
            }
            PENDING
        }
        Slot::Ready(_) => {
            let Slot::Ready(b) = std::mem::replace(&mut *slot, Slot::Taken) else {
                unreachable!()
            };
            let dst = (f as *mut u8).add(FUT_RESULT_OFFSET);
            std::ptr::copy_nonoverlapping(b.as_ptr() as *const u8, dst, m.shared.size);
            READY
        }
        Slot::Taken => READY,
        Slot::Cancelled => crate::panic::fatal("ICE: polled a dropped native future"),
    }
}

unsafe extern "C" fn native_drop(f: *mut VeltFut) {
    let base = (f as *mut u8).sub(META) as *mut Meta;
    let m = base.read();
    drop(m.start); // never polled: its context is freed by `Work::drop`
    let mut slot = m.shared.slot.lock().unwrap_or_else(|e| e.into_inner());
    match std::mem::replace(&mut *slot, Slot::Cancelled) {
        Slot::Ready(mut b) => {
            drop(slot);
            if let Some(d) = m.shared.drop_result {
                d(b.as_mut_ptr() as *mut u8);
            }
        }
        Slot::Taken => *slot = Slot::Taken,
        Slot::Pending(_) | Slot::Cancelled => {}
    }
    std::alloc::dealloc(base as *mut u8, m.layout);
}

fn shared(result_size: usize, drop_result: Option<DropResultFn>) -> Arc<Shared> {
    Arc::new(Shared {
        slot: Mutex::new(Slot::Pending(None)),
        size: result_size,
        drop_result,
    })
}

/// `VeltFut* fut_blocking(work, ctx, drop_ctx, result_size, drop_result)`: a future whose first
/// poll runs `work(ctx, out)` on the runtime's blocking pool; `work` writes `result_size` bytes to
/// `out` (and owns `ctx` from then on). Dropped before its first poll, `drop_ctx(ctx)` runs
/// instead; dropped while `work` runs, the finished result goes to `drop_result` (if any).
pub unsafe extern "C" fn fut_blocking(
    work: WorkFn,
    ctx: *mut c_void,
    drop_ctx: DropCtxFn,
    result_size: usize,
    drop_result: Option<DropResultFn>,
) -> *mut VeltFut {
    let start = Work {
        work,
        ctx,
        drop_ctx,
    };
    new_fut(shared(result_size, drop_result), Some(start))
}

/// Completion handles: ids (never reused) of futures waiting for `complete`. A handle is removed
/// by its first `complete`, so a second one, or an id that was never handed out, finds nothing
/// instead of freed memory.
type Completers = Mutex<(u64, HashMap<u64, Arc<Shared>>)>;

fn completers() -> &'static Completers {
    static C: OnceLock<Completers> = OnceLock::new();
    C.get_or_init(|| Mutex::new((0, HashMap::new())))
}

fn take_completer(handle: u64) -> Option<Arc<Shared>> {
    let mut c = completers().lock().unwrap_or_else(|e| e.into_inner());
    c.1.remove(&handle)
}

/// `VeltFut* fut_completer(result_size, drop_result, u64* handle)`: a pending future completed by
/// one later `complete(*handle, result)` from any thread (e.g. the library's own async runtime).
pub unsafe extern "C" fn fut_completer(
    result_size: usize,
    drop_result: Option<DropResultFn>,
    handle: *mut u64,
) -> *mut VeltFut {
    let s = shared(result_size, drop_result);
    let mut c = completers().lock().unwrap_or_else(|e| e.into_inner());
    c.0 += 1;
    let id = c.0;
    c.1.insert(id, s.clone());
    drop(c);
    handle.write(id);
    new_fut(s, None)
}

/// `void complete(u64 handle, const void* result)`: moves `result_size` bytes into the future of
/// `handle` (exactly once per handle); if that future was dropped, `drop_result` gets them.
/// A handle that was already completed (or never handed out) is a fatal error of the library, not
/// memory corruption.
pub unsafe extern "C" fn complete(handle: u64, result: *const u8) {
    match take_completer(handle) {
        Some(s) => s.complete(result),
        None => crate::panic::fatal(
            "a native library completed a future twice, or with an unknown handle",
        ),
    }
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
        unsafe { native_poll(f, crate::task::raw_cx(&mut cx)) }
    }

    async fn await_fut(f: *mut VeltFut) -> u64 {
        let f = crate::task::SendPtr(f);
        std::future::poll_fn(move |cx| {
            match unsafe { native_poll(f.0, crate::task::raw_cx(cx)) } {
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
            fatal
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
