//! Eager ("hybrid") promises: `velt_rt_fut_start` runs a stored promise's compiled state until
//! its first suspension right away, like calling an async function in JS, and from then on the
//! task that started it drives it as a *local* promise (https://veltlang.com/docs/reference/async/).
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

mod micro;
mod node;
mod set;

pub(crate) use micro::{settled, step_free, ticket_pending};

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use self::set::LocalSet;
use super::ResultDropFn;
use super::{DropFn, PollFn, VeltFut};

/// What `velt_rt_fut_start` needs from the task being polled.
struct TaskCx {
    /// The task's set (null until the first start). Raw pointers throughout: compiled code
    /// reached from the task's poll uses it re-entrantly.
    set: *mut *mut LocalSet,
    waker: *const Waker,
    /// The task's id for [`task_key`] (0 until first asked for).
    id: *mut u64,
    /// Which poll of the task this is ([`next_turn`]): a promise created during this same poll
    /// can only have been woken by the poll's own code, never by another event.
    turn: u32,
    /// The task's await word (micro.rs): its queued microtasks plus the synchronous runs under
    /// way. Generated code reads it through `cx` (`await_word`).
    word: Cell<u32>,
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

/// Which logical task is being polled (0 outside a task): a unique id assigned on first use from
/// a global counter, never reused, and kept by the task's local promises when they outlive its
/// root (the orphan task driving them has the same id). Shared promises (task/shared.rs) check
/// that they stay on one task; the debug cell check keys a cell's owner by it (cell_owner.rs).
/// Compiled code and std ask the executor in Velt instead (sched_id.velt).
pub(crate) fn task_key() -> u64 {
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

/// The turn of the task being polled (0 outside a task).
fn current_turn() -> u32 {
    let tcx = CURRENT.with(Cell::get);
    // SAFETY: `CURRENT` is only set while its `TaskCx` lives (`Enter`).
    unsafe { tcx.as_ref() }.map_or(0, |t| t.turn)
}

/// The turn after `turn`: turns count task polls in `1..node::TURNS` (0 means none).
fn next_turn(turn: u32) -> u32 {
    if turn + 1 >= node::TURNS {
        1
    } else {
        turn + 1
    }
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

/// Owned pointer to a task's local set (null = none yet), its id and its turn.
struct SetPtr(*mut LocalSet, u64, u32);

const _: () = assert!(std::mem::size_of::<SetPtr>() == 24);

// SAFETY: the set moves with its task between workers and is only used by that task's polls.
unsafe impl Send for SetPtr {}

impl SetPtr {
    const NONE: SetPtr = SetPtr(std::ptr::null_mut(), 0, 0);

    /// Poll with this set current: drain its woken promises (resuming the task's root with
    /// `root` when a promise it awaits finishes), then poll the root once more if a promise ran
    /// after its last poll, then run the microtasks that queued.
    ///
    /// # Safety
    /// `waker` must stay valid during the call.
    unsafe fn enter(&mut self, waker: *const Waker, root: &mut dyn FnMut()) {
        self.2 = next_turn(self.2);
        let tcx = TaskCx {
            set: &mut self.0,
            waker,
            id: &mut self.1,
            turn: self.2,
            word: Cell::new(self.0.as_ref().map_or(0, |s| s.micro.len() as u32)),
        };
        let _enter = Enter::new(&tcx);
        if !self.0.is_null() {
            // A live set owned by this task; compiled code run from the drain reaches it only
            // through `tcx`.
            set::drain(&mut set::Driver {
                set: self.0,
                task: &*waker,
                root,
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
                word: tcx.word.as_ptr(),
            });
        }
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
            drop(unsafe { Box::from_raw(set) });
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
pub unsafe extern "C-unwind" fn velt_rt_fut_box(
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
pub unsafe extern "C-unwind" fn velt_rt_fut_start(
    f: *mut VeltFut,
    result_drop: Option<ResultDropFn>,
) {
    start(f, result_drop);
}

unsafe fn start(f: *mut VeltFut, result_drop: Option<ResultDropFn>) {
    if !node::is_lazy(f) {
        return;
    }
    crate::read_mode::refuse_tasks_in_read();
    let tcx = CURRENT.with(Cell::get);
    if tcx.is_null() {
        return;
    }
    let set = task_set(&*tcx);
    node::stamp(f, (*tcx).turn);
    node::mark_started(f, &(*set).shared, result_drop);
    // The creator goes on after this first run, so the run's `await`s take their steps through
    // the queue (micro.rs).
    let t = &*tcx;
    micro::sync_enter(t);
    let finished = node::poll_first(f, t.word.as_ptr());
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
/// (#160). A started or lazy promise node keeps the function for when its state finishes;
/// anything else (a join handle, whose task already had its inputs transferred, or a runtime
/// leaf) produces nothing to transfer. A null `transfer` (a result with nothing to move or copy) only hands over a
/// holder of a shared promise (task/shared.rs).
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_fut_transfer(
    f: *mut VeltFut,
    transfer: Option<ResultDropFn>,
) {
    if crate::task::shared::is_shared(f) {
        return crate::task::shared::hand_over(f, transfer);
    }
    // Whatever its result needs, the promise is the other task's now (#1497).
    node::mark_handed_over(f);
    if let Some(t) = transfer {
        node::set_transfer(f, t);
    }
}

/// What `console.log` shows of promise `f`, which its owner holds: 1 when its result is in the
/// result slot (+16; a `Result` tag first for a promise that can reject), 0 while it is pending.
/// Nothing is polled, claimed or moved. A future that only runs when it is awaited (a runtime
/// leaf, a join handle, a combinator, a promise created outside a task) reads as pending.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_fut_peek(f: *mut VeltFut) -> u8 {
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

/// Where finished started promise `f` threw its error (unknown if it did not, or threw it in
/// the standard library).
pub(crate) unsafe fn node_loc(f: *mut VeltFut) -> crate::panic::ThrowLoc {
    node::loc_of(f)
}

/// The task that runs the promises code outside a task hands to tasks (a holder of a shared
/// promise given to a spawned call in a synchronous `main`, task/shared.rs): like any task it
/// has an id and a set, so the promise is shared on it. Code outside a task enters it for each
/// hand-over; once that code is done ([`outside_done`]), the set moves to an orphan task that
/// drives what is still running (until then a wake only queues it).
static OUTSIDE_HOME: parking_lot::Mutex<SetPtr> = parking_lot::Mutex::new(SetPtr::NONE);

/// Run `f` (outside a task) as a step of the task of [`OUTSIDE_HOME`].
pub(crate) fn in_outside_home(f: impl FnOnce()) {
    let mut home = OUTSIDE_HOME.lock();
    // Its promises may start timers and I/O, which need the runtime.
    let _rt = super::runtime::enter();
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
/// an orphan task ([`OUTSIDE_HOME`]).
pub(crate) fn outside_done() {
    let mut home = std::mem::replace(&mut *OUTSIDE_HOME.lock(), SetPtr::NONE);
    if let Some(set) = home.take_busy() {
        Orphan::spawn(set);
    }
}

/// Has started promise `f` finished?
pub(crate) unsafe fn node_done(f: *mut VeltFut) -> bool {
    node::is_done(f)
}
