//! `velt_rt_race` / `velt_rt_race_ok`: `Promise.race` and `Promise.any` over a runtime-sized
//! array of heap futures. Each poll polls every child (single-threaded, like `velt_rt_all`); the
//! winner moves its result into the race's result slot and the others are dropped (a started
//! promise keeps running, JS-like). `race_ok` skips rejected children (`Result<T, E>` slots, tag
//! byte 0 = fulfilled) until only the last one is left: see velt_rt's task/race.rs.
//!
//! Allocation layout (align 16): `[Tail, padded to 16][VeltFut header (16)][result]`: the result
//! slot is at offset 16 like every `VeltFut`'s, and its size is only known at run time.

use std::alloc::Layout;
use std::ffi::c_void;

use super::all::ResultDropFn;
use super::{VeltFut, Wide, FUT_RESULT_OFFSET, PENDING, READY};

struct Tail {
    /// Racing children; empty once one won.
    children: Vec<*mut VeltFut>,
    result_size: u64,
    done: bool,
    /// `race_ok`: rejected children lose while others are running (dropped with the function).
    first_ok: Option<Option<ResultDropFn>>,
}

const TAIL: usize = std::mem::size_of::<Tail>().next_multiple_of(16);

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
    if t.done {
        return READY;
    }
    let dst = (f as *mut u8).add(FUT_RESULT_OFFSET);
    let mut i = 0;
    while i < t.children.len() {
        let c = t.children[i];
        if ((*c).poll.0)(c, cx) != READY {
            i += 1;
            continue;
        }
        let src = (c as *mut u8).add(FUT_RESULT_OFFSET);
        t.children.swap_remove(i);
        if let (Some(drop_fn), true) = (t.first_ok, *src != 0 && !t.children.is_empty()) {
            if let Some(d) = drop_fn {
                d(src);
            }
            ((*c).drop.0)(c);
            continue;
        }
        std::ptr::copy_nonoverlapping(src, dst, t.result_size as usize);
        ((*c).drop.0)(c);
        for c in std::mem::take(&mut t.children) {
            ((*c).drop.0)(c);
        }
        t.done = true;
        return READY;
    }
    PENDING // also `Promise.race([])`, which never settles
}

unsafe extern "C" fn race_drop(f: *mut VeltFut) {
    let t = tail(f);
    for c in std::mem::take(&mut t.children) {
        ((*c).drop.0)(c);
    }
    let l = layout(t.result_size);
    std::ptr::drop_in_place(t);
    std::alloc::dealloc((f as *mut u8).sub(TAIL), l);
}

unsafe fn new_race(
    futs: *const Wide<*mut VeltFut>,
    n: u64,
    result_size: u64,
    first_ok: Option<Option<ResultDropFn>>,
) -> *mut VeltFut {
    let l = layout(result_size);
    let base = std::alloc::alloc(l);
    if base.is_null() {
        std::alloc::handle_alloc_error(l);
    }
    (base as *mut Tail).write(Tail {
        children: (0..n as usize).map(|i| (*futs.add(i)).0).collect(),
        result_size,
        done: false,
        first_ok,
    });
    let f = base.add(TAIL) as *mut VeltFut;
    f.write(VeltFut::new(race_poll, race_drop));
    f
}

/// `Promise.race(array)`: takes the `n` futures (not the pointer array); the first to finish
/// moves its `result_size`-byte result to the returned future's slot.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_race(
    futs: *const Wide<*mut VeltFut>,
    n: u64,
    result_size: u64,
) -> *mut VeltFut {
    new_race(futs, n, result_size, None)
}

/// `Promise.any(array)`: the first fulfilled child wins (see the module docs).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_race_ok(
    futs: *const Wide<*mut VeltFut>,
    n: u64,
    result_size: u64,
    reject_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    new_race(futs, n, result_size, Some(reject_drop))
}
