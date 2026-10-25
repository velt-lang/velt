//! Read mode: what lets `RwLock.read` callbacks run on many threads at once without copying or
//! counting anything (docs/internals/contracts/rt_abi_async.md "Read mode").
//!
//! A thread is in read mode while it runs a `read` callback (`velt_rt_rwlock_read` to
//! `velt_rt_rwlock_read_unlock`, nested ones counted). A `with` or `write` there stops the
//! program ([`refuse_in_read`]): its callback could not tell the store's uncounted objects from
//! what it changes. The *lock-set* types are the counted types a value inside an `RwLock` can
//! reach. A compiled function that counts or writes their objects asks `velt_rt_lk_mode` once
//! whether its thread is in read mode, and counts inline by the rules below (velt_vir `rc.rs`).
//! Their reference counts are plain (non-atomic) words, written by one thread at a time:
//! - while readers run, no writer does (the lock), and a reader changes the count of no object it
//!   did not make itself: a store object (one without [`LOCAL`]) is borrowed, never counted, so
//!   its count stays what the last writer left;
//! - an object a reader makes is [`LOCAL`]: only this thread reaches it, and it is counted as
//!   usual. One that leaves the callback (its result, transferred) loses the bit (the compiler
//!   clears it inline);
//! - a reader that writes a store object, through any reference, stops the program: other
//!   readers read it at the same time (sema rejects the writes it can see).

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::hazard::{FREES_WAIT, READER};

/// Set in the count word of a lock-set object made in read mode.
pub const LOCAL: u64 = 1 << 62;

// This thread's state is in `hazard::READER` (`read_depth`: the read callbacks it is in,
// which `Reader::push` and `pop` count).

/// 1 when this thread is in read mode, else 0. A function asks once, when it first counts or
/// writes a lock-set object (a thread enters or leaves read mode only around a `read`
/// callback, which is a function of its own); with 0 it counts them inline.
#[no_mangle]
pub extern "C" fn velt_rt_lk_mode() -> u8 {
    u8::from(in_read())
}

/// In read mode?
#[inline]
fn in_read() -> bool {
    READER.with(|r| r.read_depth.get()) != 0
}

/// A drop glue function: releases the reference at the pointer it is given.
type DropGlue = unsafe extern "C" fn(*mut *mut u8);

thread_local! {
    /// Objects whose last reference went in read mode, with their drop glue: freed when this
    /// thread's reads end (`velt_rt_free_after_reads`).
    static WAITING: RefCell<Vec<(*mut u8, DropGlue)>> = const { RefCell::new(Vec::new()) };
}

/// The last reference to the lock-set object `p` goes, in code that changes the caller's
/// containers inside a `read` callback (velt_vir `LkMode::Deferred`): free it with `drop` (its
/// type's drop glue, given a pointer to `p`, whose count is still 1) now, or, in read mode,
/// once this thread's outermost read ends. A reader may still point at it without counting it
/// (a variable read out of the caller's array the callback changes).
///
/// # Safety
/// `p` is a counted object whose count is 1 and `drop` its drop glue.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_free_after_reads(p: *mut u8, drop: DropGlue) {
    if !in_read() {
        let mut slot = p;
        return drop(&mut slot);
    }
    WAITING.with(|w| w.borrow_mut().push((p, drop)));
    READER.with(|r| r.read_depth.set(r.read_depth.get() | FREES_WAIT));
}

/// This thread's outermost read ended: free what waited (`velt_rt_free_after_reads`).
#[cold]
#[inline(never)]
pub(crate) fn free_waiting() {
    while let Some((p, drop)) = WAITING.with(|w| w.borrow_mut().pop()) {
        let mut slot = p;
        // SAFETY: `velt_rt_free_after_reads` took it with its count at 1 and its drop glue.
        unsafe { drop(&mut slot) };
    }
}

/// Set when the program makes its first `RwLock` (before that no thread is in read mode).
static RWLOCKS: AtomicBool = AtomicBool::new(false);

/// An `RwLock` is made (`velt_rt_rwlock_init`).
pub(crate) fn rwlock_made() {
    if !RWLOCKS.load(Ordering::Relaxed) {
        RWLOCKS.store(true, Ordering::Relaxed);
    }
}

/// A promise or task starts: in read mode it stops the program. What it holds would outlive
/// the callback without leaving through its result, so objects the callback made (counted by
/// read mode's rules) or shares with the store could reach other threads and writers.
/// Inline it is one load and a branch: a program without an `RwLock` starts each promise without
/// reading this thread's reader state (which the compiler would otherwise read ahead of the test).
#[inline(always)]
pub(crate) fn refuse_tasks_in_read() {
    if RWLOCKS.load(Ordering::Relaxed) {
        refuse_tasks_with_rwlocks();
    }
}

/// [`refuse_tasks_in_read`] once the program has made an `RwLock`.
#[inline(never)]
fn refuse_tasks_with_rwlocks() {
    if in_read() {
        tasks_in_read();
    }
}

#[cold]
#[inline(never)]
fn tasks_in_read() -> ! {
    crate::panic::fatal(
        "a promise or task cannot start inside a `read` callback: what it holds would outlive the callback, which shares the `RwLock`'s value without counting it (start it after `read`, with what `read` returns)",
    )
}

/// A `with` or `write` (`method`) is about to lock: in read mode it stops the program.
pub(crate) fn refuse_in_read(method: &str) {
    if in_read() {
        crate::panic::fatal(&format!(
            "`{method}` cannot run inside a `read` callback, nor what locks (a promise's resolver, clearing a timer): the callback shares the `RwLock`'s value with other readers without counting it, which `{method}` could not tell from what it changes (return what you need from `read` and lock, resolve or clear after it)"
        ));
    }
}

/// About to write the lock-set object whose count word is at `p`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_lk_write(p: *mut u64) {
    if in_read() && *p & LOCAL == 0 {
        crate::panic::fatal(
            "cannot modify a value an `RwLock` holds inside its `read` callback (other readers read it at the same time): change it in `write`",
        );
    }
}

/// The reader's own lock-set object whose count word is at `p` (`LOCAL` set) is transferred
/// out of a `read` callback: it is no longer the reader's own once the thread leaves read mode,
/// so the bit goes, unless the thread stays in read mode after this callback (the result of a
/// `read` nested in another, which the outer reader then holds as one it made).
///
/// # Safety
/// `p` is the count word of a counted object.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_lk_leave(p: *mut u64) {
    if READER.with(|r| r.depth()) <= 1 {
        *p &= !LOCAL;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_mode_follows_read_callbacks() {
        assert_eq!(in_read() as u8, 0);
        refuse_in_read("with");
        READER.with(|r| r.push());
        assert_eq!(velt_rt_lk_mode(), 1);
        let mut mine = 1 | LOCAL;
        unsafe { velt_rt_lk_write(&mut mine) };
        // Nested: still the reader's own after the inner callback; outermost: no longer.
        READER.with(|r| r.push());
        unsafe { velt_rt_lk_leave(&mut mine) };
        assert_eq!(mine, 1 | LOCAL);
        READER.with(|r| r.pop());
        unsafe { velt_rt_lk_leave(&mut mine) };
        assert_eq!(mine, 1);
        READER.with(|r| r.pop());
        assert_eq!(in_read() as u8, 0);
        let mut store = 1u64;
        unsafe { velt_rt_lk_write(&mut store) };
    }
}
