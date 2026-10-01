//! Promise nodes: the heap form of a compiled async state (`velt_rt_fut_box`) and what it
//! becomes once started (`velt_rt_fut_start`).
//!
//! Allocation layout (align 16): `[Head (96)][VeltFut hdr (16)][state ...]`. The returned pointer
//! is `hdr`, so the state (whose result lives at its offset 0) starts exactly at the result slot
//! offset 16. The head sits before the header so its position does not depend on the state size.
//!
//! A *lazy* node (just boxed) is polled by whoever awaits it, like any heap future. A *started*
//! node is driven by the local set of the task that started it (`set.rs`); its owner only
//! observes completion. Who may touch what:
//! - the state is polled and cancelled only by the driving task (never concurrently);
//! - `flags` and `refs` are atomics shared with the owner (possibly another task) and wakers;
//! - `member` is the driving task's bookkeeping, `owner` the owner's (plain memory).
//!
//! Reference counting (started nodes): the owner's handle, set membership, a place in the ready
//! queue and every cloned waker each hold one reference. The result slot belongs to whoever
//! sees completion last: the owner if it was still attached (it awaited it: `DELIVERED`, or it
//! drops it unclaimed), the set if the owner had already dropped its handle (`DETACHED`).
//!
//! The common case — a promise that finishes during its first poll (`fanout_all`) — costs one
//! atomic read-modify-write: the node takes a counted reference to its set only when it
//! suspends (or when a waker raced with its completion), and the owner's side is plain memory.

use std::alloc::Layout;
use std::cell::Cell;
use std::ffi::c_void;
use std::future::Future;
use std::sync::atomic::{fence, AtomicPtr, AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use futures_util::task::AtomicWaker;

use super::set::{LocalSet, Shared};
use crate::task::all::ResultDropFn;
use crate::task::{context, DropFn, PollFn, VeltFut, PENDING, READY};

/// The state finished; its result is in the slot (unless it was handed out).
pub(super) const DONE: u32 = 1;
/// The owner dropped its handle: the set disposes of the result.
pub(super) const DETACHED: u32 = 2;
/// The driving task was dropped before the state finished (the state was cancelled).
pub(super) const CANCELLED: u32 = 8;
/// In the set's ready queue.
pub(super) const QUEUED: u32 = 16;
/// Driven by a local set (`velt_rt_fut_start` succeeded).
pub(super) const STARTED: u32 = 32;
/// `set` holds a counted reference (`Arc::increment_strong_count`), released by `free`.
const SET_REF: u32 = 64;
/// Its owner drives it (see [`started_poll`]): its leaves hold the owner's wakers.
const ADOPTED: u32 = 128;

/// `owner` bit of a lazy node: its state finished (it is polled by its owner only).
const OWNER_DONE: u32 = 1;
/// `owner` bit of a started node: the owner's poll returned READY (the result is claimed).
const OWNER_DELIVERED: u32 = 2;

/// `member` value of a node that is not in a set's member list.
pub(super) const NO_MEMBER: u32 = u32::MAX;

/// Bookkeeping in front of every boxed compiled state.
#[repr(C, align(16))]
pub(super) struct Head {
    pub poll: PollFn,
    pub drop: DropFn,
    /// Drops an unclaimed result (started nodes; `None` when the result needs no drop).
    pub result_drop: Option<ResultDropFn>,
    pub state_size: u32,
    pub flags: AtomicU32,
    pub refs: AtomicUsize,
    /// Index in the driving set's member list, or [`NO_MEMBER`] (driving task only).
    pub member: AtomicU32,
    /// `OWNER_*` bits, touched only by whoever holds the handle.
    owner: Cell<u32>,
    /// The driving set (null while lazy; a counted reference once `SET_REF` is set).
    pub set: *const Shared,
    /// Link in the set's queue of woken nodes (set.rs).
    next: AtomicPtr<VeltFut>,
    /// Waker of the owner while it waits for a started node.
    pub awaiter: AtomicWaker,
}

const HEAD: usize = std::mem::size_of::<Head>();
const PREFIX: usize = HEAD + std::mem::size_of::<VeltFut>();

const _: () = assert!(HEAD == 96);

fn layout(state_size: u32) -> Layout {
    Layout::from_size_align(PREFIX + state_size as usize, 16)
        .unwrap_or_else(|_| crate::panic::fatal("invalid boxed future size"))
}

/// The head of node `f`.
///
/// # Safety
/// `f` must be a node allocated by [`alloc_node`].
pub(super) unsafe fn head<'a>(f: *mut VeltFut) -> &'a Head {
    &*((f as *mut u8).sub(HEAD) as *const Head)
}

unsafe fn head_mut<'a>(f: *mut VeltFut) -> &'a mut Head {
    &mut *((f as *mut u8).sub(HEAD) as *mut Head)
}

/// The compiled state of node `f` (its result is at offset 0: the future's result slot).
pub(super) unsafe fn state(f: *mut VeltFut) -> *mut u8 {
    (f as *mut u8).add(std::mem::size_of::<VeltFut>())
}

/// A lazy node holding a copy of the `state_size` bytes at `src`.
pub(super) unsafe fn alloc_node(
    poll: PollFn,
    drop: DropFn,
    src: *const u8,
    state_size: u64,
) -> *mut VeltFut {
    let Ok(state_size) = u32::try_from(state_size) else {
        crate::panic::fatal("async state larger than 4 GiB");
    };
    let l = layout(state_size);
    let base = std::alloc::alloc(l);
    if base.is_null() {
        std::alloc::handle_alloc_error(l);
    }
    (base as *mut Head).write(Head {
        poll,
        drop,
        result_drop: None,
        state_size,
        flags: AtomicU32::new(0),
        refs: AtomicUsize::new(1),
        member: AtomicU32::new(NO_MEMBER),
        owner: Cell::new(0),
        set: std::ptr::null(),
        next: AtomicPtr::new(std::ptr::null_mut()),
        awaiter: AtomicWaker::new(),
    });
    let f = base.add(HEAD) as *mut VeltFut;
    f.write(VeltFut {
        poll: lazy_poll,
        drop: lazy_drop,
    });
    std::ptr::copy_nonoverlapping(src, state(f), state_size as usize);
    f
}

unsafe fn free(f: *mut VeltFut) {
    let h = head_mut(f);
    if *h.flags.get_mut() & SET_REF != 0 {
        drop(Arc::from_raw(h.set));
    }
    let l = layout(h.state_size);
    std::ptr::drop_in_place(&mut h.awaiter);
    std::alloc::dealloc((f as *mut u8).sub(HEAD), l);
}

/// Drop one reference; the last one frees the node.
pub(super) unsafe fn release(f: *mut VeltFut) {
    let refs = &head(f).refs;
    // The only reference: nobody else can take a new one, so no atomic update is needed.
    if refs.load(Ordering::Acquire) == 1 || refs.fetch_sub(1, Ordering::Release) == 1 {
        fence(Ordering::Acquire);
        free(f);
    }
}

pub(super) unsafe fn retain(f: *mut VeltFut) {
    head(f).refs.fetch_add(1, Ordering::Relaxed);
}

/// The queue link of node `f`.
pub(super) unsafe fn next_slot<'a>(f: *mut VeltFut) -> &'a AtomicPtr<VeltFut> {
    &head(f).next
}

/// Is `f` a lazy node (created by `velt_rt_fut_box`, not started)?
pub(super) unsafe fn is_lazy(f: *mut VeltFut) -> bool {
    std::ptr::fn_addr_eq(
        (*f).poll,
        lazy_poll as unsafe extern "C" fn(*mut VeltFut, *mut c_void) -> u32,
    )
}

unsafe extern "C" fn lazy_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let h = head(f);
    if h.owner.get() & OWNER_DONE != 0 {
        return READY;
    }
    let r = (h.poll)(state(f), cx);
    if r == READY {
        h.owner.set(OWNER_DONE);
    }
    r
}

unsafe extern "C" fn lazy_drop(f: *mut VeltFut) {
    let h = head(f);
    if h.owner.get() & OWNER_DONE == 0 {
        (h.drop)(state(f));
    }
    free(f);
}

/// Turn lazy node `f` into a started node driven by the set `shared` (not counted yet: see
/// [`count_set`]; the set outlives the poll that starts the node).
pub(super) unsafe fn mark_started(
    f: *mut VeltFut,
    shared: &Arc<Shared>,
    result_drop: Option<ResultDropFn>,
) {
    let h = head_mut(f);
    h.set = Arc::as_ptr(shared);
    h.result_drop = result_drop;
    h.flags.store(STARTED, Ordering::Relaxed);
    (*f).poll = started_poll;
    (*f).drop = started_drop;
}

/// Poll the state of started node `f` with its own waker; true when it finished.
pub(super) unsafe fn poll_state(f: *mut VeltFut) -> bool {
    let w =
        std::mem::ManuallyDrop::new(Waker::from_raw(RawWaker::new(f as *const (), &NODE_WAKER)));
    let mut cx = Context::from_waker(&w);
    let h = head(f);
    (h.poll)(state(f), crate::task::raw_cx(&mut cx)) == READY
}

/// The first poll of started node `f`, part of its creator's synchronous run (like calling an
/// async function in JS): not limited by the task's cooperative budget, which the creator may
/// have spent (a loop starting 100k timers must register them all now, not defer each one).
pub(super) unsafe fn poll_first(f: *mut VeltFut) -> bool {
    let w =
        std::mem::ManuallyDrop::new(Waker::from_raw(RawWaker::new(f as *const (), &NODE_WAKER)));
    let h = head(f);
    let first = std::future::poll_fn(|cx| match (h.poll)(state(f), crate::task::raw_cx(cx)) {
        READY => Poll::Ready(()),
        _ => Poll::Pending,
    });
    let mut first = std::pin::pin!(tokio::task::unconstrained(first));
    first.as_mut().poll(&mut Context::from_waker(&w)).is_ready()
}

/// Make `f`'s reference to its set a counted one (it outlives the poll that started it).
pub(super) unsafe fn count_set(f: *mut VeltFut) {
    let h = head(f);
    Arc::increment_strong_count(h.set);
    h.flags.fetch_or(SET_REF, Ordering::Relaxed);
}

/// Started node `f` finished during its first poll: nobody can await it yet. A waker that fired
/// meanwhile may still be on its way into the set's queue, so then the set is kept alive.
pub(super) unsafe fn finish_first(f: *mut VeltFut) {
    if head(f).flags.fetch_or(DONE, Ordering::AcqRel) & QUEUED != 0 {
        count_set(f);
    }
}

/// The state of started node `f` finished: publish it to the owner (returning the waker of an
/// owner waiting for it), or drop the result if the owner is gone.
pub(super) unsafe fn finish(f: *mut VeltFut) -> Option<Waker> {
    let h = head(f);
    let prev = h.flags.fetch_or(DONE, Ordering::AcqRel);
    if prev & DETACHED != 0 {
        drop_result(f);
        return None;
    }
    h.awaiter.take()
}

/// The started node of set `shared` that waker `w` belongs to, if it is one that is still
/// running (its awaiter can then be run directly instead of being queued).
pub(super) unsafe fn local_awaiter(w: &Waker, shared: &Arc<Shared>) -> Option<*mut VeltFut> {
    if !std::ptr::eq(w.vtable(), &NODE_WAKER) {
        return None;
    }
    let g = w.data() as *mut VeltFut;
    let h = head(g);
    let running = h.flags.load(Ordering::Acquire) & (DONE | CANCELLED) == 0;
    let member = h.member.load(Ordering::Relaxed) != NO_MEMBER;
    (std::ptr::eq(h.set, Arc::as_ptr(shared)) && running && member).then_some(g)
}

/// The driving task went away before `f` finished: cancel the state.
pub(super) unsafe fn cancel(f: *mut VeltFut) {
    let h = head(f);
    let prev = h.flags.fetch_or(CANCELLED, Ordering::AcqRel);
    if prev & DONE == 0 {
        (h.drop)(state(f));
        h.awaiter.wake();
    }
}

unsafe fn drop_result(f: *mut VeltFut) {
    if let Some(d) = head(f).result_drop {
        d(state(f));
    }
}

/// The owner awaits a started node: ready once it finished. Awaited from inside the task that
/// drives it and not woken meanwhile, the owner polls the state itself ("adopts" it, like a
/// parent polling its children: the leaves then wake the owner directly, with no trip through
/// the set); otherwise it waits for the set to finish it.
unsafe extern "C" fn started_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let h = head(f);
    let fl = h.flags.load(Ordering::Acquire);
    if fl & DONE == 0 {
        let done = match adoptable(f, fl) {
            Some(set) => drive_adopted(set, f, cx, fl),
            None => wait(f, cx, fl),
        };
        if !done {
            return PENDING;
        }
    }
    h.owner.set(OWNER_DELIVERED);
    READY
}

/// The set of the task being polled, if it drives unfinished, un-woken node `f`.
unsafe fn adoptable(f: *mut VeltFut, fl: u32) -> Option<*mut LocalSet> {
    let h = head(f);
    if fl & (QUEUED | CANCELLED) != 0 || h.member.load(Ordering::Relaxed) == NO_MEMBER {
        return None;
    }
    let set = super::current_set();
    (!set.is_null() && std::ptr::eq(Arc::as_ptr(&(*set).shared), h.set)).then_some(set)
}

/// Poll adopted node `f` with its owner's context; true when it finished.
unsafe fn drive_adopted(set: *mut LocalSet, f: *mut VeltFut, cx: *mut c_void, fl: u32) -> bool {
    let h = head(f);
    if fl & ADOPTED == 0 {
        h.flags.fetch_or(ADOPTED, Ordering::Relaxed);
    }
    if (h.poll)(state(f), cx) != READY {
        return false;
    }
    super::set::finish_adopted(set, f);
    true
}

/// Wait for the set to finish `f`; true when it did. An adopted node goes back to its set (its
/// leaves would wake an owner that no longer polls it).
unsafe fn wait(f: *mut VeltFut, cx: *mut c_void, fl: u32) -> bool {
    let h = head(f);
    if fl & ADOPTED != 0 {
        h.flags.fetch_and(!ADOPTED, Ordering::Relaxed);
        waker_wake_by_ref(f as *const ());
    }
    let ready = |h: &Head| {
        let fl = h.flags.load(Ordering::Acquire);
        if fl & CANCELLED != 0 && fl & DONE == 0 {
            crate::panic::fatal("a promise was awaited after the task running it was cancelled");
        }
        fl & DONE != 0
    };
    if ready(h) {
        return true;
    }
    h.awaiter.register(context(cx).waker());
    ready(h)
}

/// The owner drops a started node. JS semantics: an unfinished promise keeps running (its set
/// drives it to completion and drops the result); a finished, unclaimed result is dropped here.
unsafe extern "C" fn started_drop(f: *mut VeltFut) {
    let h = head(f);
    // Finished: the set is done with the result, no need to tell it the owner is gone.
    let fl = h.flags.load(Ordering::Acquire);
    if fl & (ADOPTED | DONE) == ADOPTED {
        // Its leaves would wake this owner: let the set poll it (and re-register) instead.
        h.flags.fetch_and(!ADOPTED, Ordering::Relaxed);
        waker_wake_by_ref(f as *const ());
    }
    let done = fl & DONE != 0 || h.flags.fetch_or(DETACHED, Ordering::AcqRel) & DONE != 0;
    if done && h.owner.get() & OWNER_DELIVERED == 0 {
        drop_result(f);
    }
    release(f);
}

/// Node wakers queue the node in its set's ready list.
static NODE_WAKER: RawWakerVTable =
    RawWakerVTable::new(waker_clone, waker_wake, waker_wake_by_ref, waker_drop);

unsafe fn waker_clone(p: *const ()) -> RawWaker {
    retain(p as *mut VeltFut);
    RawWaker::new(p, &NODE_WAKER)
}

unsafe fn waker_wake(p: *const ()) {
    waker_wake_by_ref(p);
    waker_drop(p);
}

unsafe fn waker_wake_by_ref(p: *const ()) {
    let f = p as *mut VeltFut;
    let h = head(f);
    let prev = h.flags.fetch_or(QUEUED, Ordering::AcqRel);
    if prev & (QUEUED | DONE | CANCELLED) != 0 {
        if prev & QUEUED == 0 {
            h.flags.fetch_and(!QUEUED, Ordering::AcqRel);
        }
        return;
    }
    retain(f);
    if !(*h.set).push(f) {
        h.flags.fetch_and(!QUEUED, Ordering::AcqRel);
        release(f);
    }
}

unsafe fn waker_drop(p: *const ()) {
    release(p as *mut VeltFut);
}
