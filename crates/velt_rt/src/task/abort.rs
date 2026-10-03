//! `velt_rt_signal_*`: the runtime half of `AbortController` / `AbortSignal` (std/task.vlt,
//! docs/std/task.md). A signal is a reference-counted object: a flag, the abort reason, the tasks
//! waiting for it, and the signals derived from it (`AbortSignal.any`). Cancellation is
//! cooperative: aborting only sets the flag and wakes the waiters; Velt code reacts. No code
//! pointers are stored (rt_abi_async.md §13.5): `AbortSignal.timeout` is a runtime timer task.
//!
//! A derived signal holds strong references to its sources until it is aborted (a source holds
//! only a weak one back), so `AbortSignal.any([AbortSignal.timeout(50)])` keeps its timeout
//! signal alive, and no cycle forms.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::sync::Notify;

use super::leaf::new_leaf;
use super::VeltFut;
use crate::array::VeltArray;
use crate::handle::Handle;
use crate::str::VeltStr;

/// Why a signal was aborted.
#[derive(Default, Clone)]
struct Reason {
    text: String,
    /// The delay of the `AbortSignal.timeout` that aborted it (also through `any`).
    timeout_ms: Option<i64>,
}

/// An abort signal.
#[derive(Default)]
pub struct Signal {
    aborted: AtomicBool,
    reason: Mutex<Reason>,
    notify: Notify,
    /// Signals that abort with this one (`AbortSignal.any`).
    children: Mutex<Vec<Weak<Signal>>>,
    /// The signals this one was derived from (`AbortSignal.any`), until it is aborted.
    sources: Mutex<Vec<Arc<Signal>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Signal {
    fn abort(&self, reason: &Reason) {
        {
            let mut r = lock(&self.reason);
            if self.aborted.load(Ordering::Acquire) {
                return;
            }
            *r = reason.clone();
            self.aborted.store(true, Ordering::Release);
        }
        crate::io::publish_before_handoff();
        self.notify.notify_waiters();
        drop(std::mem::take(&mut *lock(&self.sources)));
        let children = std::mem::take(&mut *lock(&self.children));
        for c in children.iter().filter_map(Weak::upgrade) {
            c.abort(reason);
        }
    }

    fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::Acquire)
    }

    fn reason(&self) -> Reason {
        lock(&self.reason).clone()
    }

    /// Abort `child` with this signal (at once if this one is already aborted).
    fn link(self: &Arc<Self>, child: &Arc<Signal>) {
        let mut cs = lock(&self.children);
        if self.is_aborted() {
            drop(cs);
            child.abort(&self.reason());
            return;
        }
        cs.retain(|w| w.strong_count() > 0);
        cs.push(Arc::downgrade(child));
        drop(cs);
        lock(&child.sources).push(self.clone());
    }
}

/// A new signal that is not aborted.
#[no_mangle]
pub extern "C" fn velt_rt_signal_new() -> Handle<Signal> {
    Handle::from_arc(Arc::new(Signal::default()))
}

/// Another reference to the same signal (for a holder that releases it on its own).
///
/// # Safety
/// `h` must be a live signal handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_retain(h: Handle<Signal>) -> Handle<Signal> {
    Handle::from_arc(h.clone_arc())
}

/// Releases a handle.
///
/// # Safety
/// `h` must be a live signal handle, not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_free(h: Handle<Signal>) {
    h.release();
}

/// Aborts the signal with `reason` (a no-op if it is aborted already) and every signal derived
/// from it; wakes the tasks waiting for it.
///
/// # Safety
/// `h` must be a live signal handle; `reason` a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_abort(h: Handle<Signal>, reason: *const VeltStr) {
    let text = String::from_utf8_lossy((*reason).as_bytes()).into_owned();
    h.obj().abort(&Reason {
        text,
        timeout_ms: None,
    });
}

/// Whether the signal is aborted.
///
/// # Safety
/// `h` must be a live signal handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_aborted(h: Handle<Signal>) -> bool {
    h.obj().is_aborted()
}

/// The abort reason ("" while not aborted).
///
/// # Safety
/// `h` must be a live signal handle; `out` receives an owned string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_reason(h: Handle<Signal>, out: *mut VeltStr) {
    out.write(VeltStr::from_vec(h.obj().reason().text.into_bytes()));
}

/// The delay of the `AbortSignal.timeout` that aborted the signal (directly or through
/// `AbortSignal.any`), or -1 (not aborted, or aborted otherwise).
///
/// # Safety
/// `h` must be a live signal handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_timeout_ms(h: Handle<Signal>) -> i64 {
    h.obj().reason().timeout_ms.unwrap_or(-1)
}

/// A future (unit result) that completes once the signal is aborted (never, if it never is).
///
/// # Safety
/// `h` must be a live signal handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_wait(h: Handle<Signal>) -> *mut VeltFut {
    let s = h.clone_arc();
    new_leaf(async move {
        loop {
            let aborted = s.notify.notified();
            if s.is_aborted() {
                return;
            }
            aborted.await;
        }
    })
}

/// `AbortSignal.timeout(ms)`: a new signal that a runtime timer aborts after `ms` milliseconds
/// with `reason`. The timer holds only a weak reference: a signal nobody holds is not kept
/// alive by its timer.
///
/// # Safety
/// `reason` must be a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_timeout(ms: i64, reason: *const VeltStr) -> Handle<Signal> {
    let s = Arc::new(Signal::default());
    let weak = Arc::downgrade(&s);
    let reason = Reason {
        text: String::from_utf8_lossy((*reason).as_bytes()).into_owned(),
        timeout_ms: Some(ms),
    };
    let delay = Duration::from_millis(ms.max(0) as u64);
    super::runtime::handle().spawn(async move {
        tokio::time::sleep(delay).await;
        if let Some(s) = weak.upgrade() {
            s.abort(&reason);
        }
    });
    Handle::from_arc(s)
}

/// `AbortSignal.any(signals)`: a new signal aborted as soon as any of the signals in the
/// borrowed `u64[]` is (at once if one already is), with that signal's reason. It keeps the
/// signals alive until then.
///
/// # Safety
/// `signals` must be a valid array of live signal handles.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_any(signals: *const VeltArray<u64>) -> Handle<Signal> {
    let child = Arc::new(Signal::default());
    let a = &*signals;
    for i in 0..a.len as usize {
        Handle::<Signal>::from_ptr(*a.ptr.add(i) as usize as *const Signal)
            .clone_arc()
            .link(&child);
    }
    Handle::from_arc(child)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, PENDING, READY};
    use std::task::{Context, Poll, Waker};

    fn poll(f: *mut VeltFut) -> u32 {
        let mut cx = Context::from_waker(Waker::noop());
        // SAFETY: a live leaf future.
        unsafe { velt_rt_fut_poll(f, raw_cx(&mut cx)) }
    }

    #[test]
    fn abort_wakes_waiters_and_derived_signals() {
        let reason = VeltStr::from_vec(b"stop".to_vec());
        // SAFETY: handles are live until freed; futures are owned here.
        unsafe {
            let a = velt_rt_signal_new();
            let b = velt_rt_signal_new();
            let hs = VeltArray::from_vec(vec![a.bits(), b.bits()]);
            let any = velt_rt_signal_any(&hs);
            let wait = velt_rt_signal_wait(any);
            assert_eq!(poll(wait), PENDING);
            velt_rt_signal_abort(b, &reason);
            assert!(velt_rt_signal_aborted(any));
            assert!(!velt_rt_signal_aborted(a));
            assert_eq!(poll(wait), READY);
            assert_eq!(h_reason(any), "stop");
            assert_eq!(velt_rt_signal_timeout_ms(any), -1);
            velt_rt_fut_drop(wait);
            for h in [a, b, any] {
                velt_rt_signal_free(h);
            }
        }
    }

    unsafe fn h_reason(h: Handle<Signal>) -> String {
        h.obj().reason().text
    }

    #[test]
    fn a_derived_signal_keeps_its_sources_alive() {
        let reason = VeltStr::from_vec(b"late".to_vec());
        crate::task::runtime::runtime().block_on(async {
            // SAFETY: handles are live until freed.
            unsafe {
                let t = velt_rt_signal_timeout(5, &reason);
                let hs = VeltArray::from_vec(vec![t.bits()]);
                let any = velt_rt_signal_any(&hs);
                velt_rt_signal_free(t);
                // Wait for the derived signal itself: it aborts only if it kept the freed timeout
                // source alive. The limit only turns a hang (a source dropped too early) into a
                // failure; it is not a timing window.
                let wait = velt_rt_signal_wait(any);
                let done = std::future::poll_fn(|cx| match velt_rt_fut_poll(wait, raw_cx(cx)) {
                    READY => Poll::Ready(()),
                    _ => Poll::Pending,
                });
                let waited = tokio::time::timeout(Duration::from_secs(60), done).await;
                velt_rt_fut_drop(wait);
                assert!(waited.is_ok(), "the timeout source never fired");
                assert!(velt_rt_signal_aborted(any));
                assert_eq!(h_reason(any), "late");
                assert_eq!(velt_rt_signal_timeout_ms(any), 5);
                velt_rt_signal_free(any);
            }
        });
    }
}
