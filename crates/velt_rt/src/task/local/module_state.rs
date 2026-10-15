//! Module state (module-level `let`s and stored constants, rt_abi.md "Module state"): one block
//! per process, as in Node, allocated zeroed and initialized by the program's `init` before
//! `main` runs.
//!
//! Every task sees the same block, and tasks run on several threads, so the block (and every
//! object reached through it, whose reference counts are not atomic) is guarded by one
//! process-wide lock, the *module lock*. A task takes it on its first access and keeps it until
//! the end of that poll, so a task's code between two `await`s runs alone with respect to every
//! other task that uses module state, exactly as all code does in Node. A task that used module
//! state once may still hold objects it got from there, so it takes the lock at the start of
//! each later poll too (it is *touched*). Tasks that never touch module state never take it, and
//! a program without module state never sets [`USED`], which every per-task step tests first.
//!
//! The lock is never held across an `await` (a poll ends there), and it is never taken while a
//! `Mutex` is held: the compiler rejects module state in a `with` callback, and taking it there
//! anyway stops with an `ICE:`. A thread may block on a `Mutex` while it holds the module lock
//! (the `Mutex`'s holder never needs the module lock), so lock order never cycles.
//!
//! Code outside a task (a synchronous `main`) keeps the lock from its first access until it
//! hands over to the tasks (`velt_rt_block_on`, or `main` returned): tasks wait until then, as
//! in Node, where nothing else runs before the synchronous code is done.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::lock_api::RawMutex as _;
use parking_lot::RawMutex;

/// What the compiler emits once per program that has module state.
#[repr(C)]
pub struct ModuleDesc {
    pub size: u64,
    pub align: u64,
    /// Evaluates the initializers in module order and writes every slot, calling
    /// `velt_rt_module_ready` after each.
    pub init: unsafe extern "C" fn(*mut u8),
    /// Drops every slot (the state tuple's drop glue).
    pub drop: unsafe extern "C" fn(*mut u8),
    /// Each slot's name, NUL-terminated, in slot order.
    pub names: *const u8,
}

/// The module lock.
static LOCK: RawMutex = RawMutex::INIT;

/// Some module state exists: set by the first `velt_rt_module_state` call, which runs before
/// `main`. Until then no task can have touched module state.
static USED: AtomicBool = AtomicBool::new(false);

/// Does the program use module state (has the block been asked for)?
#[inline]
pub(crate) fn used() -> bool {
    USED.load(Ordering::Relaxed)
}

/// One block: its descriptor, its memory, and how many of its slots are initialized (`u64::MAX`
/// once `init` returned).
struct Block {
    desc: *const ModuleDesc,
    p: *mut u8,
    ready: u64,
}

/// The blocks, by descriptor: normally one; code swapped in by `velt dev` with a state layout of
/// its own adds one (its module-level `let`s start over, while old code keeps the old block).
/// Only touched with [`LOCK`] held.
struct Blocks(UnsafeCell<Vec<Block>>);

// SAFETY: only used with `LOCK` held.
unsafe impl Sync for Blocks {}

static BLOCKS: Blocks = Blocks(UnsafeCell::new(Vec::new()));

/// How this thread holds the lock.
const NOT_HELD: u8 = 0;
/// Held for the task poll that took it (released when the poll ends).
const IN_POLL: u8 = 1;
/// Held by code outside a task (released by [`release_outside`]).
const OUTSIDE: u8 = 2;

thread_local! {
    static HELD: Cell<u8> = const { Cell::new(NOT_HELD) };
    /// `Mutex`es this thread holds (counted only when [`USED`]).
    static MUTEXES: Cell<u32> = const { Cell::new(0) };
}

/// The bit of a task's turn word (its `SetPtr`) that says it touched module state. Turns stay
/// below it (`node::TURNS`).
pub(super) const TOUCHED: u32 = 1 << 31;

/// Per-poll bookkeeping, kept in the task's `TaskCx`.
pub(super) struct PollState {
    /// The task's turn word, whose [`TOUCHED`] bit is the task's flag (points into its
    /// `SetPtr`).
    word: *mut u32,
    /// This poll took the lock.
    took: Cell<bool>,
}

impl PollState {
    /// Start a poll of the task whose turn word is `word`: a touched task takes the lock first
    /// (unless this thread holds it already).
    #[inline]
    pub(super) fn enter(word: &mut u32) -> PollState {
        let took = *word & TOUCHED != 0 && used() && take_if_free();
        PollState {
            word,
            took: Cell::new(took),
        }
    }
}

/// The lock while a task's state is dropped outside a poll (a cancelled task), when the task
/// (turn word `word`) touched module state.
#[inline]
pub(super) fn drop_guard(word: u32) -> Option<Guard> {
    (word & TOUCHED != 0 && used() && take_if_free()).then_some(Guard)
}

impl Drop for PollState {
    /// End the poll: release the lock if this poll took it.
    #[inline]
    fn drop(&mut self) {
        if self.took.get() {
            release(IN_POLL);
        }
    }
}

/// Releases the lock when dropped.
pub(super) struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        release(IN_POLL);
    }
}

/// Take the lock for a poll unless this thread holds it already; whether it was taken.
fn take_if_free() -> bool {
    HELD.with(|h| {
        if h.get() != NOT_HELD {
            return false;
        }
        LOCK.lock();
        h.set(IN_POLL);
        true
    })
}

fn release(how: u8) {
    HELD.with(|h| {
        if h.get() == how {
            h.set(NOT_HELD);
            // SAFETY: this thread holds the lock (`HELD`).
            unsafe { LOCK.unlock() };
        }
    });
}

/// Code outside a task is done for now (`main` returned, or the program goes on in tasks): let
/// the tasks have the lock.
pub(crate) fn release_outside() {
    if used() {
        release(OUTSIDE);
    }
}

/// This thread locked a `Mutex` (`velt_rt_mutex_lock`).
#[inline]
pub(crate) fn mutex_locked() {
    if used() {
        MUTEXES.with(|m| m.set(m.get() + 1));
    }
}

/// This thread unlocked a `Mutex` (`velt_rt_mutex_unlock`).
#[inline]
pub(crate) fn mutex_unlocked() {
    if used() {
        MUTEXES.with(|m| m.set(m.get().saturating_sub(1)));
    }
}

/// About to take the module lock: never while a `Mutex` is held, which could wait for a thread
/// that holds the module lock and waits for that `Mutex`.
fn no_mutex_held() {
    if MUTEXES.with(Cell::get) != 0 {
        crate::panic::fatal(
            "ICE: module state reached while a `Mutex` is held (in a `with` callback)",
        );
    }
}

/// Drop the module state at exit when every task is done (the leak counters of the debug runtime
/// see what it held as freed). The lock stays held: a task still running waits instead of
/// reaching freed state.
pub(crate) fn drop_at_exit() {
    if !used() {
        return;
    }
    if HELD.with(Cell::get) == NOT_HELD {
        LOCK.lock();
        HELD.with(|h| h.set(OUTSIDE));
    }
    // SAFETY: the lock is held.
    let blocks = std::mem::take(unsafe { &mut *BLOCKS.0.get() });
    for b in blocks {
        // SAFETY: allocated in `block` with this descriptor's layout and initialized by `init`.
        unsafe {
            let d = &*b.desc;
            (d.drop)(b.p);
            dealloc(b.p, layout(d));
        }
    }
}

/// The block of `desc`, allocated and initialized on first use, to reach slot `slot`. The lock
/// must be held.
#[inline]
unsafe fn block(desc: *const ModuleDesc, slot: u64) -> *mut u8 {
    // Short borrows only: `init` reaches here again.
    let found = (*BLOCKS.0.get())
        .iter()
        .rev()
        .find(|b| b.desc == desc)
        .map(|b| (b.p, b.ready));
    let (p, ready) = match found {
        Some(f) => f,
        None => (new_block(desc), u64::MAX),
    };
    if slot >= ready {
        not_ready(desc, slot);
    }
    p
}

/// Allocate and initialize the block of `desc`.
#[cold]
unsafe fn new_block(desc: *const ModuleDesc) -> *mut u8 {
    let d = &*desc;
    let layout = layout(d);
    let p = alloc_zeroed(layout);
    if p.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    // Recorded before `init` runs: the initializers read the slots written before them.
    (*BLOCKS.0.get()).push(Block { desc, p, ready: 0 });
    (d.init)(p);
    if let Some(b) = (*BLOCKS.0.get()).iter_mut().find(|b| b.p == p) {
        b.ready = u64::MAX;
    }
    p
}

/// Slot `slot` of `desc`'s block is reached before its initializer ran: what JavaScript reports
/// as an uncaught `ReferenceError` (here it cannot be caught).
#[cold]
unsafe fn not_ready(desc: *const ModuleDesc, slot: u64) -> ! {
    let mut name = String::new();
    let mut s = (*desc).names;
    if !s.is_null() {
        for _ in 0..slot {
            while *s != 0 {
                s = s.add(1);
            }
            s = s.add(1);
        }
        name = std::ffi::CStr::from_ptr(s.cast())
            .to_string_lossy()
            .into_owned();
    }
    crate::io::try_flush_stdout();
    eprintln!("Uncaught ReferenceError: Cannot access '{name}' before initialization");
    std::process::exit(1)
}

fn layout(d: &ModuleDesc) -> Layout {
    Layout::from_size_align((d.size as usize).max(1), (d.align as usize).max(1))
        .unwrap_or_else(|_| crate::panic::fatal("ICE: module state layout"))
}

/// The program's module state, initialized on first access by `desc.init`, to reach slot
/// `slot` (a slot whose initializer has not run yet is a `ReferenceError`). Takes the module
/// lock unless this thread holds it: for the rest of the task's poll (and every later poll of
/// the task), or outside a task until the code there is done.
///
/// # Safety
/// `desc` must point to the program's module descriptor.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_module_state(desc: *const ModuleDesc, slot: u64) -> *mut u8 {
    let tcx = super::CURRENT.with(Cell::get);
    match tcx.as_ref() {
        Some(t) => {
            let ps = &*t.module;
            if !ps.took.get() {
                no_mutex_held();
                USED.store(true, Ordering::Relaxed);
                *ps.word |= TOUCHED;
                if take_if_free() {
                    ps.took.set(true);
                }
            }
        }
        None => HELD.with(|h| {
            if h.get() == NOT_HELD {
                no_mutex_held();
                USED.store(true, Ordering::Relaxed);
                LOCK.lock();
                h.set(OUTSIDE);
            }
        }),
    }
    block(desc, slot)
}

/// The initializers of `state`'s slots below `n` have run: those slots may be read.
///
/// # Safety
/// `state` must be a block being initialized (`ModuleDesc::init`), with the lock held.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_module_ready(state: *mut u8, n: u64) {
    if let Some(b) = (*BLOCKS.0.get()).iter_mut().find(|b| b.p == state) {
        b.ready = n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poll_takes_the_lock_only_for_a_touched_task() {
        USED.store(true, Ordering::Relaxed);
        let mut word = 3;
        drop(PollState::enter(&mut word));
        assert_eq!(HELD.with(Cell::get), NOT_HELD);
        word |= TOUCHED;
        let ps = PollState::enter(&mut word);
        assert_eq!(HELD.with(Cell::get), IN_POLL);
        drop(ps);
        assert_eq!(HELD.with(Cell::get), NOT_HELD);
    }
}
