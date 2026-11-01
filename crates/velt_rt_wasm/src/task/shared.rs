//! Shared promises on the current-thread executor (see velt_rt's task/shared.rs): the first
//! share moves the original future's header entries to a side record (the hub) that counts the
//! holders, keeps the settled result and lists the waiting holders; every other holder gets a
//! handle of its own (the hub in front of its header). The first waiter drives the original
//! future; when it settles, the others are woken in the order they started waiting, and each
//! delivery writes a fresh share of the result into the holder's slot.
//!
//! A shared promise stays on its logical task, as on native targets: the tasks share the one
//! thread here, but a holder that goes to another task (`spawn`, a channel, a timer callback)
//! is handed over the same way, and is shared again there as a promise of that task, so two
//! tasks never use one result object (docs/reference/async.md).

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
    /// The logical task the promise is shared on (`executor::logical_task`; 0: not known yet).
    task: u64,
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

/// The hub of holder `f`: an original future is always in `HUBS` (by its address), a handle
/// never is (its hub is in front of its header). The header's `poll` only says whether `f` is
/// shared at all, so a linker that folds functions cannot make one holder read as the other.
unsafe fn hub(f: *mut VeltFut) -> HubRef {
    match orig_hub(f) {
        Some(h) => h,
        None => handle_hub(f),
    }
}

/// The hub of handle `f`, kept in front of its header.
unsafe fn handle_hub(f: *mut VeltFut) -> HubRef {
    (*((f as *mut u8).sub(FRONT) as *const HubRef)).clone()
}

/// The hub of `f` if it is an original future (by its address).
fn orig_hub(f: *mut VeltFut) -> Option<HubRef> {
    HUBS.with(|m| m.borrow().get(&(f as usize)).cloned())
}

/// Is `f` a holder of a shared promise (its original future or a handle)?
///
/// # Safety
/// `f` must be a live heap future.
pub(crate) unsafe fn is_shared(f: *mut VeltFut) -> bool {
    std::ptr::fn_addr_eq((*f).poll.0, holder_poll as FutPollFn)
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
    if let Some((p, _)) = forwarded(f) {
        // Handed to this task and shared again here: its proxy becomes the promise shared on
        // this task (`f` goes on forwarding to it).
        return velt_rt_fut_share(p, result_size, share, result_drop);
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
            task: super::executor::logical_task(),
            holders: 1,
            done: false,
            result: Vec::new(),
            waiters: Vec::new(),
        }));
        HUBS.with(|m| m.borrow_mut().insert(f as usize, h.clone()));
        (*f).poll = Wide(holder_poll);
        (*f).drop = Wide(holder_drop);
        h
    };
    on_its_task(&h);
    new_handle(h)
}

/// What promise holder `f` stands for (task/identity.rs): `Ok(orig)`, the original future of
/// its shared promise; `Err(proxy)`, the proxy of a holder handed to this task; `None` when `f`
/// is not a holder.
///
/// # Safety
/// `f` must be a live heap future.
pub(super) unsafe fn origin(f: *mut VeltFut) -> Option<Result<*mut VeltFut, *mut VeltFut>> {
    if !is_shared(f) {
        return None;
    }
    if let Some((p, _)) = forwarded(f) {
        return Some(Err(p));
    }
    let orig = hub(f).borrow().orig;
    Some(Ok(orig))
}

/// Another holder of shared promise `f` (`f` is a holder, not handed over).
///
/// # Safety
/// `f` must be a live holder of a shared promise, not handed to another task.
pub(super) unsafe fn another_holder(f: *mut VeltFut) -> *mut VeltFut {
    let h = hub(f);
    on_its_task(&h);
    new_handle(h)
}

/// A new holder of the promise of `h`: a handle of its own.
unsafe fn new_handle(h: HubRef) -> *mut VeltFut {
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
    g.write(VeltFut::new(holder_poll, holder_drop));
    g
}

unsafe fn slot(f: *mut VeltFut) -> *mut u8 {
    (f as *mut u8).add(FUT_RESULT_OFFSET)
}

/// `poll` of every holder, the original future and the handles alike.
unsafe extern "C" fn holder_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    if let Some((p, size)) = forwarded(f) {
        if super::velt_rt_fut_poll(p, cx) != READY {
            return PENDING;
        }
        std::ptr::copy_nonoverlapping(slot(p), slot(f), size);
        return READY;
    }
    poll_holder(&hub(f), f, cx)
}

/// A holder handed to another task ([`hand_over`]).
struct Forward {
    /// The started promise of the promise's own task that awaits it there and delivers a
    /// transferred result.
    proxy: *mut VeltFut,
    size: usize,
    /// The holder is the original future (not a handle).
    is_orig: bool,
    /// For an original future: its own entries, once its hub released everything else.
    orig: Option<(FutPollFn, FutDropFn)>,
}

thread_local! {
    /// The holders handed to another task, by address.
    static FORWARDS: RefCell<HashMap<usize, Forward>> = RefCell::new(HashMap::new());
}

fn forwarded(f: *mut VeltFut) -> Option<(*mut VeltFut, usize)> {
    FORWARDS.with(|m| m.borrow().get(&(f as usize)).map(|w| (w.proxy, w.size)))
}

/// The owner hands holder `f` to another task (velt_rt's task/shared.rs `hand_over`): its
/// holder role goes to a started promise of this task, which awaits the promise here and
/// finishes with a share of the result that `transfer` moves or copies for the other task, so
/// two tasks never use one result object, as on native targets.
///
/// # Safety
/// `f` must be a live holder of a shared promise, owned by the caller; `transfer` must match
/// its result.
pub(crate) unsafe fn hand_over(f: *mut VeltFut, transfer: Option<ResultDropFn>) {
    if let Some((p, _)) = forwarded(f) {
        if !is_shared(p) {
            // Handed on again: the proxy's result is this holder's own copy already.
            return;
        }
        // Shared again on this task: a proxy of this task takes `f`'s role over in turn.
        let next = new_proxy(hub(p), transfer);
        FORWARDS.with(|m| {
            if let Some(w) = m.borrow_mut().get_mut(&(f as usize)) {
                w.proxy = next;
            }
        });
        super::velt_rt_fut_drop(p);
        return;
    }
    let h = hub(f);
    on_its_task(&h);
    let size = h.borrow().size;
    let proxy = new_proxy(h.clone(), transfer);
    let is_orig = orig_hub(f).is_some();
    FORWARDS.with(|m| {
        m.borrow_mut().insert(
            f as usize,
            Forward {
                proxy,
                size,
                is_orig,
                orig: None,
            },
        )
    });
    release(h, f);
}

/// A started promise of this task holding a role of its own in the promise of `h`, delivering
/// a share of the result transferred with `transfer` (null: copied as it is).
unsafe fn new_proxy(h: HubRef, transfer: Option<ResultDropFn>) -> *mut VeltFut {
    on_its_task(&h);
    let (size, result_drop) = {
        let s = h.borrow();
        (s.size, s.result_drop)
    };
    let role = new_handle(h);
    let state_size = size.max(8).next_multiple_of(16);
    let mut state = vec![0u128; state_size / 16];
    (state.as_mut_ptr() as *mut *mut VeltFut).write(role);
    let proxy = super::boxed::velt_rt_fut_box(
        proxy_poll,
        proxy_drop,
        state.as_ptr() as *const u8,
        state_size as u64,
        16,
    );
    super::local::velt_rt_fut_start(proxy, result_drop);
    super::local::velt_rt_fut_transfer(proxy, transfer);
    proxy
}

unsafe extern "C" fn proxy_poll(state: *mut u8, cx: *mut c_void) -> u32 {
    let role = *(state as *mut *mut VeltFut);
    if super::velt_rt_fut_poll(role, cx) != READY {
        return PENDING;
    }
    let size = handle_hub(role).borrow().size;
    std::ptr::copy_nonoverlapping(slot(role), state, size);
    super::velt_rt_fut_drop(role);
    READY
}

unsafe extern "C" fn proxy_drop(state: *mut u8) {
    super::velt_rt_fut_drop(*(state as *mut *mut VeltFut));
}

/// `drop` of every holder: a handle also frees its own block.
unsafe extern "C" fn holder_drop(f: *mut VeltFut) {
    if let Some(fwd) = FORWARDS.with(|m| m.borrow_mut().remove(&(f as usize))) {
        super::velt_rt_fut_drop(fwd.proxy);
        match fwd.orig {
            Some((poll, orig_drop)) => {
                (*f).poll = Wide(poll);
                (*f).drop = Wide(orig_drop);
                orig_drop(f);
            }
            None if fwd.is_orig => {}
            None => {
                let base = (f as *mut u8).sub(FRONT);
                let h = (base as *mut HubRef).read();
                let l = handle_layout(h.borrow().size);
                drop(h);
                std::alloc::dealloc(base, l);
            }
        }
        return;
    }
    if let Some(h) = orig_hub(f) {
        return release(h, f);
    }
    let base = (f as *mut u8).sub(FRONT);
    let h = (base as *mut HubRef).read();
    let l = handle_layout(h.borrow().size);
    release(h, f);
    std::alloc::dealloc(base, l);
}

/// Stop the program when the promise of `h` is used on another logical task than the one it is
/// shared on (velt_rt's task/shared.rs `on_its_task`). Outside a task anything goes, and so it
/// does in a program with module state: a holder reached through it may move between the tasks
/// that use module state, which velt_rt runs one at a time under its module lock.
fn on_its_task(h: &HubRef) {
    let now = super::executor::logical_task();
    if now == 0 {
        return;
    }
    let mut s = h.borrow_mut();
    if s.task == 0 {
        s.task = now;
    } else if s.task != now && !super::local::module_state_used() {
        crate::panic::fatal(
            "a promise shared on one task was used on another without being handed over (#212)",
        );
    }
}

unsafe fn poll_holder(h: &HubRef, f: *mut VeltFut, cx: *mut c_void) -> u32 {
    on_its_task(h);
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
    on_its_task(&h);
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
    let kept = FORWARDS.with(|m| match m.borrow_mut().get_mut(&(orig as usize)) {
        Some(w) => {
            w.orig = Some((poll, drop_fn));
            true
        }
        None => false,
    });
    if kept {
        // Handed to another task: the holder there drops it.
        return;
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
    if let Some((p, size)) = forwarded(f) {
        if super::local::velt_rt_fut_peek(p) == 0 {
            return false;
        }
        std::ptr::copy_nonoverlapping(slot(p), slot(f), size);
        return true;
    }
    let h = hub(f);
    on_its_task(&h);
    let s = h.borrow();
    if s.done {
        std::ptr::copy_nonoverlapping(s.result.as_ptr() as *const u8, slot(f), s.size);
    }
    s.done
}
