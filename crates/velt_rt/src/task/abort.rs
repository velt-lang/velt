//! The runtime's own abort signal: what `velt dev`'s graceful stop hands a server that joined it
//! (dev/shutdown.rs; std/dev/stop.velt waits for it and aborts the server's `AbortSignal`), and
//! what a runtime wait that takes a signal races (net/views.rs). `AbortController` and
//! `AbortSignal` themselves are std's (std/task/signal_core.velt). A signal is a
//! reference-counted object: a flag, the abort reason and the tasks waiting for it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use super::leaf::{new_inner_leaf, new_leaf};
use super::VeltFut;
use crate::handle::Handle;
use crate::str::VeltStr;

/// Why a signal was aborted.
#[derive(Default, Clone)]
struct Reason {
    text: String,
}

/// An abort signal.
#[derive(Default)]
pub struct Signal {
    aborted: AtomicBool,
    reason: Mutex<Reason>,
    notify: Notify,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Signal {
    /// Aborts with reason `text` (for the runtime's own signals: a no-op if aborted already).
    pub(crate) fn abort_with(&self, text: &str) {
        self.abort(&Reason {
            text: text.to_string(),
        });
    }

    fn abort(&self, reason: &Reason) {
        crate::io::publish_before_handoff();
        {
            let mut r = lock(&self.reason);
            if self.aborted.load(Ordering::Acquire) {
                return;
            }
            *r = reason.clone();
            self.aborted.store(true, Ordering::Release);
        }
        self.notify.notify_waiters();
    }

    fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::Acquire)
    }

    fn reason(&self) -> Reason {
        lock(&self.reason).clone()
    }
}

impl Signal {
    /// Completes once the signal is aborted (never, if it never is): what runtime operations
    /// that take a signal (`fetch`) race their work against.
    pub(crate) async fn aborted(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_aborted() {
                return;
            }
            notified.await;
        }
    }
}

/// The signal behind handle bits `h` (0 = none), as a new reference.
///
/// # Safety
/// A nonzero `h` must be a live signal handle.
pub(crate) unsafe fn signal_of(h: u64) -> Option<Arc<Signal>> {
    let h = Handle::<Signal>::from_ptr(h as usize as *const Signal);
    (!h.is_null()).then(|| h.clone_arc())
}

/// Releases a handle.
///
/// # Safety
/// `h` must be a live signal handle, not used afterwards.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_signal_free(h: Handle<Signal>) {
    h.release();
}

/// The abort reason ("" while not aborted).
///
/// # Safety
/// `h` must be a live signal handle; `out` receives an owned string.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_signal_reason(h: Handle<Signal>, out: *mut VeltStr) {
    out.write(VeltStr::from_vec(h.obj().reason().text.into_bytes()));
}

/// A future (unit result) that completes once the signal is aborted (never, if it never is).
/// `timed`: a timer may abort it (std/task/signal.velt: an `AbortSignal.timeout`, or an
/// `AbortSignal.any` of one), so the wait keeps the process alive, like waiting for a timer
/// (task/quiet.rs; Node unrefs that timer, but then nothing awaiting the signal would run); a
/// signal only `abort()` can abort leaves that to whoever may call it.
///
/// # Safety
/// `h` must be a live signal handle.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_signal_wait(
    h: Handle<Signal>,
    timed: bool,
) -> *mut VeltFut {
    let s = h.clone_arc();
    let wait = async move {
        loop {
            let aborted = s.notify.notified();
            if s.is_aborted() {
                return;
            }
            aborted.await;
        }
    };
    if timed {
        new_leaf(wait)
    } else {
        new_inner_leaf(wait)
    }
}
