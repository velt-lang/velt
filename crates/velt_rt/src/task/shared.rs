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
//! A started original future (the usual case, `const p = f()` in a task) is driven by its task's
//! set; the hub awaits it with a waker of its own, which fires as it finishes. Then every
//! waiting holder is queued in the task's microtask queue, in the order they started waiting
//! (`local::settled`, as a `resolve` queues the waiters of a latch), one entry each: Node queues
//! a promise's reactions in that order when it settles, so a combinator over a holder (a
//! `Promise.race`) sees it settle before a promise that settles later. Any other original (a
//! lazy one, a runtime leaf) is driven by the first waiter with its own context; when it settles
//! there, that waiter goes on as an unshared promise's awaiter would, and the others are queued.
//!
//! Dropping a holder never cancels the promise (JS never does). A waiting holder that is dropped
//! wakes the others, so none waits for a driver that went away. The last holder releases the
//! settled result, or, when the promise has not settled, gives the original future back its
//! entries and drops it: it then runs to completion and its own result drop disposes of the
//! result (an unhandled rejection is reported) as for any dropped promise.
//!
//! A shared promise stays on the task it is shared on: a holder that goes to another task is
//! handed over ([`hand_over`]), and a holder handed over is shared again there as a promise of
//! that task (`velt_rt_fut_share` of its proxy).

use std::alloc::Layout;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Wake, Waker};

use parking_lot::Mutex;

use crate::panic::ThrowLoc;

use super::all::ResultDropFn;
use super::{context, raw_cx, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};

/// Compiled share glue of a result slot: writes another reference to `*src` into `dst`.
pub type ShareFn = unsafe extern "C" fn(src: *const u8, dst: *mut u8);

type FutPoll = unsafe extern "C" fn(f: *mut VeltFut, cx: *mut c_void) -> u32;
type FutDrop = unsafe extern "C" fn(f: *mut VeltFut);

/// The side record of a shared future.
pub(super) struct Hub {
    /// The original future (its first holder's pointer), and its own entries.
    orig: *mut VeltFut,
    poll: FutPoll,
    drop: FutDrop,
    size: usize,
    share: Option<ShareFn>,
    result_drop: Option<ResultDropFn>,
    /// `orig` is a started promise, which its task's set drives (module docs).
    node: bool,
    /// The task the promise is shared on (`local::task_key`; 0: not known yet).
    task: AtomicU64,
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
    /// The hub's waker is registered with a started original (`node`).
    armed: bool,
    /// When it settled, in the order promises settle ([`SETTLES`]; 0 while pending).
    seq: u64,
    /// The settled result (16-byte aligned words), once `done`.
    result: Vec<u128>,
    /// Where its error was thrown, for every holder that takes it (panic.rs).
    loc: ThrowLoc,
    /// The waiting holders (by address) in the order they started waiting, with their wakers
    /// and the microtask ticket each was queued as when the promise settled (0: not queued).
    waiters: Vec<(usize, Waker, u64)>,
    /// A holder that is not a converted promise's view took the settled result (identity.rs):
    /// someone handled the promise.
    claimed: bool,
}

/// The hubs of the shared original futures, by address.
static HUBS: Mutex<Option<HashMap<usize, Arc<Hub>>>> = Mutex::new(None);

/// Counts the settles of shared promises: a race over holders takes the first that settled.
static SETTLES: AtomicU64 = AtomicU64::new(1);

/// When holder `f` settled (the order of [`SETTLES`]), if it did; `None` for any other future
/// or a holder handed to another task (`Promise.race` over holders, race.rs).
///
/// # Safety
/// `f` must be a live heap future.
pub(crate) unsafe fn settled_at(f: *mut VeltFut) -> Option<u64> {
    if !is_shared(f) || forwarded(f).is_some() {
        return None;
    }
    let st = hub(f).st.lock().seq;
    (st != 0).then_some(st)
}

/// Bytes in front of a handle's header: its hub, then whether it is a view (identity.rs).
const FRONT: usize = 16;

std::thread_local! {
    /// The handles made now are views: holders a converted promise awaits its promise through
    /// (identity.rs), whose taking the result handles nothing of its own.
    static VIEWS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Make the handles made from now on views (`on`), or holders again.
pub(super) fn set_views(on: bool) {
    VIEWS.with(|c| c.set(on));
}

/// Is handle (not original future) `f` a view?
unsafe fn is_view(f: *mut VeltFut) -> bool {
    *((f as *mut u8).sub(FRONT - 8) as *const u64) != 0
}

/// A shared promise's record, kept by a converted promise (identity.rs).
pub(super) type HubRef = Arc<Hub>;

/// Did a holder of the promise of `h` that is not a view take its settled result?
pub(super) fn claimed(h: &HubRef) -> bool {
    h.st.lock().claimed
}

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
    if let Some((p, _)) = forwarded(f) {
        // Handed to this task and shared again here: its proxy, which delivers this task's own
        // copy, becomes the promise shared on this task (`f` goes on forwarding to it).
        return velt_rt_fut_share(p, result_size, share, result_drop);
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
            node: super::local::is_started_node(f),
            task: AtomicU64::new(super::local::task_key()),
            st: Mutex::new(HubState {
                holders: 1,
                done: false,
                armed: false,
                seq: 0,
                result: Vec::new(),
                loc: ThrowLoc::NONE,
                waiters: Vec::new(),
                claimed: false,
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
    Some(Ok(hub(f).orig))
}

/// Another holder of shared promise `f` (`f` is a holder, not handed over): a handle of its
/// own, needing no result glue, and the promise's record.
///
/// # Safety
/// `f` must be a live holder of a shared promise, not handed to another task.
pub(super) unsafe fn another_holder(f: *mut VeltFut) -> (*mut VeltFut, HubRef) {
    let h = hub(f);
    on_its_task(&h);
    (new_handle(h.clone()), h)
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
    (base.add(FRONT - 8) as *mut u64).write(VIEWS.with(std::cell::Cell::get) as u64);
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
    if let Some((p, size)) = forwarded(f) {
        if super::velt_rt_fut_poll(p, cx) != READY {
            return PENDING;
        }
        std::ptr::copy_nonoverlapping(slot(p), slot(f), size);
        return READY;
    }
    poll_holder(&hub(f), f, cx)
}

/// `drop` of every holder: a handle also frees its own block.
unsafe extern "C" fn holder_drop(f: *mut VeltFut) {
    let fwd = FORWARDS
        .lock()
        .as_mut()
        .and_then(|m| m.remove(&(f as usize)));
    if let Some(fwd) = fwd {
        // Handed to this task: its holder role went to the proxy on the promise's own task.
        super::velt_rt_fut_drop(fwd.proxy);
        match fwd.orig.into_inner() {
            Some((poll, orig_drop)) => {
                // The original future outlived its hub for this pointer (`release`).
                (*f).poll = poll;
                (*f).drop = orig_drop;
                orig_drop(f);
            }
            // The hub still has the original future: its last holder drops it.
            None if fwd.is_orig => {}
            None => free_handle(f),
        }
        return;
    }
    if let Some(h) = orig_hub(f) {
        return release(h, f);
    }
    let base = (f as *mut u8).sub(FRONT);
    let h = (base as *mut Arc<Hub>).read();
    let l = handle_layout(h.size);
    release(h, f);
    std::alloc::dealloc(base, l);
}

/// Free handle `f`'s block (its holder role was released already).
unsafe fn free_handle(f: *mut VeltFut) {
    let base = (f as *mut u8).sub(FRONT);
    let h = (base as *mut Arc<Hub>).read();
    let l = handle_layout(h.size);
    drop(h);
    std::alloc::dealloc(base, l);
}

/// A holder handed to another task ([`hand_over`]).
struct Forward {
    /// The started promise on the promise's own task that awaits it there and delivers a
    /// transferred result.
    proxy: *mut VeltFut,
    size: usize,
    /// `f` is the original future (not a handle).
    is_orig: bool,
    /// For an original future: its own entries, once its hub released everything else
    /// (`release`); the holder that went away then drops it.
    orig: std::cell::Cell<Option<(FutPoll, FutDrop)>>,
}

// SAFETY: a forward is used by the task holding the holder, and by `release` on the promise's
// own task, under the lock of `FORWARDS`.
unsafe impl Send for Forward {}

/// The holders handed to another task, by address.
static FORWARDS: Mutex<Option<HashMap<usize, Forward>>> = Mutex::new(None);

/// The proxy and result size of holder `f` if it was handed to another task.
fn forwarded(f: *mut VeltFut) -> Option<(*mut VeltFut, usize)> {
    FORWARDS
        .lock()
        .as_ref()
        .and_then(|m| m.get(&(f as usize)))
        .map(|w| (w.proxy, w.size))
}

/// The owner hands holder `f` to another task (`velt_rt_fut_transfer`: `spawn`, a channel, a
/// closure capturing it). Its holder role goes to a started promise of this task, the proxy,
/// which awaits the promise here and finishes with a share of the result; `transfer` (compiled
/// transfer glue) then moves or copies that for the other task, as for any promise handed over
/// (#160). `f` forwards to the proxy from then on: the other task awaits a copy made here and
/// never uses the result object this task's holders see.
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
        // Shared again on this task (`velt_rt_fut_share`): `f` is a holder of its proxy's
        // promise here, so a proxy of this task takes that role over in turn.
        let next = new_proxy(hub(p), transfer);
        if let Some(w) = FORWARDS
            .lock()
            .as_mut()
            .and_then(|m| m.get_mut(&(f as usize)))
        {
            w.proxy = next;
        }
        super::velt_rt_fut_drop(p);
        return;
    }
    let h = hub(f);
    on_its_task(&h);
    let size = h.size;
    let proxy = match super::local::task_key() {
        // Outside a task (a synchronous `main`): the proxy is a promise of the task that runs
        // what code there hands over, so every holder handed over is one of the same task.
        0 => {
            let mut proxy = std::ptr::null_mut();
            let hh = h.clone();
            super::local::in_outside_home(|| proxy = new_proxy(hh, transfer));
            proxy
        }
        _ => new_proxy(h.clone(), transfer),
    };
    FORWARDS.lock().get_or_insert_with(HashMap::new).insert(
        f as usize,
        Forward {
            proxy,
            size,
            is_orig: orig_hub(f).is_some(),
            orig: std::cell::Cell::new(None),
        },
    );
    // `f`'s own role ends here (the proxy's keeps the promise alive); an original future stays
    // allocated for `f` until it is dropped.
    release(h, f);
}

/// A started promise of this task holding a role of its own in the promise of `h`: it awaits
/// the promise here and finishes with a share of the result, transferred with `transfer` (null:
/// a plain result, copied as it is) for the task it goes to.
unsafe fn new_proxy(h: Arc<Hub>, transfer: Option<ResultDropFn>) -> *mut VeltFut {
    on_its_task(&h);
    let size = h.size;
    let result_drop = h.result_drop;
    let role = new_handle(h);
    // The proxy's state: the role while it waits, the result (at offset 0) once it settled.
    let state_size = size.max(8).next_multiple_of(16);
    let mut state = vec![0u128; state_size / 16];
    (state.as_mut_ptr() as *mut *mut VeltFut).write(role);
    let proxy = super::local::velt_rt_fut_box(
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

/// Poll of a proxy ([`hand_over`]): awaits its role, then moves the result into its state.
unsafe extern "C" fn proxy_poll(state: *mut u8, cx: *mut c_void) -> u32 {
    let role = *(state as *mut *mut VeltFut);
    if super::velt_rt_fut_poll(role, cx) != READY {
        return PENDING;
    }
    let size = handle_hub(role).size;
    std::ptr::copy_nonoverlapping(slot(role), state, size);
    super::velt_rt_fut_drop(role);
    READY
}

/// Drop of a proxy that has not settled: its role goes away.
unsafe extern "C" fn proxy_drop(state: *mut u8) {
    super::velt_rt_fut_drop(*(state as *mut *mut VeltFut));
}

/// Stop the program when the promise of `h` is used on another task than the one it is shared
/// on without being handed over (`hand_over`): its original future runs on that task, and its
/// result would be used from two threads. Outside a task (a synchronous `main`, or the runtime
/// dropping what is left at exit) anything goes.
///
/// So it does for a thread holding the module lock, and that is all this tests. It is sound
/// because the task the promise is shared on holds the lock whenever it runs too: a holder
/// reaches module state (an in-flight `Map<string, Promise<T>>`) only through code holding the
/// lock, so that task put it there or first used it from there and is a touched task from then
/// on, whose every poll takes the lock (local/module_state.rs); the outside home, where code
/// outside a task hands holders over, is a touched task in every program with module state
/// (`local::in_outside_home`). So the tasks using the holder, its result and what that reaches
/// never run at once.
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
        Err(_) if super::local::module_held() => {}
        Err(_) => crate::panic::fatal(
            "a promise shared on one task was used on another without being handed over (#212)",
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
        if h.node {
            // Its set drives it; the hub hears when it finishes ([`settle`]).
            if st.armed {
                return PENDING;
            }
            st.armed = true;
            drop(st);
            let hw = Waker::from(h.clone());
            let mut c = Context::from_waker(&hw);
            if !super::local::await_unadopted(h.orig, raw_cx(&mut c).ptr()) {
                return PENDING;
            }
            // It had finished already: this waiter settles it now, unless the hub's waker did
            // meanwhile, on the promise's task ([`settle`]).
            st = h.st.lock();
        } else {
            if !first {
                return PENDING;
            }
            drop(st);
            // The first waiter drives the original future (no lock held: it runs compiled
            // code).
            if (h.poll)(h.orig, cx) != READY {
                return PENDING;
            }
            st = h.st.lock();
        }
    }
    if !st.done {
        keep_result(h, &mut st);
        // The waiters resume in the order they started waiting. This one, when it is the
        // first, goes on now when its `await` would have (as an unshared promise's awaiter
        // does), ahead of the others it queues; otherwise it is queued in its place.
        let mut waiters = std::mem::take(&mut st.waiters);
        drop(st);
        let now = waiters.first().is_some_and(|(x, ..)| *x == me) && super::local::step_free();
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
    if f == h.orig || !is_view(f) {
        st.claimed = true;
    }
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

/// The original future of `h` finished: keep its result in the hub (`st` is its locked state, not
/// settled yet). The result of a started original is claimed here, under the lock, so the hub's
/// waker on the promise's task and a holder polled on another (module state) never claim it
/// both.
unsafe fn keep_result(h: &Hub, st: &mut HubState) {
    // A started original keeps where it threw; one its first waiter drove threw on this thread.
    st.loc = if h.node {
        super::local::deliver_unadopted(h.orig);
        super::local::node_loc(h.orig)
    } else {
        ThrowLoc::current()
    };
    let mut result = vec![0u128; h.size.div_ceil(16)];
    std::ptr::copy_nonoverlapping(slot(h.orig), result.as_mut_ptr() as *mut u8, h.size);
    st.result = result;
    st.done = true;
    st.seq = SETTLES.fetch_add(1, Ordering::Relaxed);
}

/// The hub's waker, registered with a started original future ([`poll_holder`]): it fires as
/// the original finishes, on its task.
impl Wake for Hub {
    fn wake(self: Arc<Self>) {
        // SAFETY: `orig` stays allocated while the hub has holders (checked under the lock).
        unsafe { settle(&self) }
    }
}

/// The started original of `h` finished (or its task went away): keep the result and queue
/// every waiting holder in the order they started waiting, as Node queues a promise's
/// reactions when it settles (module docs).
unsafe fn settle(h: &Arc<Hub>) {
    let mut st = h.st.lock();
    if st.done || st.holders == 0 {
        return;
    }
    st.armed = false;
    if !super::local::node_done(h.orig) {
        // Cancelled with its task: the waiters poll again and find out.
        let wake: Vec<Waker> = st.waiters.iter().map(|(_, w, _)| w.clone()).collect();
        drop(st);
        wake.into_iter().for_each(Waker::wake);
        return;
    }
    keep_result(h, &mut st);
    let waiters = std::mem::take(&mut st.waiters);
    drop(st);
    let queued: Vec<(usize, Waker, u64)> = waiters
        .into_iter()
        .map(|(x, k, _)| (x, k.clone(), super::local::settled(k)))
        .filter(|(.., t)| *t != 0)
        .collect();
    h.st.lock().waiters = queued;
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
        // It may have been driving the promise: the others wait again, the first drives (a
        // started original needs no driver).
        let wake: Vec<Waker> = match st.done || h.node {
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
    // An original future handed to another task stays allocated for the holder there, which
    // drops it ([`holder_drop`]).
    if let Some(w) = FORWARDS
        .lock()
        .as_ref()
        .and_then(|m| m.get(&(h.orig as usize)))
    {
        w.orig.set(Some((h.poll, h.drop)));
        return;
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
    if let Some((p, size)) = forwarded(f) {
        if super::local::velt_rt_fut_peek(p) == 0 {
            return false;
        }
        std::ptr::copy_nonoverlapping(slot(p), slot(f), size);
        return true;
    }
    let h = hub(f);
    on_its_task(&h);
    if h.node && !h.st.lock().done && super::local::node_done(h.orig) {
        // Finished with nobody waiting yet: it settles now.
        settle(&h);
    }
    let st = h.st.lock();
    if st.done {
        std::ptr::copy_nonoverlapping(st.result.as_ptr() as *const u8, slot(f), h.size);
    }
    st.done
}
