//! Shared promises on the current-thread executor (see velt_rt's task/shared.rs): the first
//! share moves the original future's header entries to a side record (the hub) that counts the
//! holders, keeps the settled result and lists the waiting holders; every other holder gets a
//! handle of its own (the hub in front of its header). The first waiter drives the original
//! future; when it settles, the others are woken in the order they started waiting, and each
//! delivery writes a fresh share of the result into the holder's slot.

use std::alloc::Layout;
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
    orig: *mut VeltFut,
    poll: FutPollFn,
    drop: FutDropFn,
    size: usize,
    share: Option<ShareFn>,
    result_drop: Option<ResultDropFn>,
    holders: usize,
    done: bool,
    result: Vec<u128>,
    waiters: Vec<(usize, Waker)>,
}

type HubRef = Rc<RefCell<Hub>>;

thread_local! {
    static HUBS: RefCell<HashMap<usize, HubRef>> = RefCell::new(HashMap::new());
}

/// Bytes in front of a handle's header: its hub.
const FRONT: usize = 16;

fn handle_layout(size: usize) -> Layout {
    Layout::from_size_align(FRONT + FUT_RESULT_OFFSET + size, 16)
        .unwrap_or_else(|_| crate::panic::fatal("invalid shared promise size"))
}

unsafe fn is_handle(f: *mut VeltFut) -> bool {
    std::ptr::fn_addr_eq((*f).poll.0, handle_poll as FutPollFn)
}

unsafe fn hub(f: *mut VeltFut) -> HubRef {
    if is_handle(f) {
        return (*((f as *mut u8).sub(FRONT) as *const HubRef)).clone();
    }
    HUBS.with(|m| m.borrow().get(&(f as usize)).cloned())
        .unwrap_or_else(|| crate::panic::fatal("internal error: a shared promise lost its record"))
}

/// Is `f` a holder of a shared promise (its original future or a handle)?
///
/// # Safety
/// `f` must be a live heap future.
pub(crate) unsafe fn is_shared(f: *mut VeltFut) -> bool {
    is_handle(f) || std::ptr::fn_addr_eq((*f).poll.0, orig_poll as FutPollFn)
}

/// Another holder of the promise `f` (null: none): a handle of its own.
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
        let h = Rc::new(RefCell::new(Hub {
            orig: f,
            poll: (*f).poll.0,
            drop: (*f).drop.0,
            size: result_size as usize,
            share,
            result_drop,
            holders: 1,
            done: false,
            result: Vec::new(),
            waiters: Vec::new(),
        }));
        HUBS.with(|m| m.borrow_mut().insert(f as usize, h.clone()));
        (*f).poll = Wide(orig_poll);
        (*f).drop = Wide(orig_drop);
        h
    };
    let size = {
        let mut s = h.borrow_mut();
        s.holders += 1;
        s.size
    };
    let l = handle_layout(size);
    let base = std::alloc::alloc(l);
    if base.is_null() {
        std::alloc::handle_alloc_error(l);
    }
    (base as *mut HubRef).write(h);
    let g = base.add(FRONT) as *mut VeltFut;
    g.write(VeltFut::new(handle_poll, handle_drop));
    g
}

unsafe fn slot(f: *mut VeltFut) -> *mut u8 {
    (f as *mut u8).add(FUT_RESULT_OFFSET)
}

unsafe extern "C" fn orig_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    poll_holder(&hub(f), f, cx)
}

unsafe extern "C" fn handle_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    poll_holder(&hub(f), f, cx)
}

unsafe extern "C" fn orig_drop(f: *mut VeltFut) {
    release(hub(f), f);
}

unsafe extern "C" fn handle_drop(f: *mut VeltFut) {
    let base = (f as *mut u8).sub(FRONT);
    let h = (base as *mut HubRef).read();
    let l = handle_layout(h.borrow().size);
    release(h, f);
    std::alloc::dealloc(base, l);
}

unsafe fn poll_holder(h: &HubRef, f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let me = f as usize;
    let w = context(cx).waker().clone();
    if !h.borrow().done {
        let first = {
            let mut s = h.borrow_mut();
            match s.waiters.iter().position(|(x, _)| *x == me) {
                Some(i) => {
                    s.waiters[i].1 = w.clone();
                    i == 0
                }
                None => {
                    s.waiters.push((me, w.clone()));
                    s.waiters.len() == 1
                }
            }
        };
        let (poll, orig) = {
            let s = h.borrow();
            (s.poll, s.orig)
        };
        if !first || poll(orig, cx) != READY {
            return PENDING;
        }
        let others = {
            let mut s = h.borrow_mut();
            let mut result = vec![0u128; s.size.div_ceil(16)];
            std::ptr::copy_nonoverlapping(slot(orig), result.as_mut_ptr() as *mut u8, s.size);
            s.result = result;
            s.done = true;
            std::mem::take(&mut s.waiters)
        };
        for (_, o) in others.into_iter().skip(1) {
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

unsafe fn release(h: HubRef, f: *mut VeltFut) {
    let me = f as usize;
    let mut s = h.borrow_mut();
    s.holders -= 1;
    if s.holders > 0 {
        let Some(i) = s.waiters.iter().position(|(x, _)| *x == me) else {
            return;
        };
        s.waiters.remove(i);
        let wake: Vec<Waker> = match s.done {
            true => vec![],
            false => s.waiters.iter().map(|(_, w)| w.clone()).collect(),
        };
        drop(s);
        for w in wake {
            w.wake();
        }
        return;
    }
    let (done, mut result) = (s.done, std::mem::take(&mut s.result));
    let (orig, poll, drop_fn, result_drop) = (s.orig, s.poll, s.drop, s.result_drop);
    drop(s);
    HUBS.with(|m| m.borrow_mut().remove(&(orig as usize)));
    if done {
        if let Some(d) = result_drop {
            d(result.as_mut_ptr() as *mut u8);
        }
    }
    (*orig).poll = Wide(poll);
    (*orig).drop = Wide(drop_fn);
    drop_fn(orig);
}

/// `velt_rt_fut_peek` of a holder of a shared promise: its settled result, copied into the
/// holder's slot for the formatter to read (it claims nothing).
///
/// # Safety
/// `f` must be a live holder of a shared promise.
pub(crate) unsafe fn peek(f: *mut VeltFut) -> bool {
    let h = hub(f);
    let s = h.borrow();
    if s.done {
        std::ptr::copy_nonoverlapping(s.result.as_ptr() as *const u8, slot(f), s.size);
    }
    s.done
}
