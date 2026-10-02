//! `spawn`: run a compiled future (`velt_rt_spawn`) or an existing heap future
//! (`velt_rt_spawn_fut`) as an independent tokio task, with join handles as `VeltFut`s.
//!
//! A compiled initial state is copied into the task (inline size classes, see `compiled.rs`), so a
//! detached spawn is one allocation (tokio's task cell) and a joinable spawn is two (plus the
//! `JoinObj` leaf). When the task completes, its result bytes are moved into the task's output
//! (`ResultBytes<R>`, inline size classes) and from there into the join handle's result slot at
//! offset 16. The output also keeps the result's drop glue until the handle claims the result
//! or is dropped: drop glue kept with a value in flight, allowed by the hot-reload rule
//! (rt_abi_async.md §13.5).

use super::all::ResultDropFn;
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

/// A task's result bytes, 16-aligned (plain bytes: copying them moves the value they hold).
#[repr(C, align(16))]
#[derive(Clone, Copy)]
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

/// A task's output: its result bytes and how to drop them if the join handle never takes them.
pub struct TaskOutput<const R: usize> {
    bytes: ResultBytes<R>,
    /// Cleared once the join handle moved the result out.
    result_drop: Option<ResultDropFn>,
}

impl<const R: usize> Drop for TaskOutput<R> {
    fn drop(&mut self) {
        if let Some(d) = self.result_drop {
            // SAFETY: an unclaimed result, written by the finished task (16-aligned).
            unsafe { d(self.bytes.0.as_mut_ptr() as *mut u8) }
        }
    }
}

/// The spawned future: drives the body, then moves `result_size` result bytes out.
struct TaskFut<B: TaskBody, const R: usize> {
    body: B,
    result_size: usize,
    result_drop: Option<ResultDropFn>,
}

impl<B: TaskBody, const R: usize> Future for TaskFut<B, R> {
    type Output = TaskOutput<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<TaskOutput<R>> {
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
        Poll::Ready(TaskOutput {
            bytes: out,
            result_drop: this.result_drop,
        })
    }
}

/// Join handle leaf: `{ VeltFut hdr; ResultBytes<R> result /* offset 16 */; JoinHandle }`.
#[repr(C)]
struct JoinObj<const R: usize> {
    hdr: VeltFut,
    result: ResultBytes<R>,
    handle: JoinHandle<TaskOutput<R>>,
}

unsafe extern "C" fn join_poll<const R: usize>(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let obj = &mut *(f as *mut JoinObj<R>);
    match Pin::new(&mut obj.handle).poll(context(cx)) {
        Poll::Pending => PENDING,
        Poll::Ready(Ok(mut out)) => {
            out.result_drop = None;
            obj.result = out.bytes;
            READY
        }
        Poll::Ready(Err(e)) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Poll::Ready(Err(_)) => crate::panic::fatal("joined task was cancelled"),
    }
}

unsafe extern "C" fn join_drop<const R: usize>(f: *mut VeltFut) {
    // Dropping the tokio JoinHandle detaches the task (it keeps running, like an unawaited
    // spawned promise); tokio then drops its output, which disposes of an unclaimed result.
    drop(Box::from_raw(f as *mut JoinObj<R>));
}

fn spawn_body<B: TaskBody, const R: usize>(
    body: B,
    result_size: usize,
    result_drop: Option<ResultDropFn>,
) -> JoinHandle<TaskOutput<R>> {
    super::runtime::handle().spawn(TaskFut::<B, R> {
        body,
        result_size,
        result_drop,
    })
}

fn spawn_joinable<B: TaskBody, const R: usize>(
    body: B,
    result_size: usize,
    result_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    let obj = Box::new(JoinObj::<R> {
        hdr: VeltFut {
            poll: join_poll::<R>,
            drop: join_drop::<R>,
        },
        result: ResultBytes([MaybeUninit::uninit(); R]),
        handle: spawn_body::<B, R>(body, result_size, result_drop),
    });
    Box::into_raw(obj) as *mut VeltFut
}

fn spawn_sized<B: TaskBody>(
    body: B,
    result_size: u64,
    result_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    let rs = result_size as usize;
    match result_size {
        0..=16 => spawn_joinable::<B, 16>(body, rs, result_drop),
        17..=64 => spawn_joinable::<B, 64>(body, rs, result_drop),
        65..=MAX_RESULT_SIZE => spawn_joinable::<B, 256>(body, rs, result_drop),
        _ => crate::panic::fatal(
            "spawn: task result larger than 256 bytes (ICE: compiler must box it)",
        ),
    }
}

/// Spawn the compiled future whose initial state is the `state_size` bytes at `state` (copied; the
/// caller gives up ownership of the contents and must not drop them). Returns a join handle: a
/// `VeltFut` whose result slot (offset 16) receives the task's `result_size`-byte result.
/// Dropping the handle detaches the task; a result it never claims is dropped with
/// `result_drop` (null: nothing to drop).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_spawn(
    poll: PollFn,
    drop: DropFn,
    state: *const u8,
    state_size: u64,
    state_align: u64,
    result_size: u64,
    result_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    let (size, align) = (state_size as usize, state_align as usize);
    with_state_store!(size, align, |S| {
        let body = Compiled::<S>::copy_from(poll, drop, state, size, align);
        spawn_sized(body, result_size, result_drop)
    })
}

/// `spawn(p)` for a promise that is already a heap future (a leaf, a boxed compiled future, another
/// join handle): ownership of `f` moves to the task. Returns a join handle like `velt_rt_spawn`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_spawn_fut(
    f: *mut VeltFut,
    result_size: u64,
    result_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    spawn_sized(
        FutBody {
            fut: SendPtr(f),
            locals: Locals::default(),
        },
        result_size,
        result_drop,
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
        spawn_body::<_, 0>(
            Compiled::<S>::copy_from(poll, drop, state, size, align),
            0,
            None,
        );
    })
}
