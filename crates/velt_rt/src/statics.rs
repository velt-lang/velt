//! The process statics (#2779, rt_abi.md "Process statics"): words of the program's data that the
//! runtime reads and writes, each a static atomic of std (velt_common `std_hooks.rs`, a
//! `process_static` entry), and the program's function releasing what its static atomics hold.
//! `velt_main` hands the runtime a table of their addresses first thing (`velt_rt_statics`), so
//! the words are Velt's: the runtime only uses them, and needs no symbol of the program (the
//! shared runtime of debug builds is linked before the program exists).

use std::sync::atomic::AtomicU64;

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
}

// The compiler's layout, on every target.
const _: () = assert!(std::mem::offset_of!(Statics, drop) == 8);
const _: () = assert!(std::mem::offset_of!(Statics, str_epoch) == 16);
const _: () = assert!(std::mem::size_of::<Statics>() == 24);

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
}
