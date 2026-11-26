//! Shared "fake compiler" building blocks: the state machines generated code would contain for
//! the simplest async functions, written by hand against the C ABI.

use crate::task::runtime::{block_on, spawn, JoinHandle};
use crate::task::{raw_cx, PollFn, SendPtr, VeltFut, PENDING};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A compiled state `(poll, state)` run as a Rust future: each poll of the future polls the state
/// once, with the task's waker. The state is borrowed: it is never dropped here.
struct State {
    poll: PollFn,
    state: SendPtr<u8>,
}

impl Future for State {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // SAFETY: the caller of `spawn_state` keeps the state alive until the task finished.
        match unsafe { (self.poll)(self.state.0, raw_cx(cx).ptr()) } {
            PENDING => Poll::Pending,
            _ => Poll::Ready(()),
        }
    }
}

/// Start the compiled future `(poll, state)` as a task on the runtime's host, as `async main`
/// started before programs ran on the executor in Velt. `state` stays owned by the caller and
/// must outlive the task; its result is at offset 0 once the returned handle is joined.
pub unsafe fn spawn_state(poll: PollFn, state: *mut u8) -> JoinHandle<()> {
    spawn(State {
        poll,
        state: SendPtr(state),
    })
}

/// A leaf that is pending once (waking its task) and ready on the next poll, with an empty
/// result (what a stored `yieldNow()` was before the executor in Velt took it over).
pub fn yield_once() -> *mut VeltFut {
    crate::task::leaf::new_inner_leaf(tokio::task::yield_now())
}

/// Wait for a task [`spawn_state`] started (its panic goes on here).
pub fn join(h: JoinHandle<()>) {
    match block_on(h) {
        Ok(()) => {}
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("task did not finish: {e}"),
    }
}

/// Run the compiled future `(poll, state)` to completion as a task ([`spawn_state`]).
pub unsafe fn block_on_state(poll: PollFn, state: *mut u8) {
    join(spawn_state(poll, state));
}
