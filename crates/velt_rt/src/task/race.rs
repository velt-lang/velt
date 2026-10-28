//! `velt_rt_race` / `velt_rt_race_ok`: `Promise.race` and `Promise.any` over a runtime-sized
//! array of heap futures.
//!
//! The children are driven like `velt_rt_all`'s (a `FuturesUnordered` of [`Child`]ren, so only
//! woken children are re-polled, with the same cooperative-budget rule); the winner moves its
//! result into the race's own result slot, and the others are dropped right away. Like JS, that
//! does not stop the losers: a started promise keeps running to completion (its task drives it
//! and disposes of its result), a spawned task keeps running; only lazy runtime leaves (a timer
//! nobody else waits for) are cancelled, which nothing can observe. A loser that is ready to go
//! on runs once before the race's awaiter continues (`local::give_up`).
//!
//! `race` takes the first child to settle. Children that had settled already when the race was
//! made, or are ready at its first poll, come first, in array order: JS subscribes to each
//! promise in array order, and a settled promise queues its reaction at once, before any promise
//! that settles later. `race_ok` (`Promise.any`) takes the first to *fulfill*: its children's
//! results are `Result<T, E>` slots (byte 0 is the tag, 0 = `Ok`), a rejection is dropped while
//! other children are still running, and when every child rejected the last rejection is the
//! result.
//!
//! Allocation layout (align 16): `[Tail][VeltFut hdr (16)][result (result_size)]`; the result
//! slot is at offset 16 like every `VeltFut`'s, and its size is only known at run time.

use std::alloc::Layout;
use std::ffi::c_void;
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use futures_util::stream::{FuturesUnordered, Stream};

use super::all::{give_up_in_order, Child, ResultDropFn};
use super::{context, SendPtr, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};

#[repr(C, align(16))]
struct Tail {
    /// `None` once a child won (the result is in the slot).
    children: Option<FuturesUnordered<Child>>,
    result_size: u64,
    /// `race_ok`: drops a rejected child's result (null: nothing to drop).
    reject_drop: Option<ResultDropFn>,
    /// `race_ok`: skip rejected children while others are running.
    first_ok: bool,
    /// Some child is a holder of a shared promise ([`settled_holder`]).
    holders: bool,
    /// What each child was when the race was made ([`settled_first`]); empty after the first
    /// poll.
    turns: Vec<Turn>,
}

const TAIL: usize = std::mem::size_of::<Tail>();

fn layout(result_size: u64) -> Layout {
    let size = TAIL + std::mem::size_of::<VeltFut>() + result_size as usize;
    Layout::from_size_align(size, 16)
        .unwrap_or_else(|_| crate::panic::fatal("invalid Promise.race result size"))
}

unsafe fn tail<'a>(f: *mut VeltFut) -> &'a mut Tail {
    &mut *((f as *mut u8).sub(TAIL) as *mut Tail)
}

unsafe extern "C" fn race_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let t = tail(f);
    let slot = (f as *mut u8).add(FUT_RESULT_OFFSET);
    match t.children.as_ref() {
        None => return READY,
        // `Promise.race([])` never settles.
        Some(c) if c.is_empty() => return PENDING,
        Some(_) => {}
    }
    if !t.turns.is_empty() {
        if let Some(r) = settled_first(t, slot, cx) {
            return r;
        }
    }
    let Some(children) = t.children.as_mut() else {
        return READY;
    };
    if t.holders && !t.first_ok {
        if let Some(i) = settled_holder(children) {
            return win(t, i, cx);
        }
    }
    let cx = context(cx);
    loop {
        match Pin::new(&mut *children).poll_next(cx) {
            Poll::Ready(Some(_)) => {}
            _ => return PENDING,
        }
        // A rejection (`race_ok`) loses unless it is the last child.
        if t.first_ok && *slot != 0 && !children.is_empty() {
            if let Some(d) = t.reject_drop {
                d(slot);
            }
            continue;
        }
        if let Some(losers) = t.children.take() {
            give_up_in_order(losers);
        }
        return READY;
    }
}

/// The child that is a holder of a shared promise that settled first, if one did and no other
/// child finished already. A holder settles when its promise does, and the race hears of it
/// with its other wake-ups, in the order they came; a child it adopted settles only when the
/// race polls it, which may be later than the holder, so polling in wake-up order would let it
/// win (Node takes the first to settle, task/shared.rs).
unsafe fn settled_holder(children: &FuturesUnordered<Child>) -> Option<usize> {
    let mut best: Option<(u64, usize)> = None;
    for c in children.iter() {
        let f = c.fut.0;
        match crate::task::shared::settled_at(f) {
            Some(at) if best.is_none_or(|(b, _)| at < b) => best = Some((at, c.index)),
            Some(_) => {}
            // A started promise that finished: the wake-up order decides.
            None if crate::task::local::is_started_node(f) && crate::task::local::node_done(f) => {
                return None;
            }
            None => {}
        }
    }
    best.map(|(_, i)| i)
}

/// What a child future was when the race was made.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Turn {
    /// Settled: a holder of a shared promise that settled, a started promise that finished, or
    /// a converted promise (`Promise<T>` raced as a `Promise<T, E>`) of a shared one that did.
    Settled,
    /// Pending: such a promise that has not settled. Polling it now could run it (an adopted
    /// started promise goes on inside the poll), ahead of a settled child later in the array.
    Pending,
    /// Anything else (`Promise.resolve(v)`, a runtime leaf): polling it only looks.
    Other,
}

unsafe fn turn(f: *mut VeltFut) -> Turn {
    use crate::task::shared::{is_shared, settled_at};
    let settled = |h: *mut VeltFut| settled_at(h).is_some();
    if is_shared(f) {
        return if settled(f) {
            Turn::Settled
        } else {
            Turn::Pending
        };
    }
    if crate::task::local::is_started_node(f) {
        return match crate::task::local::node_done(f) {
            true => Turn::Settled,
            false => Turn::Pending,
        };
    }
    match crate::task::identity::converted_from(f) {
        Some(h) if settled(h) => Turn::Settled,
        Some(_) => Turn::Pending,
        None => Turn::Other,
    }
}

/// The first poll goes through the children in array order, as JS subscribes to them: the
/// first that settled before the race was made, or that is ready when looked at, wins
/// (`race_ok`: the first fulfilled; a rejection is dropped unless it is the last child), so a
/// settled promise wins over any that settles later, whichever settled first in time.
/// `Some(READY)` when one won, `Some(PENDING)` when the cooperative budget ran out (the turn is
/// taken at the next poll), `None` when none won: the rest go on in a new set, which polls each
/// of them (in array order) with its own waker.
unsafe fn settled_first(t: &mut Tail, slot: *mut u8, cx: *mut c_void) -> Option<u32> {
    if !super::coop::has_budget_remaining() {
        context(cx).waker().wake_by_ref();
        return Some(PENDING);
    }
    let mut rest: Vec<Child> = t.children.take()?.into_iter().collect();
    rest.sort_unstable_by_key(|c| c.index);
    let turns = std::mem::take(&mut t.turns);
    let mut at = 0;
    while at < rest.len() {
        if turns.get(rest[at].index) == Some(&Turn::Pending) {
            at += 1;
            continue;
        }
        let mut w = rest.remove(at);
        if Pin::new(&mut w).poll(context(cx)).is_pending() {
            rest.insert(at, w);
            at += 1;
            continue;
        }
        drop(w);
        if t.first_ok && *slot != 0 && !rest.is_empty() {
            if let Some(d) = t.reject_drop {
                d(slot);
            }
            continue;
        }
        give_up_in_order(rest.into_iter().collect());
        return Some(READY);
    }
    t.children = Some(rest.into_iter().collect());
    None
}

/// Child `i` (a settled holder) wins: its result goes to the race's slot, the others give up.
unsafe fn win(t: &mut Tail, i: usize, cx: *mut c_void) -> u32 {
    let Some(children) = t.children.take() else {
        return READY;
    };
    let mut rest: Vec<Child> = children.into_iter().collect();
    let Some(at) = rest.iter().position(|c| c.index == i) else {
        t.children = Some(rest.into_iter().collect());
        return PENDING;
    };
    let mut w = rest.swap_remove(at);
    if Pin::new(&mut w).poll(context(cx)).is_pending() {
        rest.push(w);
        t.children = Some(rest.into_iter().collect());
        return PENDING;
    }
    drop(w);
    give_up_in_order(rest.into_iter().collect());
    READY
}

unsafe extern "C" fn race_drop(f: *mut VeltFut) {
    let t = tail(f);
    let l = layout(t.result_size);
    drop_in_order(t.children.take());
    std::ptr::drop_in_place(t);
    std::alloc::dealloc((f as *mut u8).sub(TAIL), l);
}

/// Drop the children of a race dropped before it settled, in array order (see
/// [`give_up_in_order`]; nothing runs now).
fn drop_in_order(children: Option<FuturesUnordered<Child>>) {
    let Some(children) = children else {
        return;
    };
    let mut losers: Vec<Child> = children.into_iter().collect();
    losers.sort_unstable_by_key(|c| c.index);
    drop(losers);
}

unsafe fn new_race(
    futs: *const *mut VeltFut,
    n: u64,
    result_size: u64,
    first_ok: Option<Option<ResultDropFn>>,
) -> *mut VeltFut {
    let l = layout(result_size);
    let base = std::alloc::alloc(l);
    if base.is_null() {
        std::alloc::handle_alloc_error(l);
    }
    let f = base.add(TAIL) as *mut VeltFut;
    let dst = (f as *mut u8).add(FUT_RESULT_OFFSET);
    let holders = (0..n as usize).any(|i| crate::task::shared::is_shared(*futs.add(i)));
    let turns = (0..n as usize).map(|i| turn(*futs.add(i))).collect();
    let children = (0..n as usize)
        .map(|i| Child {
            fut: SendPtr(*futs.add(i)),
            dst: SendPtr(dst),
            size: result_size as usize,
            index: i,
        })
        .collect();
    (base as *mut Tail).write(Tail {
        children: Some(children),
        result_size,
        reject_drop: first_ok.flatten(),
        first_ok: first_ok.is_some(),
        holders,
        turns,
    });
    f.write(VeltFut {
        poll: race_poll,
        drop: race_drop,
    });
    f
}

/// If `f` is a race, mark each of its children to transfer its result with `transfer`
/// (`velt_rt_fut_transfer`): the winner's result is a child's. Whether `f` was one.
pub(super) unsafe fn pass_transfer(f: *mut VeltFut, transfer: Option<ResultDropFn>) -> bool {
    let race = race_poll as unsafe extern "C" fn(*mut VeltFut, *mut c_void) -> u32;
    if !std::ptr::fn_addr_eq((*f).poll, race) {
        return false;
    }
    if let Some(children) = tail(f).children.as_ref() {
        for c in children.iter() {
            crate::task::local::velt_rt_fut_transfer(c.fut.0, transfer);
        }
    }
    true
}

/// `Promise.race(array)`: takes ownership of the `n` futures in `futs` (not of the pointer array)
/// and returns a future whose result slot (offset 16) receives the `result_size`-byte result of
/// the first child to finish. The other children are dropped then (see the module docs).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_race(
    futs: *const *mut VeltFut,
    n: u64,
    result_size: u64,
) -> *mut VeltFut {
    new_race(futs, n, result_size, None)
}

/// `Promise.any(array)`: like [`velt_rt_race`] over children whose results are `Result<T, E>`
/// slots (tag byte at offset 0, 0 = fulfilled): the first fulfilled child wins; a rejected one is
/// dropped with `reject_drop` (null: nothing to drop) unless every other child already finished,
/// in which case its rejection is the result. `n == 0` never completes (`Promise.any([])` is
/// rejected by the caller).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_race_ok(
    futs: *const *mut VeltFut,
    n: u64,
    result_size: u64,
    reject_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    new_race(futs, n, result_size, Some(reject_drop))
}
