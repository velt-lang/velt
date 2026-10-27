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

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::hazard::{FREES_WAIT, READER};

/// Set in the count word of a lock-set object made in read mode.
pub const LOCAL: u64 = 1 << 62;

/// Set, with [`LOCAL`], in the count word of an object a reader made and stored into a
/// container of the code outside every read while still referencing it
/// ([`velt_rt_lk_share_made`]): the caller's from then on, so a write through the readers'
/// references stops the program ([`velt_rt_lk_write`]), while they keep counting them as their
/// own until this thread's outermost read ends. It counts as part of the count, so the object
/// is never unique meanwhile.
pub const STORED: u64 = 1 << 61;

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
    /// Objects a nested read stored into code outside every read, with their count words and
    /// drop glue: they lose `LOCAL` when this thread's reads end (`velt_rt_lk_leave`).
    static LEAVING: RefCell<Vec<(*mut u64, *mut u8, DropGlue)>> = const { RefCell::new(Vec::new()) };
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

/// This thread's outermost read ended: what a nested read stored outside every read is no
/// longer the reader's own (`velt_rt_lk_leave`), and what waited is freed
/// (`velt_rt_free_after_reads`).
#[cold]
#[inline(never)]
pub(crate) fn free_waiting() {
    while let Some((c, p, drop)) = LEAVING.with(|w| w.borrow_mut().pop()) {
        let mut slot = p;
        // SAFETY: `velt_rt_lk_leave` took it with one more reference, its count word and its
        // drop glue, which releases that reference (outside read mode now).
        unsafe {
            *c &= !(LOCAL | STORED);
            drop(&mut slot);
        }
    }
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
    if in_read() && *p & STORED != 0 {
        crate::panic::fatal("cannot change an object a `read` callback made after storing it outside the callback: it is the caller's from then on, as the same object (store it once it is complete, or change it through the caller's container after `read`)");
    }
    if in_read() && *p & LOCAL == 0 {
        crate::panic::fatal(
            "cannot modify a value an `RwLock` holds inside its `read` callback (other readers read it at the same time): change it in `write`",
        );
    }
}

/// The reader's own lock-set object `obj`, whose count word is at `p` (`LOCAL` set), is
/// transferred out of a `read` callback: it is no longer the reader's own once it reaches code
/// outside read mode, so the bit goes, unless the frame it goes to still runs in read mode. That
/// frame is [`velt_rt_lk_leave_to`]'s number of `read` callbacks out from the one running (1
/// unless the compiler says otherwise: a callback's result goes to the code that called `read`,
/// still in read mode when that `read` is nested in another, and the outer reader holds it as one
/// it made; 0: it stays in the running callback). One a nested callback stores into a container
/// of code outside every read keeps the bit until this thread's outermost read ends: the
/// callbacks around it still run and may change it through their own variables, as in
/// JavaScript, and only this thread reaches it meanwhile. It waits with one more reference,
/// which `drop` (its drop glue) releases once the bit is gone. Transferred again before then
/// it keeps the bit ([`leaving`]).
///
/// # Safety
/// `p` is the count word of the counted object `obj`, and `drop` its drop glue.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_lk_leave(p: *mut u64, obj: *mut u8, drop: DropGlue) {
    let depth = READER.with(|r| r.depth()) as u64;
    if depth > LEAVE_TO.with(Cell::get) {
        return;
    }
    if depth <= 1 {
        if !leaving(p) {
            *p &= !LOCAL;
        }
        return;
    }
    leave_after_reads(p, obj, drop);
}

/// A transfer keeping what the caller holds (`velt_rt_xfer_keep_begin`) moves the reader's own
/// object whose count word is at `p`, which the reader may still reference: the caller's
/// reference becomes its only one and the bit goes, since the reader no longer counts it. In a
/// nested read storing into code outside every read ([`velt_rt_lk_leave`] keeps the bit until
/// the reads end), and for an object such a store left the reader's own ([`leaving`]), the
/// reader still counts its references, so the count stays.
///
/// # Safety
/// `p` is the count word of a counted object.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_lk_keep_made(p: *mut u64) {
    let depth = READER.with(|r| r.depth()) as u64;
    if (depth <= 1 || depth > LEAVE_TO.with(Cell::get)) && !leaving(p) {
        *p = 1;
    }
}

/// A value sema transferred out of a `read` callback (`velt_rt_xfer_share_begin`) holds the
/// reader's own object `obj`, whose count word is at `p`, which the reader still references: it
/// stays itself. Going to a frame still in read mode (an outer reader's variable) it stays that
/// reader's own, counted. Going to code outside every read, at any depth, it is the caller's
/// ([`STORED`]): a change through the readers' references stops the program, so nothing the
/// readers hold uncounted can be put into it after it left, and they still count their
/// references until this thread's outermost read ends, when it loses `LOCAL` and `STORED` and
/// the one more reference it holds meanwhile, which `drop` (its drop glue) releases: its count
/// is never 1 before then, so a transfer to another thread copies it. One a nested read's store
/// already left the readers' own ([`leaving`]) stays so.
///
/// # Safety
/// `p` is the count word of the counted object `obj`, and `drop` its drop glue.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_lk_share_made(p: *mut u64, obj: *mut u8, drop: DropGlue) {
    let depth = READER.with(|r| r.depth()) as u64;
    if depth > LEAVE_TO.with(Cell::get) || leaving(p) {
        return;
    }
    *p |= STORED;
    leave_after_reads(p, obj, drop);
}

/// Did a nested read's store leave the object whose count word is at `p` the reader's own
/// until the reads end ([`leave_after_reads`])? Moved on again before they end (into another of
/// the caller's containers, a callback's result), it stays so: its count keeps the reference
/// that store took, so it is never unique and a transfer to another thread copies it, and the
/// end of the reads settles it once. Only searched when objects wait (`FREES_WAIT`).
#[inline]
fn leaving(p: *mut u64) -> bool {
    READER.with(|r| r.read_depth.get() & FREES_WAIT != 0) && leaving_slow(p)
}

#[cold]
#[inline(never)]
fn leaving_slow(p: *mut u64) -> bool {
    LEAVING.with(|w| w.borrow().iter().any(|&(c, _, _)| c == p))
}

/// [`velt_rt_lk_leave`] in a nested read: the bit goes when the outermost read ends.
#[cold]
#[inline(never)]
unsafe fn leave_after_reads(p: *mut u64, obj: *mut u8, drop: DropGlue) {
    *p += 1;
    LEAVING.with(|w| w.borrow_mut().push((p, obj, drop)));
    READER.with(|r| r.read_depth.set(r.read_depth.get() | FREES_WAIT));
}

thread_local! {
    /// How many `read` callbacks out from the running one the objects [`velt_rt_lk_leave`]
    /// sees go (`velt_rt_lk_leave_to`).
    static LEAVE_TO: Cell<u64> = const { Cell::new(1) };
}

/// The transfers that follow, until the next call, move what a `read` callback made into a
/// frame `levels` `read` callbacks out from the running one (`u64::MAX`: code outside every
/// read): a container that frame holds, which a callback captured through `levels` nested
/// callbacks (velt_vir `leave_levels.rs`). Returns the number it replaces, for the caller to put
/// back.
#[no_mangle]
pub extern "C" fn velt_rt_lk_leave_to(levels: u64) -> u64 {
    LEAVE_TO.with(|l| l.replace(levels))
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        static DROPS: Cell<u32> = const { Cell::new(0) };
    }

    /// Counts the references the leaving objects' glue releases.
    unsafe extern "C" fn count_drop(_: *mut *mut u8) {
        DROPS.with(|d| d.set(d.get() + 1));
    }

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
        let obj = std::ptr::null_mut();
        unsafe { velt_rt_lk_leave(&mut mine, obj, count_drop) };
        assert_eq!(mine, 1 | LOCAL);
        // Stored two callbacks out, into a container of code outside every read: the reader's
        // own until the outermost read ends, with one more reference meanwhile.
        let was = velt_rt_lk_leave_to(2);
        unsafe { velt_rt_lk_leave(&mut mine, obj, count_drop) };
        assert_eq!(mine, 2 | LOCAL);
        assert_eq!(velt_rt_lk_leave_to(was), 2);
        // Moved on again at the outermost read (into another of the caller's containers): still
        // the reader's own, its count kept.
        READER.with(|r| r.pop());
        unsafe { velt_rt_lk_keep_made(&mut mine) };
        unsafe { velt_rt_lk_leave(&mut mine, obj, count_drop) };
        assert_eq!(mine, 2 | LOCAL);
        let mut other = 1 | LOCAL;
        unsafe { velt_rt_lk_leave(&mut other, obj, count_drop) };
        assert_eq!(other, 1);
        READER.with(|r| r.pop());
        // As `velt_rt_rwlock_read_unlock` ends the outermost read.
        READER.with(|r| r.read_depth.set(0));
        free_waiting();
        assert_eq!(mine, 2);
        assert_eq!(DROPS.with(Cell::get), 1);
        assert_eq!(in_read() as u8, 0);
        let mut store = 1u64;
        unsafe { velt_rt_lk_write(&mut store) };
    }

    #[test]
    fn a_shared_store_keeps_the_readers_counts_until_the_reads_end() {
        let obj = std::ptr::null_mut();
        let before = DROPS.with(Cell::get);
        READER.with(|r| r.push());
        // Stored outside every read while the reader holds it twice: the caller's, with one
        // more reference until the reads end, and the reader's counts kept.
        let mut made = 2 | LOCAL;
        unsafe { velt_rt_lk_share_made(&mut made, obj, count_drop) };
        assert_eq!(made, 3 | LOCAL | STORED);
        // Going to an outer reader's variable (a nested read whose result stays the reader's).
        READER.with(|r| r.push());
        let mut kept = 2 | LOCAL;
        unsafe { velt_rt_lk_share_made(&mut kept, obj, count_drop) };
        assert_eq!(kept, 2 | LOCAL);
        READER.with(|r| r.pop());
        // The reader drops its two references (read mode counts its own objects).
        made -= 2;
        READER.with(|r| r.pop());
        READER.with(|r| r.read_depth.set(0));
        free_waiting();
        assert_eq!(made, 1);
        assert_eq!(DROPS.with(Cell::get), before + 1);
    }
}
