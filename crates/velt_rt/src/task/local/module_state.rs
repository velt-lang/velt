//! The module lock of what the runtime keeps per process for compiled code (a declared function's
//! identity and its properties, fn_props.rs), as std's lock guards module state itself
//! (module-level `let`s and stored constants, std/lang/hooks/sched_module.velt and
//! module_blocks.velt, rt_abi.md "Module state"; #2196 9c moved the blocks to std).
//!
//! Every task sees the same state, and tasks run on several threads, so the state (and every
//! object reached through it, whose reference counts are not atomic) is guarded by one
//! process-wide lock, the *module lock*. A task takes it on its first access and keeps it until
//! the end of that poll, so a task's code between two `await`s runs alone with respect to every
//! other task that uses module state, exactly as all code does in Node. A task that used module
//! state once may still hold objects it got from there, so it takes the lock at the start of
//! each later poll too (it is *touched*). Tasks that never touch module state never take it, and
//! a program without module state never sets [`USED`], which every per-task step tests first.
//!
//! The lock is never held across an `await` (a poll ends there), and it is never waited for
//! while a `Mutex` is held: the compiler rejects module state in a `with` callback, and waiting
//! for it there anyway stops with an `ICE:`. A thread may block on a `Mutex` while it holds the
//! module lock (the `Mutex`'s holder never needs the module lock), so lock order never cycles.
//! A program that may give a declared function properties through `any` (fn_props.rs: they are
//! module state, and the compiler cannot see where a callback reaches them) takes the module
//! lock before each `Mutex` or `RwLock` instead ([`velt_rt_module_with`]), for the callback,
//! so a callback holds it already and never waits for it: the lock order is the module lock,
//! then the `Mutex`.
//!
//! Code outside a task (a synchronous `main`) keeps the lock from its first access until it
//! hands over to the tasks (`main`'s root starts, or `main` returned): tasks wait until then, as
//! in Node, where nothing else runs before the synchronous code is done.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::lock_api::RawMutex as _;
use parking_lot::RawMutex;

/// The module lock.
static LOCK: RawMutex = RawMutex::INIT;

/// The module lock was taken for what the runtime keeps per process ([`touch`]). Until then no
/// task can have touched it.
static USED: AtomicBool = AtomicBool::new(false);

/// Does the program use module state (has the block been asked for)?
#[inline]
pub(crate) fn used() -> bool {
    USED.load(Ordering::Relaxed)
}

/// How this thread holds the lock.
const NOT_HELD: u8 = 0;
/// Held for the task poll that took it (released when the poll ends).
const IN_POLL: u8 = 1;
/// Held by code outside a task (released by [`release_outside`]).
const OUTSIDE: u8 = 2;
/// Held for a `with`, `read` or `write` callback ([`velt_rt_module_with`]), released after its
/// lock is ([`velt_rt_module_with_end`]), unless the callback reached module state: the poll or
/// the code outside a task keeps it then, as for any access.
const IN_WITH: u8 = 3;

thread_local! {
    static HELD: Cell<u8> = const { Cell::new(NOT_HELD) };
    /// `Mutex`es this thread holds (counted only when [`USED`]).
    static MUTEXES: Cell<u32> = const { Cell::new(0) };
}

/// A task's part of a poll (in its `TaskCx`): the task's flag "touched module state" (in its
/// `SetPtr`), and whether this poll took the lock (released when the poll ends).
pub(super) struct PollState {
    pub(super) touched: *mut bool,
    pub(super) took: Cell<bool>,
}

/// Start a poll of a task whose flag is `touched`: a touched task takes the lock first (unless
/// this thread holds it already). Whether it took it, for [`end_poll`].
#[inline]
pub(super) fn start_poll(touched: bool) -> bool {
    touched && take_if_free()
}

/// End a poll that took the lock.
#[inline]
pub(super) fn end_poll(took: bool) {
    if took {
        release(IN_POLL);
    }
}

/// Run `f`, which drops a touched task's state outside a poll (a cancelled task), with the lock.
#[cold]
#[inline(never)]
pub(super) fn with_lock(f: impl FnOnce()) {
    let took = take_if_free();
    f();
    end_poll(took);
}

/// Take the lock for a poll unless this thread holds it already; whether it was taken. Out of
/// line: the task paths that may call it stay small for programs without module state.
#[cold]
#[inline(never)]
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

#[cold]
#[inline(never)]
fn release(how: u8) {
    HELD.with(|h| {
        if h.get() == how {
            h.set(NOT_HELD);
            // SAFETY: this thread holds the lock (`HELD`).
            unsafe { LOCK.unlock() };
        }
    });
}

/// Does this thread hold the lock (for a task's poll, or outside a task)? Every task that
/// reaches module state holds it while it runs, so values from there (a shared promise kept in a
/// `Map`, task/shared.rs) may move between those tasks.
pub(crate) fn held() -> bool {
    used() && matches!(HELD.with(Cell::get), IN_POLL | OUTSIDE)
}

/// Code outside a task is done for now (`main` returned, or the program goes on in tasks): let
/// the tasks have the lock.
pub(crate) fn release_outside() {
    if used() {
        release(OUTSIDE);
    }
}

/// This thread took an `RwLock` (`velt_rt_rwlock_read`, `velt_rt_rwlock_write`). A `Mutex`'s
/// lock is std's on a native target, which counts nothing (std/lang/hooks/mutex.velt).
#[inline]
pub(crate) fn mutex_locked() {
    if used() {
        MUTEXES.with(|m| m.set(m.get() + 1));
    }
}

/// This thread gave an `RwLock` back.
#[inline]
pub(crate) fn mutex_unlocked() {
    if used() {
        MUTEXES.with(|m| m.set(m.get().saturating_sub(1)));
    }
}

/// This thread's runtime-level holds: how it holds the module lock and how many `Mutex`es it
/// counts. Saved when a poll that catches panics starts (node/panicked.rs).
#[derive(Clone, Copy)]
pub(crate) struct Holds {
    held: u8,
    mutexes: u32,
}

/// This thread's holds now.
pub(crate) fn holds() -> Holds {
    Holds {
        held: HELD.with(Cell::get),
        mutexes: MUTEXES.with(Cell::get),
    }
}

/// A panic was caught in a poll that started with `saved`: the code it abandoned never ran its
/// unlocks (an `RwLock`'s, [`velt_rt_module_with_end`]), so the lock count goes back,
/// and the module lock taken for its `with` is released. The program's `Mutex` itself stays
/// locked (documented with `catchPanic`); the runtime's bookkeeping must not stay wrong, or a
/// later module-state access would end in an ICE and other threads would wait for good.
pub(crate) fn restore_holds(saved: Holds) {
    MUTEXES.with(|m| m.set(saved.mutexes));
    if saved.held != IN_WITH {
        release(IN_WITH);
    }
}

/// Before a `Mutex` or `RwLock` is locked for a callback that may reach a declared function's
/// properties held as `any` (fn_props.rs; the compiler decides which): take the module lock
/// unless this thread holds it, until [`velt_rt_module_with_end`] after the unlock. The callback
/// then never waits for the module lock while it holds the `Mutex`, which a thread holding the
/// module lock may be waiting for. Whether it took it (1), for the end.
#[no_mangle]
pub extern "C-unwind" fn velt_rt_module_with() -> u8 {
    HELD.with(|h| match h.get() {
        NOT_HELD => {
            take_for_with(h);
            1
        }
        _ => 0,
    })
}

/// After the unlock that followed [`velt_rt_module_with`], with what it answered: the module
/// lock it took goes, unless the callback reached module state (the poll keeps it then).
#[no_mangle]
pub extern "C-unwind" fn velt_rt_module_with_end(took: u8) {
    if took != 0 {
        release(IN_WITH);
    }
}

/// [`velt_rt_module_with`] when this thread does not hold the lock.
#[inline(never)]
fn take_for_with(h: &Cell<u8>) {
    if MUTEXES.with(Cell::get) != 0 && used() {
        // A lock taken without `velt_rt_module_with` (code swapped in by `velt dev`): waiting
        // here could wait for a thread that waits for that lock.
        no_mutex_held();
    }
    USED.store(true, Ordering::Relaxed);
    LOCK.lock();
    h.set(IN_WITH);
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

/// Take the module lock as an access of module state does: for what the runtime keeps per
/// process for compiled code, whose counts are not atomic (a declared function's identity and
/// its properties, fn_props.rs).
pub(crate) fn touch() {
    // SAFETY: as for an access of module state.
    unsafe { lock_for_access() }
}

/// Take the module lock for an access unless this thread holds it ([`touch`]).
#[inline]
unsafe fn lock_for_access() {
    let tcx = super::CURRENT.with(Cell::get);
    match tcx.as_ref() {
        Some(t) => {
            let ps = &t.module;
            if !ps.took.get() {
                USED.store(true, Ordering::Relaxed);
                *ps.touched = true;
                HELD.with(|h| match h.get() {
                    NOT_HELD => {
                        no_mutex_held();
                        LOCK.lock();
                        h.set(IN_POLL);
                        ps.took.set(true);
                    }
                    // A callback's: the poll keeps it from now on.
                    IN_WITH => {
                        h.set(IN_POLL);
                        ps.took.set(true);
                    }
                    _ => {}
                });
            }
        }
        None => HELD.with(|h| match h.get() {
            NOT_HELD => {
                no_mutex_held();
                USED.store(true, Ordering::Relaxed);
                LOCK.lock();
                h.set(OUTSIDE);
            }
            IN_WITH => h.set(OUTSIDE),
            _ => {}
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poll_takes_the_lock_only_for_a_touched_task() {
        end_poll(start_poll(false));
        assert_eq!(HELD.with(Cell::get), NOT_HELD);
        let took = start_poll(true);
        assert!(took);
        assert_eq!(HELD.with(Cell::get), IN_POLL);
        end_poll(took);
        assert_eq!(HELD.with(Cell::get), NOT_HELD);
    }

    /// A `with` in a program whose callbacks may reach a declared function's properties takes
    /// the lock before its `Mutex` and gives it back with it (the outermost one), unless the
    /// callback reached module state, which then keeps it as any access does.
    #[test]
    fn a_with_holds_the_lock_while_its_mutex_is_held() {
        USED.store(true, Ordering::Relaxed);
        let outer = velt_rt_module_with();
        assert_eq!((outer, HELD.with(Cell::get)), (1, IN_WITH));
        mutex_locked();
        let inner = velt_rt_module_with();
        mutex_locked();
        mutex_unlocked();
        velt_rt_module_with_end(inner);
        assert_eq!(HELD.with(Cell::get), IN_WITH, "a nested `with` leaves it");
        mutex_unlocked();
        velt_rt_module_with_end(outer);
        assert_eq!(
            HELD.with(Cell::get),
            NOT_HELD,
            "the outermost gives it back"
        );
        assert!(!held(), "a callback's lock is not a poll's");
        let took = velt_rt_module_with();
        mutex_locked();
        // SAFETY: as for an access of module state, outside a task.
        unsafe { lock_for_access() };
        mutex_unlocked();
        velt_rt_module_with_end(took);
        assert_eq!(
            HELD.with(Cell::get),
            OUTSIDE,
            "reached in the callback: kept"
        );
        release_outside();
        assert_eq!(HELD.with(Cell::get), NOT_HELD);
    }

    /// Code outside a task may put holders of the outside home's promises in module state, where
    /// tasks holding the lock use them: the home's polls take the lock too (task/shared.rs).
    #[test]
    fn the_outside_home_polls_with_the_lock_in_a_program_with_module_state() {
        // As the first `velt_rt_module_state` call does; no other test depends on it being
        // unset (a task takes the lock only once it touched module state).
        USED.store(true, Ordering::Relaxed);
        let mut inside = false;
        super::super::in_outside_home(|| inside = held());
        assert!(inside, "the home's step runs with the module lock");
        assert!(
            super::super::OUTSIDE_HOME.lock().4,
            "the home is a touched task"
        );
        assert_eq!(HELD.with(Cell::get), NOT_HELD, "and gives the lock back");
    }
}
