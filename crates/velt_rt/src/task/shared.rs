//! Shared promises (https://veltlang.com/docs/reference/async/ "Promises", #212): a promise that several places
//! hold, each of which may await it and gets the result, as in JS.
//!
//! The executor in Velt shares its own promises (std/lang/hooks/sched_share.velt) and refuses a
//! share outside any task (#3782). What is left here is the runtime's hub, for a program that runs
//! nothing on the executor (lowering's `sched_share_live`): every promise such a program holds is a
//! runtime future, which nothing drives until it is awaited.
//!
//! The first share (`velt_rt_fut_share`) turns the future into a shared one in place: its
//! header's `poll` / `drop` become [`holder_poll`] / [`holder_drop`], and the original entries move
//! to a side record (the hub, found by the future's address). The original pointer stays its
//! first holder's; every other holder gets a handle of its own (a small heap future with a
//! result slot, the hub in front of its header), so each holder awaits, and is dropped, as one
//! future. The hub counts the holders, keeps the settled result once the original future
//! finished, and lists the waiting holders in the order they started waiting. A holder that sees
//! `READY` moves the result out of its slot at +16, where the hub wrote a fresh share of the
//! settled result (the same object for an object result, a copy of a string or a value).
//!
//! The first waiter drives the original future with its own context; when it settles there, that
//! waiter goes on as an unshared promise's awaiter would, and the others are woken in the order
//! they started waiting.
//!
//! Dropping a holder never cancels the promise (JS never does). A waiting holder that is dropped
//! wakes the others, so none waits for a driver that went away. The last holder releases the
//! settled result, or, when the promise has not settled, gives the original future back its
//! entries and drops it.

use std::alloc::Layout;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::task::Waker;

use parking_lot::Mutex;

use crate::panic::ThrowLoc;

use super::ResultDropFn;
use super::{with_context, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};

/// Compiled share glue of a result slot: writes another reference to `*src` into `dst`.
pub type ShareFn = unsafe extern "C-unwind" fn(src: *const u8, dst: *mut u8);

type FutPoll = unsafe extern "C-unwind" fn(f: *mut VeltFut, cx: *mut c_void) -> u32;
type FutDrop = unsafe extern "C-unwind" fn(f: *mut VeltFut);

/// The side record of a shared future.
pub(super) struct Hub {
    /// The original future (its first holder's pointer), and its own entries.
    orig: *mut VeltFut,
    poll: FutPoll,
    drop: FutDrop,
    size: usize,
    share: Option<ShareFn>,
    result_drop: Option<ResultDropFn>,
    st: Mutex<HubState>,
}

// SAFETY: a hub is reached from whoever awaits its promise (module docs); `orig` is the future it
// belongs to, and the state is behind a lock.
unsafe impl Send for Hub {}
// SAFETY: as above.
unsafe impl Sync for Hub {}

struct HubState {
    holders: usize,
    done: bool,
    /// The settled result (16-byte aligned words), once `done`.
    result: Vec<u128>,
    /// Where its error was thrown, for every holder that takes it (panic.rs).
    loc: ThrowLoc,
    /// The waiting holders (by address) in the order they started waiting, with their wakers.
    waiters: Vec<(usize, Waker)>,
}

/// The hubs of the shared original futures, by address.
static HUBS: Mutex<Option<HashMap<usize, Arc<Hub>>>> = Mutex::new(None);

/// Bytes in front of a handle's header: its hub, then a word kept 0.
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
        None => (*((f as *mut u8).sub(FRONT) as *const Arc<Hub>)).clone(),
    }
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
pub unsafe extern "C-unwind" fn velt_rt_fut_share(
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
                loc: ThrowLoc::NONE,
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
    new_handle(h)
}

/// The original future of holder `f`'s shared promise (task/identity.rs); `None` when `f` is not
/// a holder.
///
/// # Safety
/// `f` must be a live heap future.
pub(super) unsafe fn origin(f: *mut VeltFut) -> Option<*mut VeltFut> {
    is_shared(f).then(|| hub(f).orig)
}

/// A new holder of the promise of `h`: a handle of its own.
unsafe fn new_handle(h: Arc<Hub>) -> *mut VeltFut {
    h.st.lock().holders += 1;
    let l = handle_layout(h.size);
    let base = std::alloc::alloc(l);
    if base.is_null() {
        std::alloc::handle_alloc_error(l);
    }
    (base as *mut Arc<Hub>).write(h);
    (base.add(FRONT - 8) as *mut u64).write(0);
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
unsafe extern "C-unwind" fn holder_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    poll_holder(&hub(f), f, cx)
}

/// `drop` of every holder: a handle also frees its own block.
unsafe extern "C-unwind" fn holder_drop(f: *mut VeltFut) {
    if let Some(h) = orig_hub(f) {
        return release(h, f);
    }
    let base = (f as *mut u8).sub(FRONT);
    let h = (base as *mut Arc<Hub>).read();
    let l = handle_layout(h.size);
    release(h, f);
    std::alloc::dealloc(base, l);
}

/// Poll holder `f` of the promise of `h`.
unsafe fn poll_holder(h: &Arc<Hub>, f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let me = f as usize;
    let mut st = h.st.lock();
    if !st.done {
        let w = with_context(cx, |c| c.waker().clone());
        let first = match st.waiters.iter().position(|(x, _)| *x == me) {
            Some(i) => {
                if !st.waiters[i].1.will_wake(&w) {
                    st.waiters[i].1 = w;
                }
                i == 0
            }
            None => {
                st.waiters.push((me, w));
                st.waiters.len() == 1
            }
        };
        if !first {
            return PENDING;
        }
        drop(st);
        // The first waiter drives the original future (no lock held: it runs compiled code).
        // Velt's own raise may unwind through here: the next waiter then drives, not waiting for
        // a driver that is gone.
        let guard = DriverGone { h: h.as_ref(), me };
        let r = (h.poll)(h.orig, cx);
        std::mem::forget(guard);
        if r != READY {
            return PENDING;
        }
        st = h.st.lock();
        if !st.done {
            keep_result(h, &mut st);
            // This waiter goes on now; the others resume in the order they started waiting.
            let waiters = std::mem::take(&mut st.waiters);
            drop(st);
            for (x, w) in waiters {
                if x != me {
                    w.wake();
                }
            }
            st = h.st.lock();
        }
    }
    st.waiters.retain(|(x, _)| *x != me);
    let result = st.result.as_ptr() as *const u8;
    let loc = st.loc;
    drop(st);
    loc.restore_if_known();
    match h.share {
        Some(share) => share(result, slot(f)),
        None => std::ptr::copy_nonoverlapping(result, slot(f), h.size),
    }
    READY
}

/// Dropped only when the poll of the first waiter `me`, driving `h`'s original, unwinds: it leaves
/// the waiters and wakes the next one to drive ([`poll_holder`]).
struct DriverGone<'a> {
    h: &'a Hub,
    me: usize,
}

impl Drop for DriverGone<'_> {
    fn drop(&mut self) {
        let mut st = self.h.st.lock();
        st.waiters.retain(|(x, _)| *x != self.me);
        if let Some(w) = st.waiters.first().map(|(_, w)| w.clone()) {
            drop(st);
            w.wake();
        }
    }
}

/// The original future of `h` finished, driven on this thread: keep its result in the hub (`st`
/// is its locked state, not settled yet).
unsafe fn keep_result(h: &Hub, st: &mut HubState) {
    st.loc = ThrowLoc::current();
    let mut result = vec![0u128; h.size.div_ceil(16)];
    std::ptr::copy_nonoverlapping(slot(h.orig), result.as_mut_ptr() as *mut u8, h.size);
    st.result = result;
    st.done = true;
}

/// Holder `f` of the promise of `h` is dropped.
unsafe fn release(h: Arc<Hub>, f: *mut VeltFut) {
    let me = f as usize;
    let mut st = h.st.lock();
    st.holders -= 1;
    if st.holders > 0 {
        let Some(i) = st.waiters.iter().position(|(x, _)| *x == me) else {
            return;
        };
        st.waiters.remove(i);
        // It may have been driving the promise: the others wait again, the first drives.
        let wake: Vec<Waker> = match st.done {
            true => vec![],
            false => st.waiters.iter().map(|(_, w)| w.clone()).collect(),
        };
        drop(st);
        for w in wake {
            w.wake();
        }
        return;
    }
    let done = st.done;
    let mut result = std::mem::take(&mut st.result);
    let loc = st.loc;
    drop(st);
    if let Some(m) = HUBS.lock().as_mut() {
        m.remove(&(h.orig as usize));
    }
    if done {
        if let Some(d) = h.result_drop {
            // An unhandled rejection is reported with where it was thrown.
            let own = if loc.is_known() {
                loc.replace()
            } else {
                ThrowLoc::current()
            };
            d(result.as_mut_ptr() as *mut u8);
            own.restore();
        }
    }
    (*h.orig).poll = h.poll;
    (*h.orig).drop = h.drop;
    (h.drop)(h.orig);
}
