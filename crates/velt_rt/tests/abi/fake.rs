//! Shared "fake compiler" building blocks: the state machines generated code would contain for
//! the simplest async functions, written by hand against the C ABI.

use crate::task::{raw_cx, PollFn, VeltFut, PENDING};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::Thread;

/// Wakes the thread that runs [`block_on_state`].
struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// A leaf that is pending once (waking its task) and ready on the next poll, with an empty
/// result (what a stored `yieldNow()` was before the executor in Velt took it over).
pub fn yield_once() -> *mut VeltFut {
    crate::task::leaf::new_inner_leaf(YieldOnce(false))
}

/// Pending on its first poll, after waking its task; ready on the next.
struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            return Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// Run the compiled future `(poll, state)` to completion on this thread: poll the state with a
/// waker that unparks the thread, park while it is pending. `state` stays owned by the caller;
/// its result is at offset 0 afterwards.
pub unsafe fn block_on_state(poll: PollFn, state: *mut u8) {
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    // SAFETY: the caller keeps the state alive until this returns.
    while unsafe { poll(state, raw_cx(&mut cx).ptr()) } == PENDING {
        std::thread::park();
    }
}
