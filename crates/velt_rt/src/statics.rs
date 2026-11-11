//! The process statics (#2779, rt_abi.md "Process statics"): words of the program's data that the
//! runtime reads and writes, each a static atomic of std (velt_common `std_hooks.rs`, a
//! `process_static` entry), and the program's function releasing what its static atomics hold.
//! `velt_main` hands the runtime a table of their addresses first thing (`velt_rt_statics`), so
//! the words are Velt's: the runtime only uses them, and needs no symbol of the program (the
//! shared runtime of debug builds is linked before the program exists).

use std::sync::atomic::AtomicU64;

/// What `velt_main` hands over. The runtime reads only the fields `size` covers, so a field added
/// at the end is additive: an older table has a smaller `size`.
#[repr(C)]
pub struct Statics {
    /// The table's size in bytes.
    pub size: u64,
    /// The function releasing the references the program's static atomics hold, or null.
    pub drop: *const (),
    /// std's `STR_EPOCH` (`str_epoch`): the epoch of the per-thread position cache
    /// (`str/recent.rs`), or null.
    pub str_epoch: *const AtomicU64,
}

/// The fields of `t` its size covers: `(drop, str_epoch)`, each null when absent.
///
/// # Safety
/// `t` points at a table of at least `t.size` bytes laid out as [`Statics`].
pub(crate) unsafe fn fields(t: *const Statics) -> (*const (), *const AtomicU64) {
    let size = (*t).size as usize;
    let covers = |end: usize| size >= end;
    let drop = if covers(16) {
        (*t).drop
    } else {
        std::ptr::null()
    };
    let epoch = if covers(24) {
        (*t).str_epoch
    } else {
        std::ptr::null()
    };
    (drop, epoch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_is_read_only_as_far_as_its_size_says() {
        let word = AtomicU64::new(0);
        let drop = 0x1000 as *const ();
        let full = Statics {
            size: 24,
            drop,
            str_epoch: &word,
        };
        assert_eq!(unsafe { fields(&full) }, (drop, &word as *const AtomicU64));
        // An older table, without `str_epoch`.
        let short = Statics { size: 16, ..full };
        assert_eq!(unsafe { fields(&short) }, (drop, std::ptr::null()));
        let none = Statics { size: 8, ..full };
        assert_eq!(
            unsafe { fields(&none) },
            (std::ptr::null(), std::ptr::null())
        );
    }
}
