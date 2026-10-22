//! Async building blocks for native libraries (native_abi.md "Async I/O"): futures the library
//! polls itself, wakers, detached tasks, TCP streams and timers, all on whichever scheduler the
//! program runs. A library's future is polled inline by the task awaiting it, its sockets are
//! the runtime's own ([`crate::net::sys::TcpStream`]: on the Velt backend registered with the
//! worker's reactor), so a driver in a package runs as fast as one built into the runtime.
//!
//! Nothing Rust-specific crosses: `cx` is the runtime's opaque poll context, a runtime waker is
//! a `Waker` behind a pointer (owned ones boxed), a stream a `Box<TcpStream>`, a sleep a
//! `Box<Sleep>`. A library carries its own copy of Rust std (possibly from another compiler), so
//! it never touches these as Rust values, only through the functions here.
//!
//! The functions that wait (a stream's reads and writes, a sleep, a runtime future) take a
//! [`WakerArg`]: the runtime waker itself when the library's waker is one (no wrapping, the
//! common case), else the library's waker as `(data, vtable)` of C functions. A library polls its
//! sockets through combinators that wrap the waker (a `join`, a driver's own channels), so a
//! `cx` alone would not do.

use std::alloc::Layout;
use std::ffi::c_void;
use std::future::Future;
use std::io;
use std::mem::MaybeUninit;
use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::net::sys::TcpStream;
use crate::result::IoResult;
use crate::task::runtime::time::{sleep_until, Sleep};
use crate::task::{context, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};

/// `u32 poll(void* state, void* cx, void* out)`: polls a library future; on `READY` it has written
/// its result to `out`.
pub type NativePollFn =
    unsafe extern "C" fn(state: *mut c_void, cx: *mut c_void, out: *mut u8) -> u32;
/// Frees a library future's state.
pub type DropStateFn = unsafe extern "C" fn(state: *mut c_void);

/// Lives in front of the `VeltFut` header: `[PollMeta][VeltFut][result]`.
#[repr(C, align(16))]
struct PollMeta {
    poll: NativePollFn,
    /// Null once the future finished (its state is freed then).
    state: *mut c_void,
    drop_state: DropStateFn,
    layout: Layout,
    /// While it waits: an awaited library future keeps the process alive, as the runtime's own
    /// I/O does (task/quiet.rs; a library's own spawned tasks do not).
    pending: Option<crate::task::quiet::Pending>,
}

const META: usize = std::mem::size_of::<PollMeta>();
const _: () = assert!(META.is_multiple_of(16));

unsafe fn meta<'a>(f: *mut VeltFut) -> &'a mut PollMeta {
    &mut *((f as *mut u8).sub(META) as *mut PollMeta)
}

/// `VeltFut* fut_poll(poll, state, drop_state, result_size)`: a future the library polls itself,
/// on the thread of the task awaiting it (no thread hop). `poll` gets the task's `cx` for the
/// waker functions below. The state is freed with `drop_state` when the future finishes or is
/// dropped, whichever comes first.
pub unsafe extern "C" fn fut_poll(
    poll: NativePollFn,
    state: *mut c_void,
    drop_state: DropStateFn,
    result_size: usize,
) -> *mut VeltFut {
    let size = META + FUT_RESULT_OFFSET + result_size;
    let layout = Layout::from_size_align(size, 16).expect("ICE: native future layout");
    let base = std::alloc::alloc(layout);
    if base.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    (base as *mut PollMeta).write(PollMeta {
        poll,
        state,
        drop_state,
        layout,
        pending: None,
    });
    let f = base.add(META) as *mut VeltFut;
    f.write(VeltFut {
        poll: poll_native,
        drop: drop_native,
    });
    f
}

unsafe extern "C" fn poll_native(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let m = meta(f);
    if m.state.is_null() {
        return READY;
    }
    let r = (m.poll)(m.state, cx, (f as *mut u8).add(FUT_RESULT_OFFSET));
    if r == READY {
        let state = std::mem::replace(&mut m.state, std::ptr::null_mut());
        (m.drop_state)(state);
        m.pending = None;
    } else if m.pending.is_none() {
        m.pending = Some(crate::task::quiet::Pending::new());
    }
    r
}

unsafe extern "C" fn drop_native(f: *mut VeltFut) {
    let base = (f as *mut u8).sub(META) as *mut PollMeta;
    let m = base.read();
    if !m.state.is_null() {
        (m.drop_state)(m.state);
    }
    std::alloc::dealloc(base as *mut u8, m.layout);
}

/// `const void* cx_waker(void* cx)`: the waker of the task being polled, borrowed for the poll.
pub unsafe extern "C" fn cx_waker(cx: *mut c_void) -> *const c_void {
    context(cx).waker() as *const Waker as *const c_void
}

/// `void* waker_clone(const void* w)`: an owned copy (boxed) of a runtime waker, borrowed or
/// owned.
pub unsafe extern "C" fn waker_clone(w: *const c_void) -> *mut c_void {
    Box::into_raw(Box::new((*(w as *const Waker)).clone())) as *mut c_void
}

/// `void waker_wake(void* w)`: wakes and frees an owned waker.
pub unsafe extern "C" fn waker_wake(w: *mut c_void) {
    Box::from_raw(w as *mut Waker).wake();
}

/// `void waker_wake_by_ref(const void* w)`: wakes a runtime waker, keeping it.
pub unsafe extern "C" fn waker_wake_by_ref(w: *const c_void) {
    (*(w as *const Waker)).wake_by_ref();
}

/// `void waker_drop(void* w)`: frees an owned waker.
pub unsafe extern "C" fn waker_drop(w: *mut c_void) {
    drop(Box::from_raw(w as *mut Waker));
}

/// A library's own waker, as C functions over its `data`: `clone` returns new owned data,
/// `wake` consumes owned data, `wake_by_ref` and `drop` take any.
#[repr(C)]
pub struct ForeignVTable {
    pub clone: unsafe extern "C" fn(*const c_void) -> *const c_void,
    pub wake: unsafe extern "C" fn(*const c_void),
    pub wake_by_ref: unsafe extern "C" fn(*const c_void),
    pub drop: unsafe extern "C" fn(*const c_void),
}

/// The waker a library passes to a function that waits: `runtime` (a runtime waker, borrowed
/// for the call) when not null, else the library's waker `data` (borrowed) with its `vtable`.
#[repr(C)]
pub struct WakerArg {
    pub runtime: *const c_void,
    pub data: *const c_void,
    pub vtable: *const ForeignVTable,
}

/// A library waker as the runtime holds it: owned data and its table.
struct Foreign {
    data: *const c_void,
    vtable: *const ForeignVTable,
}

unsafe fn foreign_raw(data: *const c_void, vtable: *const ForeignVTable) -> RawWaker {
    let owned = Box::new(Foreign { data, vtable });
    RawWaker::new(Box::into_raw(owned) as *const (), &FOREIGN)
}

/// Wraps the library waker `(data, vtable)` held by a `Foreign`.
static FOREIGN: RawWakerVTable = RawWakerVTable::new(
    |p| unsafe {
        let f = &*(p as *const Foreign);
        foreign_raw(((*f.vtable).clone)(f.data), f.vtable)
    },
    |p| unsafe {
        let f = Box::from_raw(p as *mut Foreign);
        ((*f.vtable).wake)(f.data);
    },
    |p| unsafe {
        let f = &*(p as *const Foreign);
        ((*f.vtable).wake_by_ref)(f.data);
    },
    |p| unsafe {
        let f = Box::from_raw(p as *mut Foreign);
        ((*f.vtable).drop)(f.data);
    },
);

/// A borrowed library waker: cloning makes a [`FOREIGN`] one through the library's `clone`.
static BORROWED: RawWakerVTable = RawWakerVTable::new(
    |p| unsafe {
        let a = &*(p as *const WakerArg);
        foreign_raw(((*a.vtable).clone)(a.data), a.vtable)
    },
    |p| unsafe {
        let a = &*(p as *const WakerArg);
        ((*a.vtable).wake_by_ref)(a.data);
    },
    |p| unsafe {
        let a = &*(p as *const WakerArg);
        ((*a.vtable).wake_by_ref)(a.data);
    },
    |_| {},
);

/// Runs `f` with a `Context` for the library's waker `arg`.
unsafe fn with_cx<R>(arg: *const WakerArg, f: impl FnOnce(&mut Context<'_>) -> R) -> R {
    let a = &*arg;
    if !a.runtime.is_null() {
        let mut cx = Context::from_waker(&*(a.runtime as *const Waker));
        return f(&mut cx);
    }
    let w =
        std::mem::ManuallyDrop::new(Waker::from_raw(RawWaker::new(arg as *const (), &BORROWED)));
    let mut cx = Context::from_waker(&w);
    f(&mut cx)
}

/// `u32 fut_poll_ext(VeltFut* f, const WakerArg* w)`: polls a runtime future (one this table
/// returned) from library code.
pub unsafe extern "C" fn fut_poll_ext(f: *mut VeltFut, w: *const WakerArg) -> u32 {
    with_cx(w, |cx| {
        let raw = crate::task::raw_cx(cx);
        ((*f).poll)(f, raw.ptr())
    })
}

/// `void fut_drop(VeltFut* f)`: frees a runtime future the library did not hand to Velt.
pub unsafe extern "C" fn fut_drop(f: *mut VeltFut) {
    ((*f).drop)(f)
}

/// A `VeltFut` the runtime polls as a detached task.
struct Detached(*mut VeltFut);

// SAFETY: a native library's future may run on any worker (native_abi.md "Async I/O").
unsafe impl Send for Detached {}

impl Detached {
    fn poll(&mut self, cx: &mut std::task::Context<'_>) -> Poll<()> {
        let raw = crate::task::raw_cx(cx);
        // SAFETY: a live future; `raw` outlives the call.
        match unsafe { ((*self.0).poll)(self.0, raw.ptr()) } {
            PENDING => Poll::Pending,
            _ => Poll::Ready(()),
        }
    }
}

impl Drop for Detached {
    fn drop(&mut self) {
        // SAFETY: owned; freed once.
        unsafe { ((*self.0).drop)(self.0) };
    }
}

/// `void spawn(VeltFut* f)`: runs `f` (result size 0) as a detached task, on this worker when
/// called from one. The runtime frees it when it finishes.
pub unsafe extern "C" fn spawn(f: *mut VeltFut) {
    let mut task = Detached(f);
    // Detached: the join handle is dropped.
    drop(crate::task::runtime::spawn(async move {
        std::future::poll_fn(|cx| task.poll(cx)).await;
    }));
}

/// The error code of an I/O error for a library: the OS error number when there is one
/// (positive; the library's std maps it to the same kind), else the negated runtime code
/// (rt_abi_async.md §3).
fn err_code(e: &io::Error) -> i32 {
    match e.raw_os_error() {
        Some(n) if n > 0 => n,
        _ => -crate::result::code_of(e),
    }
}

/// `VeltFut* tcp_connect(const u8* addr, usize len)`: connects to `"host:port"` (resolving the
/// name off the workers); the future's result is `IoResult<u64>`, a stream handle.
pub unsafe extern "C" fn tcp_connect(addr: *const u8, len: usize) -> *mut VeltFut {
    let addr = String::from_utf8_lossy(std::slice::from_raw_parts(addr, len)).into_owned();
    crate::task::leaf::new_leaf(async move {
        IoResult::from_io(TcpStream::connect(&addr).await, |s| {
            Box::into_raw(Box::new(s)) as u64
        })
    })
}

/// `i32 stream_from_socket(i64 socket, u64* out)`: a stream of a connected stream socket the
/// library opened, TCP or (Unix) a Unix domain socket (a file descriptor, or a `SOCKET` on
/// Windows), which the stream owns from then on. 0, or an error code.
pub unsafe extern "C" fn stream_from_socket(socket: i64, out: *mut u64) -> i32 {
    #[cfg(unix)]
    let s = {
        use std::os::fd::FromRawFd;
        std::net::TcpStream::from_raw_fd(socket as i32)
    };
    #[cfg(windows)]
    let s = {
        use std::os::windows::io::FromRawSocket;
        std::net::TcpStream::from_raw_socket(socket as u64)
    };
    let r = s
        .set_nonblocking(true)
        .and_then(|()| TcpStream::from_std(s));
    match r {
        Ok(s) => {
            out.write(Box::into_raw(Box::new(s)) as u64);
            0
        }
        Err(e) => err_code(&e),
    }
}

unsafe fn stream<'a>(h: u64) -> Pin<&'a mut TcpStream> {
    Pin::new(&mut *(h as *mut TcpStream))
}

/// `u32 stream_poll_read(u64 h, const WakerArg* w, u8* buf, usize cap, usize* n, i32* err)`: `READY`
/// with `*n` bytes read (0: end of stream) or `*err` set, or `PENDING`.
pub unsafe extern "C" fn stream_poll_read(
    h: u64,
    w: *const WakerArg,
    buf: *mut u8,
    cap: usize,
    n: *mut usize,
    err: *mut i32,
) -> u32 {
    let slice = std::slice::from_raw_parts_mut(buf as *mut MaybeUninit<u8>, cap);
    let mut rb = ReadBuf::uninit(slice);
    match with_cx(w, |cx| stream(h).poll_read(cx, &mut rb)) {
        Poll::Pending => PENDING,
        Poll::Ready(r) => {
            n.write(rb.filled().len());
            err.write(r.err().map_or(0, |e| err_code(&e)));
            READY
        }
    }
}

/// `u32 stream_poll_write(u64 h, const WakerArg* w, const u8* buf, usize len, usize* n, i32* err)`.
pub unsafe extern "C" fn stream_poll_write(
    h: u64,
    w: *const WakerArg,
    buf: *const u8,
    len: usize,
    n: *mut usize,
    err: *mut i32,
) -> u32 {
    let data = if len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(buf, len)
    };
    match with_cx(w, |cx| stream(h).poll_write(cx, data)) {
        Poll::Pending => PENDING,
        Poll::Ready(Ok(k)) => {
            n.write(k);
            err.write(0);
            READY
        }
        Poll::Ready(Err(e)) => {
            n.write(0);
            err.write(err_code(&e));
            READY
        }
    }
}

fn ready_status(p: Poll<io::Result<()>>, err: *mut i32) -> u32 {
    match p {
        Poll::Pending => PENDING,
        Poll::Ready(r) => {
            // SAFETY: the caller's out-pointer.
            unsafe { err.write(r.err().map_or(0, |e| err_code(&e))) };
            READY
        }
    }
}

/// `u32 stream_poll_flush(u64 h, const WakerArg* w, i32* err)`.
pub unsafe extern "C" fn stream_poll_flush(h: u64, w: *const WakerArg, err: *mut i32) -> u32 {
    ready_status(with_cx(w, |cx| stream(h).poll_flush(cx)), err)
}

/// `u32 stream_poll_shutdown(u64 h, const WakerArg* w, i32* err)`: shuts down the writing half.
pub unsafe extern "C" fn stream_poll_shutdown(h: u64, w: *const WakerArg, err: *mut i32) -> u32 {
    ready_status(with_cx(w, |cx| stream(h).poll_shutdown(cx)), err)
}

/// `i64 stream_socket(u64 h)`: the stream's socket (for socket options), still owned by it.
pub unsafe extern "C" fn stream_socket(h: u64) -> i64 {
    let s = &*(h as *const TcpStream);
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        s.sock_ref().as_raw_fd() as i64
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        s.sock_ref().as_raw_socket() as i64
    }
}

/// `void stream_close(u64 h)`: closes and frees a stream.
pub unsafe extern "C" fn stream_close(h: u64) {
    drop(Box::from_raw(h as *mut TcpStream));
}

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// `u64 now_ns(void)`: monotonic nanoseconds (the clock of [`sleep_new`]).
pub extern "C" fn now_ns() -> u64 {
    epoch().elapsed().as_nanos() as u64
}

fn at(ns: u64) -> Instant {
    epoch() + Duration::from_nanos(ns)
}

/// `void* sleep_new(u64 deadline_ns)`: a timer for [`now_ns`] time `deadline_ns`.
pub extern "C" fn sleep_new(deadline_ns: u64) -> *mut c_void {
    Box::into_raw(Box::new(sleep_until(at(deadline_ns)))) as *mut c_void
}

/// `u32 sleep_poll(void* s, const WakerArg* w)`: `READY` once the deadline passed.
pub unsafe extern "C" fn sleep_poll(s: *mut c_void, w: *const WakerArg) -> u32 {
    match with_cx(w, |cx| Pin::new(&mut *(s as *mut Sleep)).poll(cx)) {
        Poll::Pending => PENDING,
        Poll::Ready(()) => READY,
    }
}

/// `void sleep_reset(void* s, u64 deadline_ns)`.
pub unsafe extern "C" fn sleep_reset(s: *mut c_void, deadline_ns: u64) {
    (*(s as *mut Sleep)).reset(at(deadline_ns));
}

/// `void sleep_drop(void* s)`.
pub unsafe extern "C" fn sleep_drop(s: *mut c_void) {
    drop(Box::from_raw(s as *mut Sleep));
}

/// `i64 worker_index(void)`: the worker this thread is (-1 on other threads and with tokio's
/// work-stealing scheduler): for keeping a library's per-worker state, like a pool that hands
/// a task a connection driven on its own worker.
pub extern "C" fn worker_index() -> i64 {
    crate::task::runtime::current_worker().map_or(-1, |i| i as i64)
}
