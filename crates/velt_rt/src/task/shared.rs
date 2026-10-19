//! Shared promises (docs/reference/async.md "Promises", #212): a promise that several places
//! hold, each of which may await it and gets the result, as in JS.
//!
//! The first share (`velt_rt_fut_share`) turns the future into a shared one in place: its
//! header's `poll` / `drop` become [`holder_poll`] / [`holder_drop`], and the original entries move
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
use std::sync::atomic::{AtomicUsize, Ordering};
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
    /// The task the promise is shared on (`local::task_key`; 0: not known yet).
    task: AtomicUsize,
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

/// The hub of holder `f`: an original future is always in [`HUBS`] (by its address), a handle
/// never is (its hub is in front of its header). The header's `poll` only says whether `f` is
/// shared at all ([`is_shared`]): it never tells an original from a handle, so a linker that
/// folds functions cannot make one read as the other.
unsafe fn hub(f: *mut VeltFut) -> Arc<Hub> {
    match orig_hub(f) {
        Some(h) => h,
        None => handle_hub(f),
    }
}

/// The hub of handle `f`, kept in front of its header.
unsafe fn handle_hub(f: *mut VeltFut) -> Arc<Hub> {
    (*((f as *mut u8).sub(FRONT) as *const Arc<Hub>)).clone()
}

/// The hub of `f` if it is an original future (by its address).
fn orig_hub(f: *mut VeltFut) -> Option<Arc<Hub>> {
    HUBS.lock()
        .as_ref()
        .and_then(|m| m.get(&(f as usize)))
        .cloned()
}

/// Is `f` a holder of a shared promise (its original future or a handle)?
///
/// # Safety
/// `f` must be a live heap future.
pub(crate) unsafe fn is_shared(f: *mut VeltFut) -> bool {
    std::ptr::fn_addr_eq((*f).poll, holder_poll as FutPoll)
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
            task: AtomicUsize::new(super::local::task_key()),
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
        (*f).poll = holder_poll;
        (*f).drop = holder_drop;
        h
    };
    on_its_task(&h);
    h.st.lock().holders += 1;
    let l = handle_layout(h.size);
    let base = std::alloc::alloc(l);
    if base.is_null() {
        std::alloc::handle_alloc_error(l);
    }
    (base as *mut Arc<Hub>).write(h);
    let g = base.add(FRONT) as *mut VeltFut;
    g.write(VeltFut {
        poll: holder_poll,
        drop: holder_drop,
    });
    g
}

unsafe fn slot(f: *mut VeltFut) -> *mut u8 {
    (f as *mut u8).add(FUT_RESULT_OFFSET)
}

/// `poll` of every holder, the original future and the handles alike.
unsafe extern "C" fn holder_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    poll_holder(&hub(f), f, cx)
}

/// `drop` of every holder: a handle also frees its own block.
unsafe extern "C" fn holder_drop(f: *mut VeltFut) {
    if let Some(h) = orig_hub(f) {
        return release(h, f);
    }
    let base = (f as *mut u8).sub(FRONT);
    let h = (base as *mut Arc<Hub>).read();
    let l = handle_layout(h.size);
    release(h, f);
    std::alloc::dealloc(base, l);
}

/// Stop the program when the promise of `h` is used on another task than the one it is shared
/// on (a holder passed to `spawn` or sent over a channel): its original future runs on that
/// task, and its result would be used from two threads. Outside a task (the runtime dropping
/// what is left at exit) anything goes.
fn on_its_task(h: &Hub) {
    let now = super::local::task_key();
    if now == 0 {
        return;
    }
    match h
        .task
        .compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed)
    {
        Ok(_) => {}
        Err(t) if t == now => {}
        Err(_) => crate::panic::fatal(
            "a promise shared on one task was used on another (`spawn`, a channel): awaiting a \
             shared promise from another task is not supported yet (#212); await it on its own \
             task and pass the result",
        ),
    }
}

/// Poll holder `f` of the promise of `h`.
unsafe fn poll_holder(h: &Arc<Hub>, f: *mut VeltFut, cx: *mut c_void) -> u32 {
    on_its_task(h);
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
    on_its_task(&h);
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
