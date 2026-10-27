//! `velt_rt_fut_start` on the current-thread executor: a started promise runs until its first
//! suspension at once, then a task of its own drives its state (in place, in the boxed
//! allocation) while the owner only observes completion. Everything runs on the one thread, so
//! this is JS's concurrency model exactly; when one finishes, its awaiter runs next (a JS
//! microtask). Dropped unfinished, a started promise keeps running and its task disposes of the
//! result; `block_on` waits for started promises (see executor.rs).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use super::all::ResultDropFn;
use super::boxed::{self, state, trailer};
use super::leaf::new_leaf;
use super::{context, executor, raw_cx, VeltFut, Wide, PENDING, READY};

/// What a started promise's owner and its driving task share.
pub struct Started {
    done: bool,
    /// The owner dropped its handle: the driver frees the promise.
    detached: bool,
    /// The owner's poll returned READY: the result belongs to it.
    delivered: bool,
    waiter: Option<Waker>,
    result_drop: Option<ResultDropFn>,
    /// The id of the task driving it.
    task: usize,
    /// It can reject (`velt_rt_fut_start_rejecting`): its result is a `Result`, tag first.
    rejectable: bool,
    /// A combinator handles its outcome (`velt_rt_futs_handled`).
    handled: bool,
    /// In `WATCHED`: the checkpoint frees the promise if its owner dropped it meanwhile.
    watched: bool,
    /// It runs a timer's callback (`mark_starting_timer`): `block_on` does not wait for it (a
    /// ref'd timer holds a keep-alive reference instead).
    timer: bool,
}

unsafe fn shared<'a>(f: *mut VeltFut) -> &'a RefCell<Started> {
    &*(*trailer(f)).started
}

/// Start the boxed promise `f` (anything else, or outside `block_on`, is left as it is).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_start(f: *mut VeltFut, result_drop: Option<ResultDropFn>) {
    crate::shared::refuse_tasks_in_read();
    start(f, result_drop, false);
}

/// `velt_rt_fut_start` for a promise that can reject: a rejection its owner has not claimed by
/// the next microtask checkpoint is reported then, as in Node (velt_rt's task/local/unhandled.rs).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_start_rejecting(
    f: *mut VeltFut,
    result_drop: Option<ResultDropFn>,
) {
    crate::shared::refuse_tasks_in_read();
    start(f, result_drop, true);
}

unsafe fn start(f: *mut VeltFut, result_drop: Option<ResultDropFn>, rejectable: bool) {
    if !boxed::is_lazy(f) || !executor::running() {
        return;
    }
    let (id, st) = adopt(f, result_drop);
    st.borrow_mut().rejectable = rejectable;
    let w = executor::waker(id);
    // The creator goes on after this first run: its `await`s take their steps.
    let outer = STARTING.with(|s| s.replace(Rc::as_ptr(&st)));
    let _ = executor::sync_run(|| drive(f, &st, &mut Context::from_waker(&w)));
    STARTING.with(|s| s.set(outer));
}

/// The owner hands promise `f` to another task: its result is transferred with `transfer` as
/// its state finishes (at once if it already finished), as velt_rt does, so a program's values
/// are moved or copied at the same points as on native targets (rt_abi_async.md §1). A race
/// passes the mark to its children; a second mark, and any other future, is ignored.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_transfer(f: *mut VeltFut, transfer: Option<ResultDropFn>) {
    if super::shared::is_shared(f) {
        return super::shared::hand_over(f, transfer);
    }
    // Its identity stays on this task (task/identity.rs).
    super::identity::forget(f);
    if super::race::pass_transfer(f, transfer) {
        return;
    }
    // A result with nothing to move or copy: only a holder of a shared promise is handed over.
    let Some(transfer) = transfer else {
        return;
    };
    let started = std::ptr::fn_addr_eq(
        (*f).poll.0,
        started_poll as unsafe extern "C" fn(*mut VeltFut, *mut c_void) -> u32,
    );
    if !boxed::is_lazy(f) && !started {
        return;
    }
    let t = &mut *trailer(f);
    if t.transfer.is_some() {
        return;
    }
    t.transfer = Some(transfer);
    let done = match started {
        true => shared(f).borrow().done,
        false => t.live == 0,
    };
    if done {
        transfer(state(f));
    }
}

/// The owner gives up promise `f` without cancelling it: a lazy one becomes a started promise
/// with a task of its own, a started one keeps running, and either way its outcome is handled
/// with `quiet_drop`; anything else is dropped (rt_abi_async.md §1). The promise runs before the
/// owner's continuation if it was created during this turn, else at its place in the ready queue
/// after it (like JS's rejection handler, which comes before the timers that woke it): see
/// [`give_up`].
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_detach(f: *mut VeltFut, quiet_drop: Option<ResultDropFn>) {
    if boxed::is_lazy(f) && executor::running() {
        let (id, _) = adopt(f, quiet_drop);
        let turn = executor::turn();
        if (*trailer(f)).turn != turn || executor::yielded(turn) {
            // What woke it may be a timer: it is not part of this turn's microtasks.
            executor::queued_earlier(id);
        }
        executor::refresh_first(id);
    }
    let one = [Wide(f)];
    velt_rt_futs_handled(one.as_ptr(), 1, quiet_drop);
    give_up(f);
}

/// A combinator that settled gives up loser `f` (as `velt_rt_fut_drop`: a started promise keeps
/// running). A started promise created during this turn whose task is queued (woken by this
/// turn's own code, by a promise it settled, say) runs now, before the combinator's awaiter,
/// like its JS microtask; anything else runs at its place in the ready queue, as does every
/// loser when something yielded during this turn (velt_rt's `local::give_up`, #150).
pub(super) unsafe fn give_up(f: *mut VeltFut) {
    let turn = executor::turn();
    let task = (is_started(f) && !executor::yielded(turn))
        .then(|| {
            let s = shared(f).borrow();
            (!s.done && (*trailer(f)).turn == turn).then_some(s.task)
        })
        .flatten();
    // An unfinished started promise stays allocated until its task finishes it.
    super::velt_rt_fut_drop(f);
    if let Some(id) = task {
        executor::sync_run(|| executor::run_now(id, turn));
    }
}

unsafe fn is_started(f: *mut VeltFut) -> bool {
    std::ptr::fn_addr_eq(
        (*f).poll.0,
        started_poll as unsafe extern "C" fn(*mut VeltFut, *mut c_void) -> u32,
    )
}

/// Make lazy `f` a started promise with a task of its own (scheduled, not polled yet).
unsafe fn adopt(
    f: *mut VeltFut,
    result_drop: Option<ResultDropFn>,
) -> (usize, Rc<RefCell<Started>>) {
    let st = Rc::new(RefCell::new(Started {
        done: false,
        detached: false,
        delivered: false,
        waiter: None,
        result_drop,
        task: 0,
        rejectable: false,
        handled: false,
        watched: false,
        timer: false,
    }));
    (*trailer(f)).started = Rc::into_raw(st.clone());
    *f = VeltFut::new(started_poll, started_drop);
    executor::count_local(true);
    let logical = Some(executor::logical_task()).filter(|l| *l != 0);
    let id = executor::spawn_for(new_leaf(driver(f, st.clone())), 0, None, logical);
    st.borrow_mut().task = id;
    (id, st)
}

/// The driving task's poll closure (it holds its own reference to the shared state, so it never
/// touches `f` after the promise finished).
fn driver(f: *mut VeltFut, st: Rc<RefCell<Started>>) -> impl FnMut(&mut Context<'_>) -> Poll<()> {
    // SAFETY: `f` stays allocated until the promise finished (see `started_drop`).
    move |cx: &mut Context<'_>| unsafe { drive(f, &st, cx) }
}

unsafe fn drive(f: *mut VeltFut, st: &Rc<RefCell<Started>>, cx: &mut Context<'_>) -> Poll<()> {
    if st.borrow().done {
        return Poll::Ready(());
    }
    let t = trailer(f);
    if ((*t).poll)(state(f), raw_cx(cx).ptr()) != READY {
        return Poll::Pending;
    }
    (*t).live = 0;
    boxed::run_transfer(f);
    if !st.borrow().timer {
        executor::count_local(false);
    }
    let (detached, waiter, result_drop, watch) = {
        let mut s = st.borrow_mut();
        s.done = true;
        let watch = s.rejectable && !s.handled;
        (s.detached, s.waiter.take(), s.result_drop, watch)
    };
    if detached {
        if let Some(d) = result_drop {
            if drop_unclaimed(f, d) {
                return Poll::Ready(()); // freed once reported (`flush_unhandled`)
            }
        }
        boxed::free(f);
    } else if let Some(w) = waiter {
        executor::wake_next(w);
    } else if watch && *state(f) != 0 {
        // Rejected while nobody waits for it: reported at the checkpoint unless claimed first.
        // The watch keeps `f` allocated until then (`started_drop`).
        st.borrow_mut().watched = true;
        WATCHED.with(|q| q.borrow_mut().push_back((f, Rc::clone(st))));
    }
    Poll::Ready(())
}

/// The futures are handed to a combinator, which handles their rejections like JS: a started
/// promise among them that finishes after it was dropped disposes of its result with
/// `quiet_drop` instead of its `result_drop` (rt_abi_async.md §1). `futs` points to the 8-byte
/// [`Wide`] slots of a Velt array.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_futs_handled(
    futs: *const Wide<*mut VeltFut>,
    n: u64,
    quiet_drop: Option<ResultDropFn>,
) {
    for i in 0..n as usize {
        let f = (*futs.add(i)).0;
        if std::ptr::fn_addr_eq(
            (*f).poll.0,
            started_poll as unsafe extern "C" fn(*mut VeltFut, *mut c_void) -> u32,
        ) {
            let mut s = shared(f).borrow_mut();
            s.result_drop = quiet_drop;
            s.handled = true;
        }
        super::spawn::mark_join_handled(f, quiet_drop);
    }
}

/// What `console.log` shows of promise `f`: 1 when its result is in the slot, 0 while it is
/// pending (also for futures that only run when awaited), as in velt_rt.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_peek(f: *mut VeltFut) -> u8 {
    if super::shared::is_shared(f) {
        return super::shared::peek(f) as u8;
    }
    if boxed::is_lazy(f) {
        return ((*trailer(f)).live == 0) as u8;
    }
    let started = std::ptr::fn_addr_eq(
        (*f).poll.0,
        started_poll as unsafe extern "C" fn(*mut VeltFut, *mut c_void) -> u32,
    );
    if !started {
        return 0;
    }
    let s = shared(f).borrow();
    (s.done && !s.delivered) as u8
}

unsafe extern "C" fn started_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let mut s = shared(f).borrow_mut();
    if s.done {
        s.delivered = true;
        return READY;
    }
    s.waiter = Some(context(cx).waker().clone());
    PENDING
}

unsafe extern "C" fn started_drop(f: *mut VeltFut) {
    super::identity::forget(f);
    // An error a timer or microtask callback threw (`velt_rt_uncaught_now`) is reported now.
    let now = NOW.with(|c| c.get().is_some());
    let (done, delivered, result_drop) = {
        let mut s = shared(f).borrow_mut();
        s.detached = true;
        if s.watched && !(now && s.done) {
            return; // the checkpoint disposes of it (`flush_unhandled`)
        }
        (s.done, s.delivered, s.result_drop)
    };
    if !done {
        return; // the driver finishes it and frees it
    }
    dispose(f, delivered, result_drop);
}

/// Disposes of the finished promise `f` its owner dropped: drops a result the owner did not
/// claim (a rejection is reported) and frees it.
unsafe fn dispose(f: *mut VeltFut, delivered: bool, result_drop: Option<ResultDropFn>) {
    if let (false, Some(d)) = (delivered, result_drop) {
        if drop_unclaimed(f, d) {
            return; // freed once reported (`flush_unhandled`)
        }
    }
    boxed::free(f);
}

/// A promise that rejected while nobody waited for it, and its shared state.
type Watched = (*mut VeltFut, Rc<RefCell<Started>>);

/// The promise being started runs a timer's callback (`velt_rt_timer_turn` in its first run).
pub(super) fn mark_starting_timer() {
    let st = STARTING.with(Cell::get);
    // SAFETY: the shared state of the promise whose first run `start` is doing.
    if let Some(st) = unsafe { st.as_ref() } {
        let mut s = st.borrow_mut();
        if !s.timer && !s.done {
            s.timer = true;
            executor::count_local(false);
        }
    }
}

thread_local! {
    /// The shared state of the promise whose first run `start` is doing (null: none).
    static STARTING: Cell<*const RefCell<Started>> = const { Cell::new(std::ptr::null()) };
    /// The started promise whose result is being dropped right now (null: none).
    static DROPPING: Cell<*mut VeltFut> = const { Cell::new(std::ptr::null_mut()) };
    /// The next rejection reported ends the process at once (`velt_rt_uncaught_now`).
    static NOW: Cell<Option<*const crate::str::VeltStr>> = const { Cell::new(None) };
    /// Rejected promises to report once the microtask queue is drained, with their drops.
    static DEFERRED: RefCell<VecDeque<(*mut VeltFut, ResultDropFn)>> =
        const { RefCell::new(VecDeque::new()) };
    /// Promises that rejected while their owner did not wait for them (`drive`): reported once
    /// the microtask queue is drained unless the owner claimed them by then.
    static WATCHED: RefCell<VecDeque<Watched>> =
        const { RefCell::new(VecDeque::new()) };
}

/// Drop the unclaimed result of finished promise `f` with `d`; true when it is an unhandled
/// rejection kept to be reported once the microtask queue is drained, as in Node (velt_rt's
/// task/local/unhandled.rs).
unsafe fn drop_unclaimed(f: *mut VeltFut, d: ResultDropFn) -> bool {
    if !executor::running() {
        d(state(f));
        return false;
    }
    DROPPING.with(|c| c.set(f));
    d(state(f));
    // Still set: `velt_rt_unhandled_defer` was not called (not a rejection).
    if !DROPPING.with(|c| c.replace(std::ptr::null_mut())).is_null() {
        return false;
    }
    DEFERRED.with(|q| q.borrow_mut().push_back((f, d)));
    true
}

/// Called by a rejection's report before it reports: 1 when the runtime keeps the promise and
/// reports it once the microtask queue is drained, 0 to report now.
#[no_mangle]
pub extern "C" fn velt_rt_unhandled_defer() -> u8 {
    let kept = DROPPING.with(|c| !c.replace(std::ptr::null_mut()).is_null());
    if let Some(loc) = NOW.with(|c| c.take()) {
        crate::panic::velt_rt_set_throw_loc(loc);
        return 0;
    }
    kept as u8
}

/// The next unhandled rejection is an error a timer or microtask callback threw: reported at
/// once, as Node's uncaught exception (velt_rt's task/local/unhandled.rs).
#[no_mangle]
pub extern "C" fn velt_rt_uncaught_now() {
    // Its report shows where the callback threw, not where the prelude throws it again.
    NOW.with(|c| c.set(Some(crate::panic::velt_rt_throw_loc())));
}

/// The microtask queue is drained: report the rejections that waited for it (the first one
/// ends the process).
pub(super) fn flush_unhandled() {
    while let Some((f, st)) = WATCHED.with(|q| q.borrow_mut().pop_front()) {
        let (detached, unclaimed, delivered, d) = {
            let mut s = st.borrow_mut();
            s.watched = false;
            let unclaimed = !s.delivered && !s.handled && s.waiter.is_none();
            (s.detached, unclaimed, s.delivered, s.result_drop)
        };
        // SAFETY: the watch kept `f` allocated (`started_drop`).
        unsafe {
            if detached {
                // Dropped by its owner since: disposed of as the drop would have.
                dispose(f, delivered, d);
            } else if let (true, Some(d)) = (unclaimed, d) {
                // A promise handed to another task (its transfer is set) is that task's to
                // claim; otherwise the report ends the process.
                if (*trailer(f)).transfer.is_none() {
                    d(state(f));
                }
            }
        }
    }
    while let Some((f, d)) = DEFERRED.with(|q| q.borrow_mut().pop_front()) {
        // SAFETY: a finished promise kept with its unclaimed result (`drop_unclaimed`).
        unsafe {
            d(state(f));
            boxed::free(f);
        }
    }
}

/// `velt_rt_task_gate`: one thread, so a timer's callback never runs during a task's poll and
/// needs no gate (velt_rt's task/local/gate.rs).
#[no_mangle]
pub extern "C" fn velt_rt_task_gate() -> u64 {
    0
}

/// `velt_rt_task_gate_wait`: ready at once (see [`velt_rt_task_gate`]).
#[no_mangle]
pub extern "C" fn velt_rt_task_gate_wait(_gate: u64) -> *mut VeltFut {
    new_leaf(|_: &mut Context<'_>| Poll::Ready(()))
}

/// `velt_rt_task_gate_free`: nothing to release.
#[no_mangle]
pub extern "C" fn velt_rt_task_gate_free(_gate: u64) {}

/// The task id `new Promise` compares (see velt_rt): one thread, so values never need copying
/// between tasks and every task reports the same id.
#[no_mangle]
pub extern "C" fn velt_rt_task_id() -> u64 {
    1
}

/// What the compiler emits once per program with module state (velt_rt's
/// task/local/module_state.rs).
#[repr(C)]
pub struct ModuleDesc {
    pub size: u64,
    pub align: u64,
    pub init: unsafe extern "C" fn(*mut u8),
    pub drop: unsafe extern "C" fn(*mut u8),
    /// Each slot's name, NUL-terminated, in slot order.
    pub names: *const u8,
    /// The root file's slots, for code swapped in (not on WebAssembly: unused here).
    pub rest: Option<unsafe extern "C" fn(*mut u8)>,
}

thread_local! {
    /// The program's module state: one thread, and every task reports the same id
    /// (`velt_rt_task_id`), so the tasks share one block.
    static MODULE_STATE: Cell<*mut u8> = const { Cell::new(std::ptr::null_mut()) };
    /// How many of its slots are initialized (`u64::MAX` once the last one is).
    static MODULE_READY: Cell<u64> = const { Cell::new(0) };
}

/// Has the program's module state been set up (it has some, and `init` ran or runs)?
pub(crate) fn module_state_used() -> bool {
    !MODULE_STATE.with(Cell::get).is_null()
}

/// The module state, allocated zeroed and initialized by `desc.init` on first access, to reach
/// slot `slot` (one whose initializer has not run yet is a `ReferenceError`).
///
/// # Safety
/// `desc` must point to the program's module descriptor.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_module_state(desc: *const ModuleDesc, slot: u64) -> *mut u8 {
    let p = velt_rt_module_block(desc);
    if slot >= MODULE_READY.with(Cell::get) {
        not_ready(desc, slot);
    }
    p
}

/// The module state (allocated and initialized as by [`velt_rt_module_state`]) without checking
/// that a slot is initialized: for the root file's `main`, which initializes its file's slots.
///
/// # Safety
/// `desc` must point to the program's module descriptor.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_module_block(desc: *const ModuleDesc) -> *mut u8 {
    let mut p = MODULE_STATE.with(Cell::get);
    if p.is_null() {
        let d = &*desc;
        let layout = std::alloc::Layout::from_size_align(
            (d.size as usize).max(1),
            (d.align as usize).max(1),
        )
        .expect("ICE: module state layout");
        p = std::alloc::alloc_zeroed(layout);
        if p.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        MODULE_STATE.with(|c| c.set(p));
        (d.init)(p);
    }
    p
}

/// The initializers of the slots below `n` have run.
#[no_mangle]
pub extern "C" fn velt_rt_module_ready(_state: *mut u8, n: u64) {
    MODULE_READY.with(|c| c.set(n));
}

/// A slot reached before its initializer ran: an uncaught `ReferenceError`, as in JavaScript.
#[cold]
unsafe fn not_ready(desc: *const ModuleDesc, slot: u64) -> ! {
    let mut s = (*desc).names;
    for _ in 0..slot {
        while *s != 0 {
            s = s.add(1);
        }
        s = s.add(1);
    }
    let name = std::ffi::CStr::from_ptr(s.cast()).to_string_lossy();
    crate::io::flush_stdout();
    let msg = format!(
        "Uncaught ReferenceError: Cannot access '{name}' before initialization
"
    );
    crate::platform::write(2, msg.as_bytes());
    crate::platform::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::boxed::velt_rt_fut_box;
    use crate::task::executor::velt_rt_block_on;
    use crate::task::{velt_rt_fut_drop, velt_rt_fut_poll, velt_rt_yield_now};
    use std::cell::Cell;

    thread_local! {
        static LOG: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
        static RESULT_DROPS: Cell<u32> = const { Cell::new(0) };
    }

    fn log(s: &'static str) {
        LOG.with(|l| l.borrow_mut().push(s));
    }

    /// `{ result: i64, tag: i64 }`: logs, yields once, logs, returns 5.
    unsafe extern "C" fn child_poll(s: *mut u8, cx: *mut c_void) -> u32 {
        let st = s as *mut i64;
        if *st.add(1) == 0 {
            log("child start");
            *st.add(1) = 1;
            velt_rt_yield_now(cx);
            return PENDING;
        }
        log("child end");
        *st = 5;
        READY
    }

    unsafe extern "C" fn no_drop(_: *mut u8) {}

    unsafe extern "C" fn count_result_drop(_: *mut u8) {
        RESULT_DROPS.with(|c| c.set(c.get() + 1));
    }

    unsafe fn started_child() -> *mut VeltFut {
        let init = [0i64; 2];
        let f = velt_rt_fut_box(child_poll, no_drop, init.as_ptr() as *const u8, 16, 8);
        velt_rt_fut_start(f, Some(count_result_drop));
        f
    }

    /// `{ result, tag, p, q }`: starts two children, awaits the first, drops the second.
    unsafe extern "C" fn main_poll(s: *mut u8, cx: *mut c_void) -> u32 {
        let st = s as *mut i64;
        if *st.add(1) == 0 {
            *st.add(2) = started_child() as i64;
            log("main");
            *st.add(3) = started_child() as i64;
            velt_rt_fut_drop(*st.add(3) as *mut VeltFut);
            *st.add(1) = 1;
        }
        let p = *st.add(2) as *mut VeltFut;
        if velt_rt_fut_poll(p, cx) == PENDING {
            return PENDING;
        }
        *st = *((p as *const u8).add(16) as *const i64);
        velt_rt_fut_drop(p);
        log("main end");
        READY
    }

    /// `{ result, tag, x, y }`: starts two children, awaits the first: its continuation runs as
    /// soon as the child finishes, before the other child's next step (a microtask).
    unsafe extern "C" fn first_poll(s: *mut u8, cx: *mut c_void) -> u32 {
        let st = s as *mut i64;
        if *st.add(1) == 0 {
            *st.add(2) = started_child() as i64;
            *st.add(3) = started_child() as i64;
            *st.add(1) = 1;
        }
        let x = *st.add(2) as *mut VeltFut;
        if *st.add(1) == 1 {
            if velt_rt_fut_poll(x, cx) == PENDING {
                return PENDING;
            }
            velt_rt_fut_drop(x);
            log("root got x");
            *st.add(1) = 2;
        }
        let y = *st.add(3) as *mut VeltFut;
        if velt_rt_fut_poll(y, cx) == PENDING {
            return PENDING;
        }
        velt_rt_fut_drop(y);
        READY
    }

    /// `{ result, tag }`: polls a lazy child once, then gives it up without cancelling it (a
    /// sibling of an early `Promise.all` rejection) and returns.
    unsafe extern "C" fn detach_poll(s: *mut u8, cx: *mut c_void) -> u32 {
        let init = [0i64; 2];
        let f = velt_rt_fut_box(child_poll, no_drop, init.as_ptr() as *const u8, 16, 8);
        assert_eq!(velt_rt_fut_poll(f, cx), PENDING);
        velt_rt_fut_detach(f, Some(count_result_drop));
        log("owner goes on");
        *(s as *mut i64) = 0;
        READY
    }

    /// `{ result, tag, wait }`: awaits the future `wait`, logs, returns 0.
    unsafe extern "C" fn waiter_poll(s: *mut u8, cx: *mut c_void) -> u32 {
        let st = s as *mut i64;
        let wait = *st.add(2) as *mut VeltFut;
        if velt_rt_fut_poll(wait, cx) == PENDING {
            return PENDING;
        }
        velt_rt_fut_drop(wait);
        log("waiter end");
        *st = 0;
        READY
    }

    /// `{ result, tag }`: polls a lazy waiter once, settles what it waits for and gives it up (a
    /// sibling of an early `Promise.all` rejection that resolved its promise first, #150).
    unsafe extern "C" fn ready_detach_poll(s: *mut u8, cx: *mut c_void) -> u32 {
        use crate::task::latch::{velt_rt_latch_free, velt_rt_latch_new, velt_rt_latch_open};
        let latch = velt_rt_latch_new();
        let init = [0, 0, crate::task::latch::velt_rt_latch_wait(latch) as i64];
        let f = velt_rt_fut_box(waiter_poll, no_drop, init.as_ptr() as *const u8, 24, 8);
        assert_eq!(velt_rt_fut_poll(f, cx), PENDING);
        velt_rt_latch_open(latch);
        velt_rt_fut_detach(f, None);
        log("owner goes on");
        velt_rt_latch_free(latch);
        *(s as *mut i64) = 0;
        READY
    }

    #[test]
    fn a_detached_promise_that_is_ready_runs_before_its_owner_goes_on() {
        LOG.with(|l| l.borrow_mut().clear());
        let mut st = [0i64; 2];
        unsafe { velt_rt_block_on(ready_detach_poll, st.as_mut_ptr() as *mut u8) };
        let log = LOG.with(|l| l.borrow().clone());
        assert_eq!(log, ["waiter end", "owner goes on"]);
    }

    /// Fake transfer glue: logs and adds 100 to an `i64` result.
    unsafe extern "C" fn tag(slot: *mut u8) {
        log("transfer");
        *(slot as *mut i64) += 100;
    }

    /// `{ result, tag, p }`: starts a child, marks it twice (it goes to another task), awaits it.
    unsafe extern "C" fn hand_on_poll(s: *mut u8, cx: *mut c_void) -> u32 {
        let st = s as *mut i64;
        if *st.add(1) == 0 {
            let p = started_child();
            velt_rt_fut_transfer(p, Some(tag));
            velt_rt_fut_transfer(p, Some(tag));
            log("marked");
            *st.add(2) = p as i64;
            *st.add(1) = 1;
        }
        let p = *st.add(2) as *mut VeltFut;
        if velt_rt_fut_poll(p, cx) == PENDING {
            return PENDING;
        }
        *st = *((p as *const u8).add(16) as *const i64);
        velt_rt_fut_drop(p);
        READY
    }

    #[test]
    fn a_marked_promise_transfers_its_result_once_as_it_finishes() {
        LOG.with(|l| l.borrow_mut().clear());
        let mut st = [0i64; 3];
        unsafe { velt_rt_block_on(hand_on_poll, st.as_mut_ptr() as *mut u8) };
        assert_eq!(st[0], 105);
        let log = LOG.with(|l| l.borrow().clone());
        assert_eq!(log, ["child start", "marked", "child end", "transfer"]);
    }

    #[test]
    fn a_detached_promise_runs_after_its_owner_and_finishes() {
        LOG.with(|l| l.borrow_mut().clear());
        let mut st = [0i64; 2];
        unsafe { velt_rt_block_on(detach_poll, st.as_mut_ptr() as *mut u8) };
        let log = LOG.with(|l| l.borrow().clone());
        assert_eq!(log, ["child start", "owner goes on", "child end"]);
        assert_eq!(RESULT_DROPS.with(Cell::get), 1);
    }

    #[test]
    fn a_finished_promise_resumes_its_awaiter_first() {
        LOG.with(|l| l.borrow_mut().clear());
        let mut st = [0i64; 4];
        unsafe { velt_rt_block_on(first_poll, st.as_mut_ptr() as *mut u8) };
        let log = LOG.with(|l| l.borrow().clone());
        let want = [
            "child start",
            "child start",
            "child end",
            "root got x",
            "child end",
        ];
        assert_eq!(log, want);
    }

    #[test]
    fn started_promises_run_now_and_dropped_ones_finish() {
        LOG.with(|l| l.borrow_mut().clear());
        let mut st = [0i64; 4];
        unsafe { velt_rt_block_on(main_poll, st.as_mut_ptr() as *mut u8) };
        assert_eq!(st[0], 5);
        let log = LOG.with(|l| l.borrow().clone());
        let want = [
            "child start",
            "main",
            "child start",
            "child end",
            "main end",
            "child end",
        ];
        assert_eq!(log, want);
        // The dropped child's result was disposed of when it finished; the awaited one's not.
        assert_eq!(RESULT_DROPS.with(Cell::get), 1);
    }
}
