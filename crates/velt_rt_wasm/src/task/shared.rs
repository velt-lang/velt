//! Shared promises on the current-thread executor (see velt_rt's task/shared.rs): every holder
//! holds the same `VeltFut*`; the first share moves its header entries to a side record (the
//! hub) that counts the holders, keeps the settled result and lists the waiting holders. The
//! first waiter drives the original future; when it settles, the others are woken in the order
//! they started waiting, and each delivery writes a fresh share of the result into the slot.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::rc::Rc;
use std::task::Waker;

use super::all::ResultDropFn;
use super::{context, FutDropFn, FutPollFn, VeltFut, Wide, FUT_RESULT_OFFSET, PENDING, READY};

/// Compiled share glue of a result slot: writes another reference to `*src` into `dst`.
pub type ShareFn = unsafe extern "C" fn(src: *const u8, dst: *mut u8);

struct Hub {
    poll: FutPollFn,
    drop: FutDropFn,
    size: usize,
    share: Option<ShareFn>,
    result_drop: Option<ResultDropFn>,
    holders: usize,
    done: bool,
    result: Vec<u128>,
    waiters: Vec<Waker>,
}

thread_local! {
    static HUBS: RefCell<HashMap<usize, Rc<RefCell<Hub>>>> = RefCell::new(HashMap::new());
}

fn hub(f: *mut VeltFut) -> Rc<RefCell<Hub>> {
    HUBS.with(|m| m.borrow().get(&(f as usize)).cloned())
        .unwrap_or_else(|| crate::panic::fatal("internal error: a shared promise lost its record"))
}

/// Is `f` a shared future?
///
/// # Safety
/// `f` must be a live heap future.
pub(crate) unsafe fn is_shared(f: *mut VeltFut) -> bool {
    std::ptr::fn_addr_eq((*f).poll.0, shared_poll as FutPollFn)
}

/// Another holder of the promise `f` (null: none): `f` itself, now shared.
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
        hub(f).borrow_mut().holders += 1;
        return f;
    }
    let h = Hub {
        poll: (*f).poll.0,
        drop: (*f).drop.0,
        size: result_size as usize,
        share,
        result_drop,
        holders: 2,
        done: false,
        result: Vec::new(),
        waiters: Vec::new(),
    };
    HUBS.with(|m| m.borrow_mut().insert(f as usize, Rc::new(RefCell::new(h))));
    (*f).poll = Wide(shared_poll);
    (*f).drop = Wide(shared_drop);
    f
}

unsafe fn slot(f: *mut VeltFut) -> *mut u8 {
    (f as *mut u8).add(FUT_RESULT_OFFSET)
}

unsafe extern "C" fn shared_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let h = hub(f);
    let w = context(cx).waker().clone();
    if !h.borrow().done {
        let first = {
            let mut s = h.borrow_mut();
            match s.waiters.iter().position(|x| x.will_wake(&w)) {
                Some(i) => i == 0,
                None => {
                    s.waiters.push(w.clone());
                    s.waiters.len() == 1
                }
            }
        };
        let poll = h.borrow().poll;
        if !first || poll(f, cx) != READY {
            return PENDING;
        }
        let others = {
            let mut s = h.borrow_mut();
            let mut result = vec![0u128; s.size.div_ceil(16)];
            std::ptr::copy_nonoverlapping(slot(f), result.as_mut_ptr() as *mut u8, s.size);
            s.result = result;
            s.done = true;
            std::mem::take(&mut s.waiters)
        };
        for o in others.into_iter().skip(1) {
            o.wake();
        }
    }
    let (share, size, result) = {
        let s = h.borrow();
        (s.share, s.size, s.result.as_ptr() as *const u8)
    };
    match share {
        Some(share) => share(result, slot(f)),
        None => std::ptr::copy_nonoverlapping(result, slot(f), size),
    }
    READY
}

unsafe extern "C" fn shared_drop(f: *mut VeltFut) {
    let h = hub(f);
    let mut s = h.borrow_mut();
    s.holders -= 1;
    if s.holders > 0 {
        let waiters = if s.done {
            vec![]
        } else {
            std::mem::take(&mut s.waiters)
        };
        drop(s);
        for w in waiters {
            w.wake();
        }
        return;
    }
    let (done, mut result, poll, drop_fn, result_drop) = (
        s.done,
        std::mem::take(&mut s.result),
        s.poll,
        s.drop,
        s.result_drop,
    );
    drop(s);
    HUBS.with(|m| m.borrow_mut().remove(&(f as usize)));
    if done {
        if let Some(d) = result_drop {
            d(result.as_mut_ptr() as *mut u8);
        }
    }
    (*f).poll = Wide(poll);
    (*f).drop = Wide(drop_fn);
    drop_fn(f);
}

/// `velt_rt_fut_peek` of a shared future: its settled result, copied into the slot for the
/// formatter to read (it claims nothing).
///
/// # Safety
/// `f` must be a live shared future.
pub(crate) unsafe fn peek(f: *mut VeltFut) -> bool {
    let h = hub(f);
    let s = h.borrow();
    if s.done {
        std::ptr::copy_nonoverlapping(s.result.as_ptr() as *const u8, slot(f), s.size);
    }
    s.done
}
