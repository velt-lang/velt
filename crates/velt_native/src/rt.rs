//! Async Rust on Velt's scheduler: `#[export(async)] async fn`, [`spawn`], and the glue that
//! passes wakers across the table (native_abi.md "Async I/O").
//!
//! The runtime polls an exported future inline, on the task that awaits it, with its own poll
//! context. Here that context becomes an ordinary `Waker`; the functions that wait ([`crate::net`],
//! [`crate::time`], [`RtFuture`]) hand any `Waker` back to the runtime as a [`WakerArg`]: the
//! runtime's waker itself when it is one (no wrapping), else this library's waker behind C
//! functions.

use std::ffi::c_void;
use std::future::Future;
use std::marker::PhantomData;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::pin::Pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use crate::{__panic_message, api, OutRet};

/// `READY` / `PENDING` of the runtime's poll functions.
pub(crate) const READY: u32 = 1;
pub(crate) const PENDING: u32 = 0;
/// The result slot's offset in a runtime future.
const RESULT_OFFSET: usize = 16;

/// `u32 poll(void* state, void* cx, void* out)`: a library future polled by the runtime.
pub type PollFn = unsafe extern "C" fn(state: *mut c_void, cx: *mut c_void, out: *mut u8) -> u32;

/// A library waker as C functions over its data (`clone` returns new owned data; `wake`
/// consumes owned data; `wake_by_ref` and `drop` take any).
#[repr(C)]
pub struct ForeignVTable {
    clone: unsafe extern "C" fn(*const c_void) -> *const c_void,
    wake: unsafe extern "C" fn(*const c_void),
    wake_by_ref: unsafe extern "C" fn(*const c_void),
    drop: unsafe extern "C" fn(*const c_void),
}

/// The waker passed to a runtime function that waits: `runtime` (a runtime waker) when not null,
/// else `data` and `vtable` (this library's waker, borrowed for the call).
#[repr(C)]
pub struct WakerArg {
    runtime: *const c_void,
    data: *const c_void,
    vtable: *const ForeignVTable,
}

/// A runtime waker borrowed for a poll (`data` from `cx_waker`): cloning boxes a runtime copy.
static RT_BORROWED: RawWakerVTable = RawWakerVTable::new(
    |p| unsafe {
        RawWaker::new(
            (api().waker_clone)(p as *const c_void) as *const (),
            &RT_OWNED,
        )
    },
    |p| unsafe { (api().waker_wake_by_ref)(p as *const c_void) },
    |p| unsafe { (api().waker_wake_by_ref)(p as *const c_void) },
    |_| {},
);

/// An owned runtime waker (`data` from `waker_clone`).
static RT_OWNED: RawWakerVTable = RawWakerVTable::new(
    |p| unsafe {
        RawWaker::new(
            (api().waker_clone)(p as *const c_void) as *const (),
            &RT_OWNED,
        )
    },
    |p| unsafe { (api().waker_wake)(p as *mut c_void) },
    |p| unsafe { (api().waker_wake_by_ref)(p as *const c_void) },
    |p| unsafe { (api().waker_drop)(p as *mut c_void) },
);

/// This library's own wakers (a `join`'s, a channel's), for the runtime: `data` is a `Waker`
/// (borrowed for the call, or boxed when the runtime cloned it).
static FOREIGN: ForeignVTable = ForeignVTable {
    clone: clone_foreign,
    wake: wake_foreign,
    wake_by_ref: wake_by_ref_foreign,
    drop: drop_foreign,
};

unsafe extern "C" fn clone_foreign(d: *const c_void) -> *const c_void {
    Box::into_raw(Box::new((*(d as *const Waker)).clone())) as *const c_void
}

unsafe extern "C" fn wake_foreign(d: *const c_void) {
    Box::from_raw(d as *mut Waker).wake();
}

unsafe extern "C" fn wake_by_ref_foreign(d: *const c_void) {
    (*(d as *const Waker)).wake_by_ref();
}

unsafe extern "C" fn drop_foreign(d: *const c_void) {
    drop(Box::from_raw(d as *mut Waker));
}

impl WakerArg {
    /// `w` for a runtime call (valid while `w` is).
    pub(crate) fn of(w: &Waker) -> WakerArg {
        let vt = w.vtable();
        if std::ptr::eq(vt, &RT_BORROWED) || std::ptr::eq(vt, &RT_OWNED) {
            return WakerArg {
                runtime: w.data() as *const c_void,
                data: std::ptr::null(),
                vtable: std::ptr::null(),
            };
        }
        WakerArg {
            runtime: std::ptr::null(),
            data: w as *const Waker as *const c_void,
            vtable: &FOREIGN,
        }
    }
}

/// The task's waker for one poll by the runtime (`cx` from the poll function).
unsafe fn borrowed_waker(cx: *mut c_void) -> ManuallyDrop<Waker> {
    let rt = (api().cx_waker)(cx);
    ManuallyDrop::new(Waker::from_raw(RawWaker::new(
        rt as *const (),
        &RT_BORROWED,
    )))
}

/// Boxes `fut` as a runtime future the runtime polls inline; on completion `done` writes its
/// output to the result slot.
fn into_runtime<F>(
    fut: F,
    result_size: usize,
    done: fn(F::Output, *mut u8),
    panicked: fn(&str, *mut u8),
) -> *mut c_void
where
    F: Future + Send + 'static,
{
    struct State<F: Future> {
        fut: F,
        done: fn(F::Output, *mut u8),
        panicked: fn(&str, *mut u8),
    }
    unsafe extern "C" fn poll<F: Future>(state: *mut c_void, cx: *mut c_void, out: *mut u8) -> u32 {
        let st = &mut *(state as *mut State<F>);
        let waker = borrowed_waker(cx);
        let mut ctx = Context::from_waker(&waker);
        // SAFETY: the state is boxed and never moved.
        let fut = Pin::new_unchecked(&mut st.fut);
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fut.poll(&mut ctx))) {
            Ok(Poll::Pending) => PENDING,
            Ok(Poll::Ready(v)) => {
                (st.done)(v, out);
                READY
            }
            Err(p) => {
                (st.panicked)(&__panic_message(p), out);
                READY
            }
        }
    }
    unsafe extern "C" fn drop_state<F: Future>(state: *mut c_void) {
        drop(Box::from_raw(state as *mut State<F>));
    }
    let state = Box::into_raw(Box::new(State {
        fut,
        done,
        panicked,
    })) as *mut c_void;
    // SAFETY: callbacks for this `State<F>`.
    unsafe { (api().fut_poll)(poll::<F>, state, drop_state::<F>, result_size) }
}

/// The future behind an `#[export(async)]` function: `fut` runs on the awaiting task's worker,
/// and its output is the `Promise`'s result. A panic becomes the result's error (or a fatal
/// error for results that cannot fail).
#[doc(hidden)]
pub fn __async<R, F>(fut: F) -> *mut c_void
where
    R: OutRet,
    F: Future<Output = R> + Send + 'static,
{
    fn done<R: OutRet>(r: R, out: *mut u8) {
        // SAFETY: `out` is the future's result slot, sized for `R::Slot`.
        unsafe { r.write(out as *mut R::Slot) }
    }
    fn panicked<R: OutRet>(msg: &str, out: *mut u8) {
        done::<R>(R::panicked(msg), out)
    }
    into_runtime(
        fut,
        std::mem::size_of::<R::Slot>(),
        done::<R>,
        panicked::<R>,
    )
}

/// Runs `fut` as a detached task on the runtime's scheduler (this worker, when called from one).
/// A panic in it is a fatal error.
pub fn spawn<F>(fut: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    fn done(_: (), _: *mut u8) {}
    fn panicked(msg: &str, _: *mut u8) {
        crate::fatal(msg)
    }
    let f = into_runtime(fut, 0, done, panicked);
    // SAFETY: a result-size-0 runtime future, handed over.
    unsafe { (api().spawn)(f) }
}

/// The runtime's worker this thread is (`None` off the workers, and with tokio's work-stealing
/// scheduler): for per-worker state, like a pool handing a task a connection on its worker.
pub fn worker_index() -> Option<usize> {
    usize::try_from((api().worker_index)()).ok()
}

/// A runtime future (one the table returned) awaited from Rust; its result slot holds `T`.
pub(crate) struct RtFuture<T> {
    raw: *mut c_void,
    _t: PhantomData<fn() -> T>,
}

// SAFETY: runtime futures may be polled on any worker.
unsafe impl<T> Send for RtFuture<T> {}

impl<T> RtFuture<T> {
    /// # Safety
    /// `raw` is a runtime future whose result slot holds a `T`.
    pub(crate) unsafe fn new(raw: *mut c_void) -> RtFuture<T> {
        RtFuture {
            raw,
            _t: PhantomData,
        }
    }
}

impl<T> Future for RtFuture<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let this = self.get_mut();
        if this.raw.is_null() {
            crate::fatal("a runtime future was polled after it finished");
        }
        let arg = WakerArg::of(cx.waker());
        // SAFETY: a live runtime future and a waker valid for the call.
        if unsafe { (api().fut_poll_ext)(this.raw, &arg) } == PENDING {
            return Poll::Pending;
        }
        let raw = std::mem::replace(&mut this.raw, std::ptr::null_mut());
        // SAFETY: READY: the slot holds a `T`, moved out here; then the future is freed
        // (it never drops its slot).
        unsafe {
            let mut v = MaybeUninit::<T>::uninit();
            std::ptr::copy_nonoverlapping(
                (raw as *const u8).add(RESULT_OFFSET),
                v.as_mut_ptr() as *mut u8,
                std::mem::size_of::<T>(),
            );
            (api().fut_drop)(raw);
            Poll::Ready(v.assume_init())
        }
    }
}

impl<T> Drop for RtFuture<T> {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: not finished: the runtime cancels and frees it.
            unsafe { (api().fut_drop)(self.raw) };
        }
    }
}
