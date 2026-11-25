//! `velt_rt_signal_*`: the runtime half of `AbortController` / `AbortSignal` (std/task/signal.velt,
//! https://veltlang.com/docs/std/task/). A signal is a reference-counted object: a flag, the abort reason, the tasks
//! waiting for it, and the signals derived from it (`AbortSignal.any`). Cancellation is
//! cooperative: aborting only sets the flag and wakes the waiters; Velt code reacts. No code
//! pointers are stored (rt_abi_async.md §13.5). `AbortSignal.timeout`'s timer is std's, one of
//! Node's timers (std/task/signal_timeout.velt, #3029), which aborts the signal through
//! `velt_rt_signal_abort`; std tells a wait for such a signal that a timer may abort it
//! (`velt_rt_signal_wait`'s `timed`).
//!
//! A derived signal holds strong references to its sources until it is aborted (a source holds
//! only a weak one back), so a source aborted through another reference aborts it, and no cycle
//! forms.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};

use super::leaf::{new_inner_leaf, new_leaf};
use super::VeltFut;
use crate::array::VeltArray;
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
    /// The tasks waiting for the abort ([`AbortWait`]), by the id their wait took.
    waiters: Mutex<(u64, Vec<(u64, Waker)>)>,
    /// Signals that abort with this one (`AbortSignal.any`).
    children: Mutex<Vec<Weak<Signal>>>,
    /// The signals this one was derived from (`AbortSignal.any`), until it is aborted.
    sources: Mutex<Vec<Arc<Signal>>>,
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
        let waiters = std::mem::take(&mut lock(&self.waiters).1);
        waiters.into_iter().for_each(|(_, w)| w.wake());
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
    pub(crate) fn aborted(&self) -> AbortWait<'_> {
        AbortWait { s: self, id: None }
    }
}

/// [`Signal::aborted`]: registered with the signal while it waits, and removed when dropped, so
/// waits on a signal that is never aborted leave nothing behind.
pub(crate) struct AbortWait<'a> {
    s: &'a Signal,
    id: Option<u64>,
}

impl Future for AbortWait<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut w = lock(&self.s.waiters);
        // Checked under the lock `abort` takes the waiters with: no wake is lost.
        if self.s.is_aborted() {
            return Poll::Ready(());
        }
        let (next, list) = &mut *w;
        match self.id.and_then(|id| list.iter_mut().find(|(i, _)| *i == id)) {
            Some((_, waker)) => waker.clone_from(cx.waker()),
            None => {
                *next += 1;
                list.push((*next, cx.waker().clone()));
                let id = *next;
                drop(w);
                self.id = Some(id);
            }
        }
        Poll::Pending
    }
}

impl Drop for AbortWait<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            lock(&self.s.waiters).1.retain(|(i, _)| *i != id);
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

/// A new signal that is not aborted.
#[no_mangle]
pub extern "C-unwind" fn velt_rt_signal_new() -> Handle<Signal> {
    Handle::from_arc(Arc::new(Signal::default()))
}

/// Another reference to the same signal (for a holder that releases it on its own).
///
/// # Safety
/// `h` must be a live signal handle.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_signal_retain(h: Handle<Signal>) -> Handle<Signal> {
    Handle::from_arc(h.clone_arc())
}

/// Releases a handle.
///
/// # Safety
/// `h` must be a live signal handle, not used afterwards.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_signal_free(h: Handle<Signal>) {
    h.release();
}

/// Aborts the signal with `reason` (a no-op if it is aborted already) and every signal derived
/// from it; wakes the tasks waiting for it.
///
/// # Safety
/// `h` must be a live signal handle; `reason` a valid string.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_signal_abort(h: Handle<Signal>, reason: *const VeltStr) {
    let text = (*reason).text_lossy().into_owned();
    h.obj().abort(&Reason { text });
}

/// Whether the signal is aborted.
///
/// # Safety
/// `h` must be a live signal handle.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_signal_aborted(h: Handle<Signal>) -> bool {
    h.obj().is_aborted()
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
    let wait = async move { s.aborted().await };
    if timed {
        new_leaf(wait)
    } else {
        new_inner_leaf(wait)
    }
}

/// `AbortSignal.any(signals)`: a new signal aborted as soon as any of the signals in the
/// borrowed `u64[]` is (at once if one already is), with that signal's reason. It keeps the
/// signals alive until then.
///
/// # Safety
/// `signals` must be a valid array of live signal handles.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_signal_any(
    signals: *const VeltArray<u64>,
) -> Handle<Signal> {
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
    use crate::task::{drop_fut, poll_fut, raw_cx, PENDING, READY};
    use std::task::{Context, Waker};

    fn poll(f: *mut VeltFut) -> u32 {
        let mut cx = Context::from_waker(Waker::noop());
        // SAFETY: a live leaf future.
        unsafe { poll_fut(f, raw_cx(&mut cx).ptr()) }
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
            let wait = velt_rt_signal_wait(any, false);
            assert_eq!(poll(wait), PENDING);
            velt_rt_signal_abort(b, &reason);
            assert!(velt_rt_signal_aborted(any));
            assert!(!velt_rt_signal_aborted(a));
            assert_eq!(poll(wait), READY);
            assert_eq!(h_reason(any), "stop");
            drop_fut(wait);
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
        // SAFETY: handles are live until freed.
        unsafe {
            let t = velt_rt_signal_new();
            let other = velt_rt_signal_retain(t);
            let hs = VeltArray::from_vec(vec![t.bits()]);
            let any = velt_rt_signal_any(&hs);
            // The derived signal's reference keeps the source linked after this handle goes:
            // aborting it through the other one aborts the derived signal.
            velt_rt_signal_free(t);
            velt_rt_signal_abort(other, &reason);
            assert!(velt_rt_signal_aborted(any));
            assert_eq!(h_reason(any), "late");
            velt_rt_signal_free(other);
            velt_rt_signal_free(any);
        }
    }
}
