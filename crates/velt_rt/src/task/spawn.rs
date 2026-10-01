//! `spawn`: run a compiled future (`velt_rt_spawn`) or an existing heap future
//! (`velt_rt_spawn_fut`) as an independent tokio task, with join handles as `VeltFut`s.
//!
//! A compiled initial state is copied into the task (inline size classes, see `compiled.rs`), so a
//! detached spawn is one allocation (tokio's task cell) and a joinable spawn is two (plus the
//! `JoinObj` leaf). When the task completes, its result bytes are moved into the task's output
//! (`ResultBytes<R>`, inline size classes) and from there into the join handle's result slot at
//! offset 16.

use super::compiled::{with_state_store, Compiled, OwnedStore};
use super::local::Locals;
use super::{context, raw_cx, DropFn, PollFn, SendPtr, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};
use std::ffi::c_void;
use std::future::Future;
use std::mem::MaybeUninit;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::task::JoinHandle;

/// Largest result a joinable task may produce; larger results must be boxed by the compiler.
pub const MAX_RESULT_SIZE: u64 = 256;

/// A task's result bytes, 16-aligned.
#[repr(C, align(16))]
pub struct ResultBytes<const R: usize>([MaybeUninit<u8>; R]);

// SAFETY: result values of compiled futures are `Send` (see `SendPtr`).
unsafe impl<const R: usize> Send for ResultBytes<R> {}

/// What a task runs: a future that leaves its result in memory at `result_ptr` when done.
trait TaskBody: Future<Output = ()> + Send + 'static {
    fn result_ptr(self: Pin<&mut Self>) -> *mut u8;
}

impl<S: OwnedStore> TaskBody for Compiled<S> {
    fn result_ptr(self: Pin<&mut Self>) -> *mut u8 {
        self.state_ptr()
    }
}

/// A heap `VeltFut` spawned as a task (`spawn(p)` where `p` is already a promise value).
struct FutBody {
    fut: SendPtr<VeltFut>,
    /// Promises started while polling `fut` (dropped after it).
    locals: Locals,
}

impl Future for FutBody {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        let f = this.fut.0;
        let r = this.locals.poll_root(cx, |cx| {
            // SAFETY: an owned, live heap future.
            match unsafe { ((*f).poll)(f, raw_cx(cx)) } {
                READY => Poll::Ready(()),
                _ => Poll::Pending,
            }
        });
        crate::io::publish_thread_output();
        r
    }
}

impl TaskBody for FutBody {
    fn result_ptr(self: Pin<&mut Self>) -> *mut u8 {
        // SAFETY: the result slot of every VeltFut is at offset 16.
        unsafe { (self.fut.0 as *mut u8).add(FUT_RESULT_OFFSET) }
    }
}

impl Drop for FutBody {
    fn drop(&mut self) {
        // SAFETY: owned; after READY its result was already moved into the task output.
        unsafe { ((*self.fut.0).drop)(self.fut.0) }
    }
}

/// The spawned future: drives the body, then moves `result_size` result bytes out.
struct TaskFut<B: TaskBody, const R: usize> {
    body: B,
    result_size: usize,
}

impl<B: TaskBody, const R: usize> Future for TaskFut<B, R> {
    type Output = ResultBytes<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<ResultBytes<R>> {
        // SAFETY: structural pinning of `body`; `result_size` is plain data.
        let this = unsafe { self.get_unchecked_mut() };
        let mut body = unsafe { Pin::new_unchecked(&mut this.body) };
        if body.as_mut().poll(cx).is_pending() {
            return Poll::Pending;
        }
        let mut out = ResultBytes([MaybeUninit::uninit(); R]);
        // SAFETY: the body wrote `result_size` (<= R) bytes at its result pointer.
        unsafe {
            std::ptr::copy_nonoverlapping(
                body.result_ptr(),
                out.0.as_mut_ptr() as *mut u8,
                this.result_size,
            )
        };
        Poll::Ready(out)
    }
}

/// Join handle leaf: `{ VeltFut hdr; ResultBytes<R> result /* offset 16 */; JoinHandle }`.
#[repr(C)]
struct JoinObj<const R: usize> {
    hdr: VeltFut,
    result: ResultBytes<R>,
    handle: JoinHandle<ResultBytes<R>>,
}

unsafe extern "C" fn join_poll<const R: usize>(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let obj = &mut *(f as *mut JoinObj<R>);
    match Pin::new(&mut obj.handle).poll(context(cx)) {
        Poll::Pending => PENDING,
        Poll::Ready(Ok(bytes)) => {
            obj.result = bytes;
            READY
        }
        Poll::Ready(Err(e)) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Poll::Ready(Err(_)) => crate::panic::fatal("joined task was cancelled"),
    }
}

unsafe extern "C" fn join_drop<const R: usize>(f: *mut VeltFut) {
    // Dropping the tokio JoinHandle detaches the task (it keeps running, like an unawaited
    // spawned promise).
    drop(Box::from_raw(f as *mut JoinObj<R>));
}

fn spawn_body<B: TaskBody, const R: usize>(
    body: B,
    result_size: usize,
) -> JoinHandle<ResultBytes<R>> {
    super::runtime::handle().spawn(TaskFut::<B, R> { body, result_size })
}

fn spawn_joinable<B: TaskBody, const R: usize>(body: B, result_size: usize) -> *mut VeltFut {
    let obj = Box::new(JoinObj::<R> {
        hdr: VeltFut {
            poll: join_poll::<R>,
            drop: join_drop::<R>,
        },
        result: ResultBytes([MaybeUninit::uninit(); R]),
        handle: spawn_body::<B, R>(body, result_size),
    });
    Box::into_raw(obj) as *mut VeltFut
}

fn spawn_sized<B: TaskBody>(body: B, result_size: u64) -> *mut VeltFut {
    let rs = result_size as usize;
    match result_size {
        0..=16 => spawn_joinable::<B, 16>(body, rs),
        17..=64 => spawn_joinable::<B, 64>(body, rs),
        65..=MAX_RESULT_SIZE => spawn_joinable::<B, 256>(body, rs),
        _ => crate::panic::fatal(
            "spawn: task result larger than 256 bytes (ICE: compiler must box it)",
        ),
    }
}

/// Spawn the compiled future whose initial state is the `state_size` bytes at `state` (copied; the
/// caller gives up ownership of the contents and must not drop them). Returns a join handle: a
/// `VeltFut` whose result slot (offset 16) receives the task's `result_size`-byte result.
/// Dropping the handle detaches the task.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_spawn(
    poll: PollFn,
    drop: DropFn,
    state: *const u8,
    state_size: u64,
    state_align: u64,
    result_size: u64,
) -> *mut VeltFut {
    let (size, align) = (state_size as usize, state_align as usize);
    with_state_store!(size, align, |S| {
        let body = Compiled::<S>::copy_from(poll, drop, state, size, align);
        spawn_sized(body, result_size)
    })
}

/// `spawn(p)` for a promise that is already a heap future (a leaf, a boxed compiled future, another
/// join handle): ownership of `f` moves to the task. Returns a join handle like `velt_rt_spawn`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_spawn_fut(f: *mut VeltFut, result_size: u64) -> *mut VeltFut {
    spawn_sized(
        FutBody {
            fut: SendPtr(f),
            locals: Locals::default(),
        },
        result_size,
    )
}

/// Spawn without a join handle (`spawn(f())` whose result is unused): one allocation.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_spawn_detached(
    poll: PollFn,
    drop: DropFn,
    state: *const u8,
    state_size: u64,
    state_align: u64,
) {
    let (size, align) = (state_size as usize, state_align as usize);
    with_state_store!(size, align, |S| {
        spawn_body::<_, 0>(Compiled::<S>::copy_from(poll, drop, state, size, align), 0);
    })
}
