//! The process statics (#2779, rt_abi.md "Process statics"): words of the program's data that the
//! runtime reads and writes, each a static atomic of std (velt_common `std_hooks.rs`, a
//! `process_static` entry), and the program's function releasing what its static atomics hold.
//! `velt_main` hands the runtime a table of their addresses first thing (`velt_rt_statics`), so
//! the words are Velt's: the runtime only uses them, and needs no symbol of the program (the
//! shared runtime of debug builds is linked before the program exists). The same table holds
//! the transfer map's functions the runtime calls (std/lang/hooks/transfer.velt, #3584): the
//! state of a transfer under way is std's, per thread.

use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

/// What `velt_main` hands over, as the compiler lays it out (velt_vir `lower/static_atomic.rs`):
/// 8-byte slots on every target, an address in the first half of its slot on wasm32 (the second
/// half zero). The runtime reads only the slots `size` covers, so a slot added at the end is
/// additive: an older table has a smaller `size`.
#[repr(C)]
pub struct Statics {
    /// The table's size in bytes.
    pub size: u64,
    /// The address of the function releasing the references the program's static atomics hold,
    /// or 0.
    pub drop: u64,
    /// The address of std's `STR_EPOCH` (`str_epoch`): the epoch of the per-thread position cache
    /// (`str/recent.rs`), or 0.
    pub str_epoch: u64,
    /// The address of std's `xferKeeps() -> u8` (`xfer_keeps`), or 0.
    pub xfer_keeps: u64,
    /// The address of std's `xferMark() -> u64` (`xfer_mark`), or 0.
    pub xfer_mark: u64,
    /// The address of std's `xferForgetSince(mark: u64)` (`xfer_forget_since`), or 0.
    pub xfer_forget_since: u64,
}

// The compiler's layout, on every target.
const _: () = assert!(std::mem::offset_of!(Statics, drop) == 8);
const _: () = assert!(std::mem::offset_of!(Statics, str_epoch) == 16);
const _: () = assert!(std::mem::offset_of!(Statics, xfer_keeps) == 24);
const _: () = assert!(std::mem::offset_of!(Statics, xfer_mark) == 32);
const _: () = assert!(std::mem::offset_of!(Statics, xfer_forget_since) == 40);
const _: () = assert!(std::mem::size_of::<Statics>() == 48);

/// The fields of `t` its size covers: `(drop, str_epoch)`, each null when absent.
///
/// # Safety
/// `t` points at a table of at least `t.size` bytes laid out as [`Statics`].
pub(crate) unsafe fn fields(t: *const Statics) -> (*const (), *const AtomicU64) {
    let size = (*t).size;
    let slot = |end: u64, v: u64| if size >= end { v as usize } else { 0 };
    let drop = slot(16, (*t).drop) as *const ();
    let epoch = slot(24, (*t).str_epoch) as *const AtomicU64;
    (drop, epoch)
}

/// The transfer map's functions of `t` its size covers, each null when absent:
/// `(xfer_keeps, xfer_mark, xfer_forget_since)`.
///
/// # Safety
/// As [`fields`].
unsafe fn transfer_fields(t: *const Statics) -> [*mut (); 3] {
    let size = (*t).size;
    let slot = |end: u64, v: u64| if size >= end { v as usize } else { 0 } as *mut ();
    [
        slot(32, (*t).xfer_keeps),
        slot(40, (*t).xfer_mark),
        slot(48, (*t).xfer_forget_since),
    ]
}

/// std's `xferKeeps`, `xferMark` and `xferForgetSince` (null: the table had none, so no transfer
/// is ever under way: a program without std).
static XFER: [AtomicPtr<()>; 3] = [
    AtomicPtr::new(std::ptr::null_mut()),
    AtomicPtr::new(std::ptr::null_mut()),
    AtomicPtr::new(std::ptr::null_mut()),
];

/// Keeps the transfer map's functions of the table `velt_main` handed over (`velt_rt_statics`).
///
/// # Safety
/// As [`fields`], and each function address non-null in it is a function of the signature its
/// field says, living as long as the process.
pub(crate) unsafe fn adopt_transfer_map(t: *const Statics) {
    for (slot, f) in XFER.iter().zip(transfer_fields(t)) {
        if !f.is_null() {
            slot.store(f, Ordering::Release);
        }
    }
}

/// 1 inside a transfer that keeps what a `read` callback's caller holds, 2 inside one that keeps
/// what the reader made, else 0 (std's `xferKeeps`, read on this thread).
pub(crate) fn xfer_keeps() -> u8 {
    let f = XFER[0].load(Ordering::Acquire);
    if f.is_null() {
        return 0;
    }
    // SAFETY: stored by `adopt_transfer_map` from the table's `xferKeeps`.
    let f: extern "C" fn() -> u8 = unsafe { std::mem::transmute::<*mut (), _>(f) };
    f()
}

/// What is open on this thread now, for [`xfer_forget_since`] when a poll that catches panics
/// starts (std's `xferMark`).
// velt_rt_wasm compiles this file too, without a catching poll.
#[allow(dead_code)]
pub(crate) fn xfer_mark() -> u64 {
    let f = XFER[1].load(Ordering::Acquire);
    if f.is_null() {
        return 0;
    }
    // SAFETY: stored by `adopt_transfer_map` from the table's `xferMark`.
    let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute::<*mut (), _>(f) };
    f()
}

/// A panic was caught by the poll that took `mark` ([`xfer_mark`]): the transfers that poll
/// opened end, their copies forgotten and what they deferred released; those open when it
/// started go on (std's `xferForgetSince`).
#[allow(dead_code)]
pub(crate) fn xfer_forget_since(mark: u64) {
    let f = XFER[2].load(Ordering::Acquire);
    if f.is_null() {
        return;
    }
    // SAFETY: stored by `adopt_transfer_map` from the table's `xferForgetSince`.
    let f: extern "C" fn(u64) = unsafe { std::mem::transmute::<*mut (), _>(f) };
    f(mark)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table as the compiler emits it: words, the addresses in 8-byte slots.
    fn table(words: [u64; 3]) -> [u64; 3] {
        words
    }

    #[test]
    fn a_table_is_read_in_the_compiler_s_layout_as_far_as_its_size_says() {
        let word = AtomicU64::new(0);
        let at = &word as *const AtomicU64 as u64;
        let full = table([24, 0x1000, at]);
        let read = |t: &[u64; 3]| unsafe { fields(t.as_ptr() as *const Statics) };
        assert_eq!(
            read(&full),
            (0x1000 as *const (), &word as *const AtomicU64)
        );
        // An older table, without `str_epoch`.
        assert_eq!(
            read(&table([16, 0x1000, at])),
            (0x1000 as *const (), std::ptr::null())
        );
        assert_eq!(
            read(&table([8, 0x1000, at])),
            (std::ptr::null(), std::ptr::null())
        );
        // No drop function: its slot is 0.
        assert_eq!(
            read(&table([24, 0, at])),
            (std::ptr::null(), &word as *const AtomicU64)
        );
    }

    #[test]
    fn the_transfer_map_s_functions_are_read_as_far_as_the_size_says() {
        let read = |t: &[u64; 6]| unsafe { transfer_fields(t.as_ptr() as *const Statics) };
        let at = |w: u64| w as usize as *mut ();
        let full = [48, 0, 0, 0x10, 0x20, 0x30];
        assert_eq!(read(&full), [at(0x10), at(0x20), at(0x30)]);
        // A table from before #3584 (24 bytes), and one cut after `xfer_keeps`.
        assert_eq!(read(&[24, 0, 0, 0x10, 0x20, 0x30]), [at(0); 3]);
        assert_eq!(
            read(&[32, 0, 0, 0x10, 0x20, 0x30]),
            [at(0x10), at(0), at(0)]
        );
    }
}
