//! Read mode: what lets `RwLock.read` callbacks run on many threads at once without copying or
//! counting anything (docs/internals/contracts/rt_abi_async.md "Read mode").
//!
//! A thread is in read mode while it runs a `read` callback (`velt_rt_rwlock_read` to
//! `velt_rt_rwlock_read_unlock`, nested ones counted; a `with` or `write` inside one leaves it
//! for its duration). The *lock-set* types are the counted types a value inside an `RwLock` can
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

use crate::hazard::{EXCLUSIVE, READER};

/// Set in the count word of a lock-set object made in read mode.
pub const LOCAL: u64 = 1 << 62;

// This thread's state is in `hazard::READER` (`read_depth`: the read callbacks it is in,
// which `Reader::push` and `pop` count, and `EXCLUSIVE` while a `write` / `with` section inside
// one runs; `saved`: what those sections interrupted).

/// 1 when this thread is in read mode, else 0. A function asks once, when it first counts or
/// writes a lock-set object (a thread enters or leaves read mode only around a `read`
/// callback, which is a function of its own); with 0 it counts them inline.
#[no_mangle]
pub extern "C" fn velt_rt_lk_mode() -> u8 {
    u8::from(in_read())
}

/// Entering a `write` or `with` section: it may change what it locks, so it is not in read mode.
/// Up to 64 nested sections remember whether they interrupted one.
pub(crate) fn enter_exclusive() {
    READER.with(enter_exclusive_in)
}

/// [`enter_exclusive`] with this thread's state at hand.
#[inline]
pub(crate) fn enter_exclusive_in(r: &crate::hazard::Reader) {
    let d = r.read_depth.get();
    let was = d != 0;
    r.saved.set(r.saved.get() << 1 | u64::from(was));
    if was {
        r.read_depth.set(d | EXCLUSIVE);
    }
}

/// Leaving a `write` or `with` section.
pub(crate) fn leave_exclusive() {
    READER.with(leave_exclusive_in)
}

/// [`leave_exclusive`] with this thread's state at hand.
#[inline]
pub(crate) fn leave_exclusive_in(r: &crate::hazard::Reader) {
    let v = r.saved.get();
    r.saved.set(v >> 1);
    if v & 1 == 1 {
        r.read_depth.set(r.read_depth.get() & !EXCLUSIVE);
    }
}

/// In read mode, outside any `write` / `with` section of its own?
#[inline]
fn in_read() -> bool {
    let d = READER.with(|r| r.read_depth.get());
    d != 0 && d & EXCLUSIVE == 0
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_mode_follows_read_and_exclusive_sections() {
        assert_eq!(in_read() as u8, 0);
        READER.with(|r| r.push());
        assert_eq!(velt_rt_lk_mode(), 1);
        enter_exclusive();
        assert_eq!(velt_rt_lk_mode(), 0);
        leave_exclusive();
        assert_eq!(velt_rt_lk_mode(), 1);
        let mut mine = 1 | LOCAL;
        unsafe { velt_rt_lk_write(&mut mine) };
        READER.with(|r| r.pop());
        assert_eq!(in_read() as u8, 0);
        let mut store = 1u64;
        unsafe { velt_rt_lk_write(&mut store) };
    }
}
