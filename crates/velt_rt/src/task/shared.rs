//! Shared promises (docs/reference/async.md "Promises", #212): a promise that several places
//! hold, each of which may await it and gets the result, as in JS.
//!
//! The first share (`velt_rt_fut_share`) turns the future into a shared one in place: its
//! header's `poll` / `drop` become [`orig_poll`] / [`orig_drop`], and the original entries move
//! to a side record (the hub, found by the future's address). The original pointer stays its
//! first holder's; every other holder gets a handle of its own (a small heap future with a
//! result slot, the hub in front of its header), so each holder awaits, and is dropped, as one
//! future. The hub counts the holders, keeps the settled result once the original future
//! finished, and lists the waiting holders in the order they started waiting. Awaiting, the
//! combinators and `then` keep their code: a holder that sees `READY` moves the result out of
//! its slot at +16, where the hub wrote a fresh share of the settled result (the same object
//! for an object result, a copy of a string or a value).
//!
//! The first waiter drives the original future with its own context. When the original settles
//! there, that waiter goes on as an unshared promise's awaiter would, and the others are queued
//! in the task's microtask queue in the order they started waiting (`local::settled`, as a
//! `resolve` queues the waiters of a latch): JS queues a promise's reactions in that order.
//!
//! Dropping a holder never cancels the promise (JS never does). A waiting holder that is dropped
//! wakes the others, so none waits for a driver that went away. The last holder releases the
//! settled result, or, when the promise has not settled, gives the original future back its
//! entries and drops it: it then runs to completion and its own result drop disposes of the
//! result (an unhandled rejection is reported) as for any dropped promise.
//!
//! A shared promise stays on the task that awaits it: the hub is locked for each step, but
//! awaits from several tasks at once are not supported yet.

use std::alloc::Layout;
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
    /// The original future (its first holder's pointer), and its own entries.
    orig: *mut VeltFut,
    poll: FutPoll,
    drop: FutDrop,
    size: usize,
    share: Option<ShareFn>,
    result_drop: Option<ResultDropFn>,
    st: Mutex<HubState>,
}

// SAFETY: a hub is reached from the task that awaits its promise (module docs); `orig` is the
// future it belongs to, and the state is behind a lock.
unsafe impl Send for Hub {}
// SAFETY: as above.
unsafe impl Sync for Hub {}

struct HubState {
    holders: usize,
    done: bool,
    /// The settled result (16-byte aligned words), once `done`.
    result: Vec<u128>,
    /// The waiting holders (by address) in the order they started waiting, with their wakers
    /// and the microtask ticket each was queued as when the promise settled (0: not queued).
    waiters: Vec<(usize, Waker, u64)>,
}

/// The hubs of the shared original futures, by address.
static HUBS: Mutex<Option<HashMap<usize, Arc<Hub>>>> = Mutex::new(None);

/// Bytes in front of a handle's header: its hub.
const FRONT: usize = 16;

fn handle_layout(size: usize) -> Layout {
    Layout::from_size_align(FRONT + FUT_RESULT_OFFSET + size, 16)
        .unwrap_or_else(|_| crate::panic::fatal("invalid shared promise size"))
}

/// The hub of holder `f` (the original future or a handle).
unsafe fn hub(f: *mut VeltFut) -> Arc<Hub> {
    if is_handle(f) {
        handle_hub(f)
    } else {
        orig_hub(f)
    }
}

/// The hub of handle `f`, kept in front of its header.
unsafe fn handle_hub(f: *mut VeltFut) -> Arc<Hub> {
    (*((f as *mut u8).sub(FRONT) as *const Arc<Hub>)).clone()
}

/// The hub of the original future `f`, by its address.
unsafe fn orig_hub(f: *mut VeltFut) -> Arc<Hub> {
    match HUBS.lock().as_ref().and_then(|m| m.get(&(f as usize))) {
        Some(h) => h.clone(),
        None => crate::panic::fatal("internal error: a shared promise lost its record"),
    }
}

unsafe fn is_handle(f: *mut VeltFut) -> bool {
    std::ptr::fn_addr_eq((*f).poll, handle_poll as FutPoll)
}

/// Is `f` a holder of a shared promise (its original future or a handle)?
///
/// # Safety
/// `f` must be a live heap future.
pub(crate) unsafe fn is_shared(f: *mut VeltFut) -> bool {
    is_handle(f) || std::ptr::fn_addr_eq((*f).poll, orig_poll as FutPoll)
}

/// Another holder of the promise `f` (null: none): a handle of its own (rt_abi_async.md §1.2).
/// `result_size` is the size of the result slot, `share` the glue that writes another reference
/// to a result (null: the bytes are copied), `result_drop` the glue that drops one (null:
/// nothing to drop).
///
/// # Safety
/// `f` must be null or a live heap future held by the caller; the glue must match its result.
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
    let h = if is_shared(f) {
        hub(f)
    } else {
        let h = Arc::new(Hub {
            orig: f,
            poll: (*f).poll,
            drop: (*f).drop,
            size: result_size as usize,
            share,
            result_drop,
            st: Mutex::new(HubState {
                holders: 1,
                done: false,
                result: Vec::new(),
                waiters: Vec::new(),
            }),
        });
        HUBS.lock()
            .get_or_insert_with(HashMap::new)
            .insert(f as usize, h.clone());
        (*f).poll = orig_poll;
        (*f).drop = orig_drop;
        h
    };
    h.st.lock().holders += 1;
    let l = handle_layout(h.size);
    let base = std::alloc::alloc(l);
    if base.is_null() {
        std::alloc::handle_alloc_error(l);
    }
    (base as *mut Arc<Hub>).write(h);
    let g = base.add(FRONT) as *mut VeltFut;
    g.write(VeltFut {
        poll: handle_poll,
        drop: handle_drop,
    });
    g
}

unsafe fn slot(f: *mut VeltFut) -> *mut u8 {
    (f as *mut u8).add(FUT_RESULT_OFFSET)
}

// `orig_poll` and `handle_poll` tell the original future from a handle by address
// ([`is_handle`]), so their code must differ: the linker folds identical functions into one
// (`/OPT:ICF`), and with the same body both would have one address, every original would read as
// a handle, and its hub would be read from the bytes in front of it.
unsafe extern "C" fn orig_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    poll_holder(&orig_hub(f), f, cx)
}

unsafe extern "C" fn handle_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    poll_holder(&handle_hub(f), f, cx)
}

unsafe extern "C" fn orig_drop(f: *mut VeltFut) {
    release(hub(f), f);
}

unsafe extern "C" fn handle_drop(f: *mut VeltFut) {
    let base = (f as *mut u8).sub(FRONT);
    let h = (base as *mut Arc<Hub>).read();
    let l = handle_layout(h.size);
    release(h, f);
    std::alloc::dealloc(base, l);
}

/// Poll holder `f` of the promise of `h`.
unsafe fn poll_holder(h: &Arc<Hub>, f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let me = f as usize;
    let w = context(cx).waker().clone();
    let mut st = h.st.lock();
    if !st.done {
        let first = match st.waiters.iter().position(|(x, ..)| *x == me) {
            Some(i) => {
                if !st.waiters[i].1.will_wake(&w) {
                    st.waiters[i].1 = w.clone();
                }
                i == 0
            }
            None => {
                st.waiters.push((me, w.clone(), 0));
                st.waiters.len() == 1
            }
        };
        if !first {
            return PENDING;
        }
        drop(st);
        // The first waiter drives the original future (no lock held: it runs compiled code).
        if (h.poll)(h.orig, cx) != READY {
            return PENDING;
        }
        st = h.st.lock();
        let mut result = vec![0u128; h.size.div_ceil(16)];
        std::ptr::copy_nonoverlapping(slot(h.orig), result.as_mut_ptr() as *mut u8, h.size);
        st.result = result;
        st.done = true;
        // The waiters resume in the order they started waiting. This one, the first, goes on
        // now when its `await` would have (as an unshared promise's awaiter does), ahead of the
        // others it queues; otherwise it is queued first.
        let mut waiters = std::mem::take(&mut st.waiters);
        drop(st);
        let now = super::local::step_free();
        if now {
            waiters.remove(0);
        }
        let queued: Vec<(usize, Waker, u64)> = waiters
            .into_iter()
            .map(|(x, k, _)| (x, k.clone(), super::local::settled(k)))
            .filter(|(.., t)| *t != 0)
            .collect();
        if now && !queued.is_empty() {
            super::local::grant_step();
        }
        st = h.st.lock();
        st.waiters = queued;
    }
    if let Some(i) = st.waiters.iter().position(|(x, ..)| *x == me) {
        if super::local::ticket_pending(st.waiters[i].2, &w) {
            return PENDING;
        }
        st.waiters.remove(i);
    }
    let result = st.result.as_ptr() as *const u8;
    drop(st);
    match h.share {
        Some(share) => share(result, slot(f)),
        None => std::ptr::copy_nonoverlapping(result, slot(f), h.size),
    }
    READY
}

/// Holder `f` of the promise of `h` is dropped.
unsafe fn release(h: Arc<Hub>, f: *mut VeltFut) {
    let me = f as usize;
    let mut st = h.st.lock();
    st.holders -= 1;
    if st.holders > 0 {
        let Some(i) = st.waiters.iter().position(|(x, ..)| *x == me) else {
            return;
        };
        st.waiters.remove(i);
        // It may have been driving the promise: the others wait again, the first drives.
        let wake: Vec<Waker> = match st.done {
            true => vec![],
            false => st.waiters.iter().map(|(_, w, _)| w.clone()).collect(),
        };
        drop(st);
        for w in wake {
            w.wake();
        }
        return;
    }
    let done = st.done;
    let mut result = std::mem::take(&mut st.result);
    drop(st);
    if let Some(m) = HUBS.lock().as_mut() {
        m.remove(&(h.orig as usize));
    }
    if done {
        if let Some(d) = h.result_drop {
            d(result.as_mut_ptr() as *mut u8);
        }
    }
    (*h.orig).poll = h.poll;
    (*h.orig).drop = h.drop;
    (h.drop)(h.orig);
}

/// `velt_rt_fut_peek` of a holder of a shared promise: its settled result, copied into the
/// holder's slot for the formatter to read (it claims nothing).
///
/// # Safety
/// `f` must be a live holder of a shared promise.
pub(crate) unsafe fn peek(f: *mut VeltFut) -> bool {
    let h = hub(f);
    let st = h.st.lock();
    if st.done {
        std::ptr::copy_nonoverlapping(st.result.as_ptr() as *const u8, slot(f), h.size);
    }
    st.done
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The original future and a handle are told apart by their `poll` (`is_handle`): the two
    /// functions must keep their own addresses after the linker folded identical code.
    #[test]
    fn original_and_handle_polls_differ() {
        assert!(!std::ptr::fn_addr_eq(
            orig_poll as FutPoll,
            handle_poll as FutPoll
        ));
        assert!(!std::ptr::fn_addr_eq(
            orig_drop as FutDrop,
            handle_drop as FutDrop
        ));
    }
}
