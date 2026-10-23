//! Eager ("hybrid") promises: `velt_rt_fut_start` runs a stored promise's compiled state until
//! its first suspension right away, like calling an async function in JS, and from then on the
//! task that started it drives it as a *local* promise (docs/reference/async.md).
//!
//! Every task root (`block_on`, spawned tasks, HTTP handlers) polls through [`Locals`], which
//! makes the task's local set current (a thread-local pointer, valid only during the poll) and
//! first polls the local promises that were woken. A task that never starts a promise has no set
//! and pays two thread-local writes per poll. When the root finishes while local promises are
//! still running, the set moves to an *orphan* task that drives them to completion. Like every
//! task, it keeps the process alive while it can still make progress (task/quiet.rs), as a JS
//! program waits for its pending work but not for promises nothing can settle.
//!
//! Directly awaited calls never get here: `await f()` embeds `f`'s state in the caller's (no
//! allocation), and `spawn(f())` gives `f` its own task.

mod gate;
mod losers;
mod micro;
mod module_state;
mod node;
mod order;
mod set;
mod timers;
mod unhandled;

pub use gate::{velt_rt_task_gate, velt_rt_task_gate_free, velt_rt_task_gate_wait};
pub(crate) use losers::give_up;
pub use losers::velt_rt_fut_detach;
pub(crate) use micro::{grant_step, settled, step_free, ticket_pending};
pub use micro::{velt_rt_await_ready, velt_rt_await_tick, velt_rt_microtask};
pub(crate) use module_state::{
    drop_at_exit as module_drop_at_exit, held as module_held, mutex_locked, mutex_unlocked,
    release_outside as module_release_outside,
};
pub use module_state::{velt_rt_module_block, velt_rt_module_ready, velt_rt_module_state};
pub use order::{velt_rt_timer_again, velt_rt_timer_free, velt_rt_timer_new, velt_rt_timer_turn};
pub(crate) use timers::TimerLeaf;
pub use unhandled::{velt_rt_uncaught_now, velt_rt_unhandled_defer};

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use self::gate::Gate;
use self::set::LocalSet;
use self::timers::Timers;
use super::all::ResultDropFn;
use super::{DropFn, PollFn, VeltFut};
use std::sync::Arc;

/// What `velt_rt_fut_start` needs from the task being polled.
struct TaskCx {
    /// The task's set (null until the first start). Raw pointers throughout: compiled code
    /// reached from the task's poll uses it re-entrantly.
    set: *mut *mut LocalSet,
    waker: *const Waker,
    /// The task's id for `velt_rt_task_id` (0 until first asked for).
    id: *mut u64,
    /// The task's timers (none until its first `sleep`; timers.rs).
    timers: *mut Option<Arc<Timers>>,
    /// Whether the task uses module state, and whether this poll holds the module lock
    /// (module_state.rs).
    module: module_state::PollState,
    /// Which poll of the task this is ([`next_turn`]): a promise created during this same poll
    /// can only have been woken by the poll's own code, never by a timer or another event.
    turn: u32,
    /// Something yielded during this poll (`yieldNow()`): it must not run again before the
    /// task's next poll (losers.rs).
    yielded: Cell<bool>,
    /// The task's await word (micro.rs): its queued microtasks plus the synchronous runs under
    /// way. Generated code reads it through `cx` (`await_word`).
    word: Cell<u32>,
    /// The microtask being run is a waiter's step already (micro.rs).
    credit: Cell<bool>,
}

thread_local! {
    static CURRENT: Cell<*const TaskCx> = const { Cell::new(std::ptr::null()) };
}

/// The await word of no task: always 0 (outside a task an `await` goes straight on).
static NO_WORD: u32 = 0;

/// The await word of the task being polled, for `cx` (task/mod.rs `raw_cx`).
pub(crate) fn await_word() -> *const u32 {
    let tcx = CURRENT.with(Cell::get);
    // SAFETY: `CURRENT` is only set while its `TaskCx` lives (`Enter`).
    match unsafe { tcx.as_ref() } {
        Some(t) => t.word.as_ptr(),
        None => &NO_WORD,
    }
}

/// The set of the task being polled (null outside a task or before its first started promise).
fn current_set() -> *mut LocalSet {
    let tcx = CURRENT.with(Cell::get);
    if tcx.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `CURRENT` is only set while its `TaskCx` lives (`Enter`).
    unsafe { *(*tcx).set }
}

/// Which logical task is being polled (0 outside a task): its [`velt_rt_task_id`], never reused,
/// and kept by the task's local promises when they outlive its root (the orphan task driving
/// them has the same id). Shared promises (task/shared.rs) check that they stay on one task.
pub(crate) fn task_key() -> u64 {
    velt_rt_task_id()
}

/// The turn of the task being polled (0 outside a task).
fn current_turn() -> u32 {
    let tcx = CURRENT.with(Cell::get);
    // SAFETY: `CURRENT` is only set while its `TaskCx` lives (`Enter`).
    unsafe { tcx.as_ref() }.map_or(0, |t| t.turn)
}

/// `yieldNow()` in the task being polled: what yielded waits for the task's next poll, so no
/// promise of this poll runs early (losers.rs).
pub(crate) fn note_yield() {
    let tcx = CURRENT.with(Cell::get);
    // SAFETY: `CURRENT` is only set while its `TaskCx` lives (`Enter`).
    if let Some(t) = unsafe { tcx.as_ref() } {
        t.yielded.set(true);
    }
}

/// The turn after `turn`: turns count task polls in `1..node::TURNS` (0 means none).
fn next_turn(turn: u32) -> u32 {
    if turn + 1 >= node::TURNS {
        1
    } else {
        turn + 1
    }
}

/// Run `f` with the timers of the task being polled, created on first use; `None` outside a task.
/// `f` also gets the task's waker.
fn with_timers<R>(f: impl FnOnce(&Arc<Timers>, &Waker) -> R) -> Option<R> {
    let tcx = CURRENT.with(Cell::get);
    if tcx.is_null() {
        return None;
    }
    // SAFETY: `CURRENT` is only set while its `TaskCx` lives (`Enter`); `timers` points into the
    // task's own `SetPtr`, which only this task's polls touch.
    let (timers, task) = unsafe { (&mut *(*tcx).timers, &*(*tcx).waker) };
    Some(f(timers.get_or_insert_with(Default::default), task))
}

/// Run local promise `f` of the task being polled now (queued if it is being polled already, or
/// outside a task).
pub(super) unsafe fn run_here(f: *mut VeltFut) {
    let tcx = CURRENT.with(Cell::get);
    let set = current_set();
    if tcx.is_null() || set.is_null() {
        node::queue(f);
        return;
    }
    set::run_now(set, &*tcx, f);
}

/// Run `f` with the gate of the task being polled, created on first use (with the task's set,
/// which keeps it); `None` outside a task.
fn with_gate<R>(f: impl FnOnce(&Arc<Gate>) -> R) -> Option<R> {
    // SAFETY: the set of the task being polled, used only by this task's polls.
    let set = unsafe { current_set_or_new().as_mut() }?;
    Some(f(set.gate.get_or_insert_with(Gate::new_busy)))
}

/// Makes a task's set current for the duration of a poll (restores the previous one on drop).
struct Enter(*const TaskCx);

impl Enter {
    fn new(cx: &TaskCx) -> Enter {
        Enter(CURRENT.with(|c| c.replace(cx)))
    }
}

impl Drop for Enter {
    fn drop(&mut self) {
        CURRENT.with(|c| c.set(self.0));
    }
}

/// Owned pointer to a task's local set (null = none yet), its id, its timers, its turn, and
/// whether it touched module state (in the padding: the size stays 32 bytes).
struct SetPtr(*mut LocalSet, u64, Option<Arc<Timers>>, u32, bool);

const _: () = assert!(std::mem::size_of::<SetPtr>() == 32);

// SAFETY: the set moves with its task between workers and is only used by that task's polls.
unsafe impl Send for SetPtr {}

impl SetPtr {
    const NONE: SetPtr = SetPtr(std::ptr::null_mut(), 0, None, 0, false);

    /// Poll with this set current: drain its woken promises (resuming the task's root with
    /// `root` when a promise it awaits finishes), then poll the root once more if a promise ran
    /// after its last poll, then run the microtasks that queued.
    ///
    /// # Safety
    /// `waker` must stay valid during the call.
    unsafe fn enter(&mut self, waker: *const Waker, root: &mut dyn FnMut()) {
        self.3 = next_turn(self.3);
        let took = module_state::start_poll(self.4);
        let tcx = TaskCx {
            set: &mut self.0,
            waker,
            id: &mut self.1,
            timers: &mut self.2,
            module: module_state::PollState {
                touched: &mut self.4,
                took: Cell::new(took),
            },
            turn: self.3,
            yielded: Cell::new(false),
            word: Cell::new(self.0.as_ref().map_or(0, |s| s.micro.len() as u32)),
            credit: Cell::new(false),
        };
        let _enter = Enter::new(&tcx);
        if let Some(g) = self.0.as_ref().and_then(|s| s.gate.as_ref()) {
            g.enter();
        }
        let timers = self.2.as_deref();
        if self.0.is_null() {
            // Only the root can wait for timers here; it is polled below.
            if let Some(t) = timers {
                t.fire(&*waker);
            }
        } else {
            // A live set owned by this task; compiled code run from the drain reaches it only
            // through `tcx`.
            set::drain(&mut set::Driver {
                set: self.0,
                task: &*waker,
                root,
                timers,
                word: tcx.word.as_ptr(),
            });
        }
        root();
        // The microtasks the root's poll queued (micro.rs); they live in the set.
        if !self.0.is_null() && set::has_microtasks(self.0) {
            micro::drain(&mut set::Driver {
                set: self.0,
                task: &*waker,
                root,
                timers: None,
                word: tcx.word.as_ptr(),
            });
        }
        // Rejections nobody handles, reported once the queue is drained (unhandled.rs).
        if !self.0.is_null() && unhandled::waiting(self.0) {
            unhandled::flush(self.0);
        }
        // The poll is over: timers this task created may run their callbacks (gate.rs).
        if let Some(g) = self.0.as_ref().and_then(|s| s.gate.as_ref()) {
            g.leave();
        }
        // Last: the module lock this poll took (module_state.rs).
        module_state::end_poll(tcx.module.took.get());
    }

    /// Hand the set over if promises in it are still running (it is dropped otherwise).
    fn take_busy(&mut self) -> Option<SetPtr> {
        let set = std::mem::replace(self, SetPtr::NONE);
        (!set.is_idle()).then_some(set)
    }

    fn is_idle(&self) -> bool {
        // SAFETY: null or a live set owned by `self`.
        unsafe { self.0.as_ref() }.is_none_or(LocalSet::is_idle)
    }
}

impl Drop for SetPtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            let set = self.0;
            // SAFETY: created by `Box::into_raw` in `velt_rt_fut_start`, owned by `self`.
            let free = || drop(unsafe { Box::from_raw(set) });
            // Promises of a task that used module state may hold objects from there.
            if self.4 {
                module_state::with_lock(free);
            } else {
                free();
            }
        }
    }
}

/// The local promises of one task root (see the module docs).
pub struct Locals {
    set: SetPtr,
}

impl Default for Locals {
    fn default() -> Self {
        Locals { set: SetPtr::NONE }
    }
}

impl Locals {
    /// Run `f`, which drops the task's own state outside a poll (a cancelled task), with the
    /// module lock when the task used module state (module_state.rs).
    #[inline]
    pub(super) fn drop_with(&self, f: impl FnOnce()) {
        if self.set.4 {
            module_state::with_lock(f);
        } else {
            f();
        }
    }

    /// Poll the task: its woken local promises, then `root` (with the set current). Local
    /// promises still running when `root` finishes move to an orphan task.
    pub fn poll_root(
        &mut self,
        cx: &mut Context<'_>,
        mut root: impl FnMut(&mut Context<'_>) -> Poll<()>,
    ) -> Poll<()> {
        let waker: *const Waker = cx.waker();
        let mut r = Poll::Pending;
        let mut poll = || {
            if r.is_pending() {
                r = root(cx);
            }
        };
        // SAFETY: `cx`'s waker outlives this call.
        unsafe { self.set.enter(waker, &mut poll) };
        if r.is_ready() {
            if let Some(set) = self.set.take_busy() {
                Orphan::spawn(set);
            }
        }
        r
    }
}

/// Local promises that outlived their task's root: driven to completion by a task of their own.
struct Orphan {
    set: SetPtr,
}

impl Orphan {
    fn spawn(set: SetPtr) {
        // What the root printed comes first: the orphan may run on another worker at once.
        crate::io::publish_before_handoff();
        super::runtime::spawn(Orphan { set });
    }
}

impl Future for Orphan {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        // SAFETY: `cx`'s waker outlives the call.
        unsafe { this.set.enter(cx.waker(), &mut || ()) };
        crate::io::publish_thread_output();
        if this.set.is_idle() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

/// Move the compiled state at `state_ptr` (`state_size` bytes, align <= 16) into a new heap
/// `VeltFut`. The caller gives up ownership of the state's contents. Its result (state offset 0)
/// appears at the future's result slot, offset 16. The future is lazy: it runs when polled, or
/// from its first suspension on after `velt_rt_fut_start`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_box(
    poll: PollFn,
    drop: DropFn,
    state_ptr: *const u8,
    state_size: u64,
    state_align: u64,
) -> *mut VeltFut {
    if state_align > 16 {
        crate::panic::fatal("velt_rt_fut_box: state alignment above 16");
    }
    node::alloc_node(poll, drop, state_ptr, state_size)
}

/// Start the promise `f` now (a stored promise, `const p = f()`): run its state until its first
/// suspension, then let the current task drive it as a local promise. `result_drop` drops a
/// result nobody claims (null if the result needs no drop). Only futures from `velt_rt_fut_box`
/// are started; anything else (runtime leaves, join handles, promises already started) is left
/// as it is, as is every future when no task is running (a promise created by synchronous `main`
/// runs when it is awaited or spawned).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_start(f: *mut VeltFut, result_drop: Option<ResultDropFn>) {
    start(f, result_drop, false);
}

/// [`velt_rt_fut_start`] for a promise that can reject (its result is a `Result`, tag first): a
/// rejection its owner has not claimed by the next microtask checkpoint is reported as uncaught
/// then, as in Node (unhandled.rs).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_start_rejecting(
    f: *mut VeltFut,
    result_drop: Option<ResultDropFn>,
) {
    start(f, result_drop, true);
}

unsafe fn start(f: *mut VeltFut, result_drop: Option<ResultDropFn>, rejecting: bool) {
    if !node::is_lazy(f) {
        return;
    }
    let tcx = CURRENT.with(Cell::get);
    if tcx.is_null() {
        return;
    }
    let set = task_set(&*tcx);
    node::stamp(f, (*tcx).turn);
    node::mark_started(f, &(*set).shared, result_drop);
    if rejecting {
        node::mark_rejectable(f);
    }
    // The creator goes on after this first run, so the run's `await`s take their steps through
    // the queue (micro.rs).
    let t = &*tcx;
    micro::sync_enter(t);
    // The credit of a waiter being resumed (micro.rs) is its own step: not one of the promise
    // it starts, whose `await`s take steps of their own.
    let credit = t.credit.replace(false);
    let finished = node::poll_first(f, t.word.as_ptr());
    t.credit.set(credit);
    micro::sync_exit(t);
    if finished {
        // Finished before its first suspension: nobody awaits it yet, nothing to resume.
        node::finish_first(f);
    } else {
        // A wake during the first poll was queued; the next drain runs it as a member.
        node::count_set(f);
        set::add_member(set, f);
    }
}

/// The set of task `tcx`, created on first use.
unsafe fn task_set(tcx: &TaskCx) -> *mut LocalSet {
    if (*tcx.set).is_null() {
        *tcx.set = Box::into_raw(LocalSet::new(&*tcx.waker));
    }
    *tcx.set
}

/// The set of the task being polled, created on first use (null outside a task).
fn current_set_or_new() -> *mut LocalSet {
    let tcx = CURRENT.with(Cell::get);
    // SAFETY: `CURRENT` is only set while its `TaskCx` lives (`Enter`).
    match unsafe { tcx.as_ref() } {
        Some(t) => unsafe { task_set(t) },
        None => std::ptr::null_mut(),
    }
}

/// The owner hands promise `f` to another task (`spawn`, a channel, a settled promise): its
/// result is transferred with `transfer` (in place, compiled transfer glue) on the task that
/// produces it, so the other task never sees an object the producing task still references
/// (#160). A started or lazy promise node keeps the function for when its state finishes; a
/// race passes it on to its children (the winner's result is theirs); anything else (a join
/// handle, whose task already had its inputs transferred, or a runtime leaf) produces nothing
/// to transfer. A null `transfer` (a result with nothing to move or copy) only hands over a
/// holder of a shared promise (task/shared.rs).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_transfer(f: *mut VeltFut, transfer: Option<ResultDropFn>) {
    if crate::task::shared::is_shared(f) {
        return crate::task::shared::hand_over(f, transfer);
    }
    if crate::task::race::pass_transfer(f, transfer) {
        return;
    }
    if let Some(t) = transfer {
        node::set_transfer(f, t);
    }
}

/// What `console.log` shows of promise `f`, which its owner holds: 1 when its result is in the
/// result slot (+16; a `Result` tag first for a promise that can reject), 0 while it is pending.
/// Nothing is polled, claimed or moved. A future that only runs when it is awaited (a runtime
/// leaf, a join handle, a combinator, a promise created outside a task) reads as pending.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_peek(f: *mut VeltFut) -> u8 {
    if crate::task::shared::is_shared(f) {
        return crate::task::shared::peek(f) as u8;
    }
    node::peek(f) as u8
}

/// Is `f` a started promise (driven by its task's set)?
pub(crate) unsafe fn is_started_node(f: *mut VeltFut) -> bool {
    node::is_started(f)
}

/// Await started promise `f` without adopting it: its set drives it, and `cx`'s waker is woken
/// as it finishes (a shared promise's hub, task/shared.rs). True once it finished; its owner
/// then claims the result with [`deliver_unadopted`].
pub(crate) unsafe fn await_unadopted(f: *mut VeltFut, cx: *mut std::ffi::c_void) -> bool {
    node::poll_unadopted(f, cx)
}

/// The owner of finished started promise `f` claims its result.
pub(crate) unsafe fn deliver_unadopted(f: *mut VeltFut) {
    node::deliver_unadopted(f)
}

/// The task that runs the promises code outside a task hands to tasks (a holder of a shared
/// promise given to a spawned call in a synchronous `main`, task/shared.rs): like any task it
/// has an id and a set, so the promise is shared on it. Code outside a task enters it for each
/// hand-over; once that code is done ([`outside_done`]), the set moves to an orphan task that
/// drives what is still running (until then a wake only queues it).
///
/// In a program with module state it is a touched task (module_state.rs): code outside a task
/// may have put holders of its promises in module state, where tasks holding the module lock
/// use them (task/shared.rs `on_its_task`), so each of its polls takes the lock too, here and
/// in the orphan.
static OUTSIDE_HOME: parking_lot::Mutex<SetPtr> = parking_lot::Mutex::new(SetPtr::NONE);

/// Run `f` (outside a task) as a step of the task of [`OUTSIDE_HOME`].
pub(crate) fn in_outside_home(f: impl FnOnce()) {
    if module_state::used() {
        // The module lock first, then the home's: the order every taker of both keeps.
        module_state::with_lock(|| outside_home_step(f));
    } else {
        outside_home_step(f);
    }
}

fn outside_home_step(f: impl FnOnce()) {
    let mut home = OUTSIDE_HOME.lock();
    home.4 |= module_state::used();
    // Its promises may start timers and I/O, which need the runtime.
    let _rt = super::runtime::handle().enter();
    let mut f = Some(f);
    // SAFETY: the no-op waker is static; the set is the home's own.
    unsafe {
        home.enter(Waker::noop(), &mut || {
            if let Some(f) = f.take() {
                f()
            }
        })
    };
}

/// Code outside a task is done (`main` returned): the promises the outside home runs go on in
/// an orphan task, a touched one when the program has module state ([`OUTSIDE_HOME`]).
pub(crate) fn outside_done() {
    let mut home = std::mem::replace(&mut *OUTSIDE_HOME.lock(), SetPtr::NONE);
    home.4 |= module_state::used();
    if let Some(set) = home.take_busy() {
        Orphan::spawn(set);
    }
}

/// Has started promise `f` finished?
pub(crate) unsafe fn node_done(f: *mut VeltFut) -> bool {
    node::is_done(f)
}

/// The `n` futures in `futs` are handled by a combinator (`Promise.race`, `any`, `all`): a
/// started promise among them that is dropped unfinished and rejects later is not reported as an
/// unhandled rejection, like in JS; `quiet_drop` disposes of its result slot (null if nothing to
/// drop). Call before handing the futures over.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_futs_handled(
    futs: *const *mut VeltFut,
    n: u64,
    quiet_drop: Option<ResultDropFn>,
) {
    for i in 0..n as usize {
        node::mark_handled(*futs.add(i), quiet_drop);
        crate::task::spawn::mark_join_handled(*futs.add(i), quiet_drop);
    }
}

/// A unique id of the task being polled (0 outside a task): assigned on first use from a global
/// counter, never reused, and kept by the task's local promises when they outlive it (they stay
/// on that one logical task). `new Promise` compares it to decide whether a resolved value stays
/// on its task or must be copied for another one (stage 2: counted objects never cross tasks,
/// docs/internals/design/semantics-stage2.md §6).
#[no_mangle]
pub extern "C" fn velt_rt_task_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let tcx = CURRENT.with(Cell::get);
    if tcx.is_null() {
        return 0;
    }
    // SAFETY: `CURRENT` is only set while its `TaskCx` lives (`Enter`), and `id` points into the
    // task's own `SetPtr`, which only this task's polls touch.
    unsafe {
        let id = &mut *(*tcx).id;
        if *id == 0 {
            *id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        *id
    }
}
