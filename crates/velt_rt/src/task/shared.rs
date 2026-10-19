//! Shared promises (docs/reference/async.md "Promises", #212): a promise that several places
//! hold, each of which may await it and gets the result, as in JS.
//!
//! Every holder holds the same `VeltFut*`. The first share (`velt_rt_fut_share`) turns the
//! future into a shared one in place: its header's `poll` / `drop` become [`shared_poll`] /
//! [`shared_drop`], and the original entries move to a side record (the hub, found by the
//! future's address). The hub counts the holders, keeps the settled result once the original
//! future finished, and lists the holders waiting for it, in the order they first waited.
//! So awaiting, the combinators and `then` keep their code: a holder that sees `READY` moves
//! the result out of the slot at +16 as usual, and every delivery writes a fresh share of the
//! settled result there first (the same object for an object result, a copy of a string or a
//! value).
//!
//! The first waiter drives the original future with its own context, so a lone waiter resumes
//! as it would without sharing. When the original settles, every waiter is queued in the task's
//! microtask queue in the order it started waiting (`local::settled`, as a `resolve` queues the
//! waiters of a latch): JS queues a promise's reactions in that order.
//!
//! Dropping a holder never cancels the promise (JS never does). The last one releases the
//! settled result, or, when the promise has not settled, gives the original future back its
//! entries and drops it: it then runs to completion and its own result drop disposes of the
//! result (an unhandled rejection is reported) as for any dropped promise. A holder dropped
//! while others wait wakes them, so none waits for a driver that went away.
//!
//! A shared promise stays on the task that awaits it (the hub is not synchronized for awaits
//! from several tasks at once; sema keeps promises from crossing tasks shared).

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::task::Waker;

use parking_lot::Mutex;

use super::all::ResultDropFn;
use super::{context, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};

/// Compiled share glue of a result slot: writes another reference to `*src` into `dst`.
pub type ShareFn = unsafe extern "C" fn(src: *const u8, dst: *mut u8);

type FutPoll = unsafe extern "C" fn(f: *mut VeltFut, cx: *mut c_void) -> u32;
type FutDrop = unsafe extern "C" fn(f: *mut VeltFut);

/// The side record of a shared future.
struct Hub {
    poll: FutPoll,
    drop: FutDrop,
    size: usize,
    share: Option<ShareFn>,
    result_drop: Option<ResultDropFn>,
    st: Mutex<HubState>,
}

struct HubState {
    holders: usize,
    done: bool,
    /// The settled result (16-byte aligned words), once `done`.
    result: Vec<u128>,
    /// The waiting holders in the order they first waited, with the microtask ticket each was
    /// queued as when the promise settled (0: not queued).
    waiters: Vec<(Waker, u64)>,
}

/// The hubs of the shared futures, by address.
static HUBS: Mutex<Option<HashMap<usize, Arc<Hub>>>> = Mutex::new(None);

fn hub(f: *mut VeltFut) -> Arc<Hub> {
    match HUBS.lock().as_ref().and_then(|m| m.get(&(f as usize))) {
        Some(h) => h.clone(),
        None => crate::panic::fatal("internal error: a shared promise lost its record"),
    }
}

/// Is `f` a shared future?
///
/// # Safety
/// `f` must be a live heap future.
pub(crate) unsafe fn is_shared(f: *mut VeltFut) -> bool {
    std::ptr::fn_addr_eq((*f).poll, shared_poll as FutPoll)
}

/// Another holder of the promise `f` (null: none): `f` itself, now shared (rt_abi_async.md §1.2).
/// `result_size` is the size of its result slot, `share` the glue that writes another reference
/// to a result (null: the bytes are copied), `result_drop` the glue that drops one (null:
/// nothing to drop).
///
/// # Safety
/// `f` must be null or a live heap future owned by the caller; the glue must match its result.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_share(
    f: *mut VeltFut,
    result_size: u64,
    share: Option<ShareFn>,
    result_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    if f.is_null() {
        return f;
    }
    if is_shared(f) {
        hub(f).st.lock().holders += 1;
        return f;
    }
    let h = Arc::new(Hub {
        poll: (*f).poll,
        drop: (*f).drop,
        size: result_size as usize,
        share,
        result_drop,
        st: Mutex::new(HubState {
            holders: 2,
            done: false,
            result: Vec::new(),
            waiters: Vec::new(),
        }),
    });
    HUBS.lock()
        .get_or_insert_with(HashMap::new)
        .insert(f as usize, h);
    (*f).poll = shared_poll;
    (*f).drop = shared_drop;
    f
}

unsafe fn slot(f: *mut VeltFut) -> *mut u8 {
    (f as *mut u8).add(FUT_RESULT_OFFSET)
}

/// Write another reference to the settled result into the slot of `f`.
unsafe fn deliver(f: *mut VeltFut, h: &Hub, result: *const u8) {
    match h.share {
        Some(share) => share(result, slot(f)),
        None => std::ptr::copy_nonoverlapping(result, slot(f), h.size),
    }
}

unsafe extern "C" fn shared_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let h = hub(f);
    let w = &context(cx).waker().clone();
    let mut st = h.st.lock();
    if !st.done {
        let first = match st.waiters.iter().position(|(x, _)| x.will_wake(w)) {
            Some(i) => i == 0,
            None => {
                st.waiters.push((w.clone(), 0));
                st.waiters.len() == 1
            }
        };
        if !first {
            return PENDING;
        }
        drop(st);
        // The first waiter drives the original future (no lock held: it runs compiled code).
        if (h.poll)(f, cx) != READY {
            return PENDING;
        }
        st = h.st.lock();
        let mut result = vec![0u128; h.size.div_ceil(16)];
        std::ptr::copy_nonoverlapping(slot(f), result.as_mut_ptr() as *mut u8, h.size);
        st.result = result;
        st.done = true;
        // Every waiter, this one included, resumes in the order it started waiting.
        let waiters = std::mem::take(&mut st.waiters);
        drop(st);
        let queued: Vec<(Waker, u64)> = waiters
            .into_iter()
            .map(|(k, _)| (k.clone(), super::local::settled(k)))
            .filter(|(_, t)| *t != 0)
            .collect();
        st = h.st.lock();
        st.waiters = queued;
    }
    if let Some(i) = st.waiters.iter().position(|(x, _)| x.will_wake(w)) {
        if super::local::ticket_pending(st.waiters[i].1, w) {
            return PENDING;
        }
        st.waiters.remove(i);
    }
    let result = st.result.as_ptr() as *const u8;
    drop(st);
    deliver(f, &h, result);
    READY
}

unsafe extern "C" fn shared_drop(f: *mut VeltFut) {
    let h = hub(f);
    let mut st = h.st.lock();
    st.holders -= 1;
    if st.holders > 0 {
        // A waiter may have been the one driving the promise: let the others register again.
        let waiters = if st.done {
            vec![]
        } else {
            std::mem::take(&mut st.waiters)
        };
        drop(st);
        for (w, _) in waiters {
            w.wake();
        }
        return;
    }
    let done = st.done;
    let mut result = std::mem::take(&mut st.result);
    drop(st);
    if let Some(m) = HUBS.lock().as_mut() {
        m.remove(&(f as usize));
    }
    if done {
        if let Some(d) = h.result_drop {
            d(result.as_mut_ptr() as *mut u8);
        }
    }
    (*f).poll = h.poll;
    (*f).drop = h.drop;
    (h.drop)(f);
}

/// `velt_rt_fut_peek` of a shared future: its settled result, copied into the slot for the
/// formatter to read (it claims nothing).
///
/// # Safety
/// `f` must be a live shared future.
pub(crate) unsafe fn peek(f: *mut VeltFut) -> bool {
    let h = hub(f);
    let st = h.st.lock();
    if st.done {
        std::ptr::copy_nonoverlapping(st.result.as_ptr() as *const u8, slot(f), h.size);
    }
    st.done
}
