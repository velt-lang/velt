//! `velt_rt_signal_*`: the runtime half of `AbortController` / `AbortSignal` (std/task.vlt,
//! docs/std/task.md). A signal is a reference-counted object: a flag, the abort reason, the tasks
//! waiting for it, and the signals derived from it (`AbortSignal.any`). Cancellation is
//! cooperative: aborting only sets the flag and wakes the waiters; Velt code reacts. No code
//! pointers are stored (rt_abi_async.md §13.5): `AbortSignal.timeout` is a runtime timer task,
//! which holds only a weak reference to its signal and is aborted when the signal is dropped, so
//! a dropped signal with a long timeout frees its memory and its timer at once (#149).
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
    /// The timer task of an `AbortSignal.timeout` signal, aborted when the signal is dropped.
    timer: std::sync::OnceLock<tokio::task::AbortHandle>,
}

impl Drop for Signal {
    fn drop(&mut self) {
        if let Some(t) = self.timer.get() {
            t.abort();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Signal {
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

/// Abort `s` with no reason (tests of the operations that take a signal).
#[cfg(test)]
pub(crate) fn abort_for_test(s: &Signal) {
    s.abort(&Reason::default());
}

/// The signal behind handle bits `h` (0 = none), as a new reference.
///
/// # Safety
/// A nonzero `h` must be a live signal handle.
pub(crate) unsafe fn signal_of(h: u64) -> Option<Arc<Signal>> {
    let h = Handle::<Signal>::from_ptr(h as usize as *const Signal);
    (!h.is_null()).then(|| h.clone_arc())
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
    let text = (*reason).text_lossy().into_owned();
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
/// with `reason`. The timer holds only a weak reference, and dropping the signal aborts the
/// timer: a signal nobody holds is freed at once, with its timer, not when the delay is up.
///
/// # Safety
/// `reason` must be a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_timeout(ms: i64, reason: *const VeltStr) -> Handle<Signal> {
    let s = Arc::new(Signal::default());
    let weak = Arc::downgrade(&s);
    let reason = Reason {
        text: (*reason).text_lossy().into_owned(),
        timeout_ms: Some(ms),
    };
    let delay = Duration::from_millis(ms.max(0) as u64);
    let live = live_timers::Live::new();
    let task = super::runtime::handle().spawn(async move {
        let _live = live;
        tokio::time::sleep(delay).await;
        if let Some(s) = weak.upgrade() {
            s.abort(&reason);
        }
    });
    // The task never outlives the signal by more than its abort: it holds nothing else.
    let _ = s.timer.set(task.abort_handle());
    Handle::from_arc(s)
}

/// Live `AbortSignal.timeout` timer tasks, for the cost test (a count, not a time).
mod live_timers {
    #[cfg(test)]
    pub(super) static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// Counts one live timer task while it exists (nothing outside tests).
    pub(super) struct Live;

    impl Live {
        pub(super) fn new() -> Live {
            #[cfg(test)]
            LIVE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Live
        }
    }

    impl Drop for Live {
        fn drop(&mut self) {
            #[cfg(test)]
            LIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
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
    fn abort_publishes_this_threads_output_first() {
        use crate::io::handoff_probe::{buffer_output, published};
        let reason = VeltStr::from_vec(b"stop".to_vec());
        // SAFETY: the handle is live until freed.
        unsafe {
            let s = velt_rt_signal_new();
            buffer_output();
            velt_rt_signal_abort(s, &reason);
            assert!(published());
            velt_rt_signal_free(s);
        }
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
    fn dropped_timeout_signals_free_their_timers() {
        use std::sync::atomic::Ordering::SeqCst;
        let reason = VeltStr::from_vec(b"late".to_vec());
        crate::task::runtime::runtime().block_on(async {
            const N: usize = 100_000;
            let before = live_timers::LIVE.load(SeqCst);
            for _ in 0..N {
                // SAFETY: a fresh handle, freed at once (the last reference: the signal drops).
                unsafe { velt_rt_signal_free(velt_rt_signal_timeout(600_000, &reason)) };
            }
            // Aborted tasks are dropped on their next turn: wait for that (the limit only turns
            // a leak into a failure instead of a hang; it is not a timing window).
            let t = std::time::Instant::now();
            // Other tests may hold a few timeouts of their own meanwhile.
            while live_timers::LIVE.load(SeqCst) > before + 100 {
                assert!(
                    t.elapsed() < Duration::from_secs(60),
                    "{} of {N} dropped timeouts still hold their timer",
                    live_timers::LIVE.load(SeqCst) - before
                );
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
    }

    #[test]
    fn a_held_timeout_signal_still_aborts() {
        let reason = VeltStr::from_vec(b"late".to_vec());
        crate::task::runtime::runtime().block_on(async {
            // SAFETY: handles are live until freed.
            unsafe {
                let t = velt_rt_signal_timeout(5, &reason);
                let wait = velt_rt_signal_wait(t);
                // A waiter holds the signal too: freeing the handle keeps the timer.
                velt_rt_signal_free(t);
                let done = std::future::poll_fn(|cx| match velt_rt_fut_poll(wait, raw_cx(cx)) {
                    READY => Poll::Ready(()),
                    _ => Poll::Pending,
                });
                let waited = tokio::time::timeout(Duration::from_secs(60), done).await;
                velt_rt_fut_drop(wait);
                assert!(waited.is_ok(), "the timeout never fired");
            }
        });
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
