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
//! `race` takes the first child to settle. `race_ok` (`Promise.any`) takes the first to
//! *fulfill*: its children's results are `Result<T, E>` slots (byte 0 is the tag, 0 = `Ok`), a
//! rejection is dropped while other children are still running, and when every child rejected
//! the last rejection is the result.
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
    let Some(children) = t.children.as_mut() else {
        return READY;
    };
    if children.is_empty() {
        // `Promise.race([])` never settles.
        return PENDING;
    }
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
