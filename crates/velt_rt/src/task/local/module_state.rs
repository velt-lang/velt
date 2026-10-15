//! Module state (module-level `let`s and stored constants, rt_abi.md "Module state"): one block
//! per process, as in Node, allocated zeroed and initialized by the program's `init` on the
//! first access.
//!
//! Every task sees the same block, and tasks run on several threads, so the block (and every
//! object reached through it, whose reference counts are not atomic) is guarded by one
//! process-wide lock, the *module lock*. A task takes it on its first access and keeps it until
//! the end of that poll, so a task's code between two `await`s runs alone with respect to every
//! other task that uses module state, exactly as all code does in Node. A task that used module
//! state once may still hold objects it got from there, so it takes the lock at the start of
//! each later poll too (it is *touched*). Tasks that never touch module state never take it.
//! The lock is never held across an `await` (a poll ends there), and a thread that blocks on a
//! `Mutex` lets it go while it waits (`suspend` / `resume`), so lock order never cycles.
//!
//! Code outside a task (a synchronous `main`) keeps the lock from its first access until it
//! hands over to the tasks (`velt_rt_block_on`, or `main` returned): tasks wait until then, as
//! in Node, where nothing else runs before the synchronous code is done.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::cell::{Cell, UnsafeCell};

use parking_lot::lock_api::RawMutex as _;
use parking_lot::RawMutex;

/// What the compiler emits once per program that has module state.
#[repr(C)]
pub struct ModuleDesc {
    pub size: u64,
    pub align: u64,
    /// Evaluates the initializers in module order and writes every slot.
    pub init: unsafe extern "C" fn(*mut u8),
    /// Drops every slot (the state tuple's drop glue).
    pub drop: unsafe extern "C" fn(*mut u8),
}

/// The module lock.
static LOCK: RawMutex = RawMutex::INIT;

/// The blocks, by descriptor: normally one; code swapped in by `velt dev` with a state layout of
/// its own adds one (its module-level `let`s start over, while old code keeps the old block).
/// Only touched with [`LOCK`] held.
struct Blocks(UnsafeCell<Vec<(*const ModuleDesc, *mut u8)>>);

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
}

/// A task's part: whether it ever touched module state, and whether the current poll took the
/// lock (and so releases it when it ends).
#[derive(Default)]
pub(super) struct TaskState {
    touched: bool,
}

/// Per-poll bookkeeping, kept in the task's `TaskCx`.
pub(super) struct PollState {
    /// The task's flag (points into its `SetPtr`).
    touched: *mut bool,
    /// This poll took the lock.
    took: Cell<bool>,
}

impl TaskState {
    pub(super) const NEW: TaskState = TaskState { touched: false };

    /// Start a poll: a touched task takes the lock first (unless this thread holds it already).
    pub(super) fn enter(&mut self) -> PollState {
        let took = self.touched && take_if_free();
        PollState {
            touched: &mut self.touched,
            took: Cell::new(took),
        }
    }

    /// The lock while the task's state is dropped outside a poll (a cancelled task), when the
    /// task touched module state.
    pub(super) fn drop_guard(&self) -> Option<Guard> {
        (self.touched && take_if_free()).then_some(Guard)
    }
}

impl Drop for PollState {
    /// End the poll: release the lock if this poll took it.
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
    release(OUTSIDE);
}

/// About to block on another lock: let the module lock go meanwhile (other threads may need it to
/// release the one this thread waits for). Returns how it was held, for [`resume`].
pub(crate) fn suspend() -> u8 {
    HELD.with(|h| {
        let how = h.replace(NOT_HELD);
        if how != NOT_HELD {
            // SAFETY: this thread holds the lock (`HELD`).
            unsafe { LOCK.unlock() };
        }
        how
    })
}

/// Take the module lock back after [`suspend`].
pub(crate) fn resume(how: u8) {
    if how != NOT_HELD {
        LOCK.lock();
        HELD.with(|h| h.set(how));
    }
}

/// Drop the module state at exit when every task is done (the leak counters of the debug runtime
/// see what it held as freed). The lock stays held: a task still running waits instead of
/// reaching freed state.
pub(crate) fn drop_at_exit() {
    if HELD.with(Cell::get) == NOT_HELD {
        LOCK.lock();
        HELD.with(|h| h.set(OUTSIDE));
    }
    // SAFETY: the lock is held.
    let blocks = std::mem::take(unsafe { &mut *BLOCKS.0.get() });
    for (desc, p) in blocks {
        // SAFETY: allocated in `block` with this descriptor's layout and initialized by `init`.
        unsafe {
            let d = &*desc;
            (d.drop)(p);
            dealloc(p, layout(d));
        }
    }
}

/// The block of `desc`, allocated and initialized on first use. The lock must be held.
unsafe fn block(desc: *const ModuleDesc) -> *mut u8 {
    let blocks = &mut *BLOCKS.0.get();
    if let Some(&(d, p)) = blocks.last() {
        if d == desc {
            return p;
        }
    }
    if let Some(&(_, p)) = blocks.iter().find(|(d, _)| *d == desc) {
        return p;
    }
    let d = &*desc;
    let layout = layout(d);
    let p = alloc_zeroed(layout);
    if p.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    // Recorded before `init` runs: the initializers read the slots written before them.
    blocks.push((desc, p));
    (d.init)(p);
    p
}

fn layout(d: &ModuleDesc) -> Layout {
    Layout::from_size_align((d.size as usize).max(1), (d.align as usize).max(1))
        .unwrap_or_else(|_| crate::panic::fatal("ICE: module state layout"))
}

/// The program's module state, initialized on first access by `desc.init`. Takes the module lock
/// unless this thread holds it: for the rest of the task's poll (and every later poll of the
/// task), or outside a task until the code there is done.
///
/// # Safety
/// `desc` must point to the program's module descriptor.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_module_state(desc: *const ModuleDesc) -> *mut u8 {
    let tcx = super::CURRENT.with(Cell::get);
    match tcx.as_ref() {
        Some(t) => {
            let ps = &*t.module;
            if !*ps.touched || !ps.took.get() {
                *ps.touched = true;
                if take_if_free() {
                    ps.took.set(true);
                }
            }
        }
        None => HELD.with(|h| {
            if h.get() == NOT_HELD {
                LOCK.lock();
                h.set(OUTSIDE);
            }
        }),
    }
    block(desc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspend_and_resume_hand_the_lock_back() {
        assert!(take_if_free());
        let how = suspend();
        assert_eq!(how, IN_POLL);
        // Another thread can take it meanwhile.
        std::thread::spawn(|| {
            assert!(take_if_free());
            release(IN_POLL);
        })
        .join()
        .unwrap();
        resume(how);
        assert!(!take_if_free(), "held again by this thread");
        release(IN_POLL);
        assert_eq!(HELD.with(Cell::get), NOT_HELD);
    }
}
