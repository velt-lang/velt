//! Memory: `velt_rt_alloc` / `velt_rt_realloc` / `velt_rt_free` over mimalloc (also the Rust
//! global allocator), or over the Rust global allocator when built without the `mimalloc`
//! feature.
//!
//! Zero-sized requests never touch the allocator: `alloc(0, align)` returns the non-null dangling
//! pointer `align`, and `free(p, 0, _)` is a no-op.
//!
//! These calls sit on every `new C()` and every array growth, so with mimalloc they call its C
//! entry points directly (`raw`) instead of going through `GlobalAlloc`, which rebuilds a
//! `Layout` and always takes mimalloc's aligned path. Every mimalloc block is at least 8-aligned,
//! so an alignment of 8 or less (class instances, arrays of scalars and pointers) uses plain
//! `mi_malloc` / `mi_realloc`; larger alignments use the `_aligned` variants. Blocks from either
//! path, and from Rust code in the runtime, are all freed by `mi_free`, so memory moves freely
//! between Velt code and the runtime.
//!
//! The LLVM backend declares these functions with allocator attributes (`allockind`,
//! `allocsize`, `"alloc-family"="velt_rt_alloc"`, see `velt_codegen_llvm::runtime`): they must
//! behave like `malloc` / `realloc` / `free` and touch no program memory other than the blocks
//! they are given.

#[inline]
fn check_align(align: u64) -> usize {
    let align = if align == 0 { 1 } else { align };
    if !align.is_power_of_two() {
        crate::panic::fatal("invalid allocation layout");
    }
    align as usize
}

#[inline]
fn dangling(align: u64) -> *mut u8 {
    (if align == 0 { 1 } else { align }) as usize as *mut u8
}

/// Stops the program: the request cannot be satisfied. With `handle_alloc_error`'s message, but
/// as the runtime's other fatal errors do (exit status 101), the same on every target: a size
/// past a 32-bit target's address space (`block_size`) is reported the same way as one the
/// allocator refuses.
#[cold]
pub(crate) fn out_of_memory(size: u64, _align: usize) -> ! {
    crate::panic::fatal(&format!("memory allocation of {size} bytes failed"))
}

/// The largest block a target can allocate: Rust's `Layout` limit, `isize::MAX` bytes.
#[cfg_attr(target_pointer_width = "64", allow(dead_code))]
const MAX_BLOCK: u64 = isize::MAX as u64;

/// Whether a block of `size` bytes can exist on a target whose blocks hold at most `max` bytes.
#[cfg_attr(target_pointer_width = "64", allow(dead_code))]
#[inline(always)]
fn block_fits(size: u64, max: u64) -> bool {
    size <= max
}

/// `size` (generated code computes byte sizes in 64 bits) as this target's `usize`. On a 32-bit
/// target (wasm32) a size past [`MAX_BLOCK`] is out of memory: truncated, a 2^32 + 8 byte
/// request would get an 8-byte block. On a 64-bit target every size fits, and the allocator
/// refuses one it cannot serve, so there is no compare on the `new` path.
#[inline(always)]
pub(crate) fn block_size(size: u64, align: usize) -> usize {
    #[cfg(target_pointer_width = "32")]
    if !block_fits(size, MAX_BLOCK) {
        out_of_memory(size, align);
    }
    let _ = align;
    size as usize
}

/// The allocator entry points; each returns null when the request cannot be satisfied. Not in
/// the debug runtime linked into programs: its global allocator checks blocks (`debug_alloc`),
/// and blocks move between generated code and Rust code, so both must go through it.
#[cfg(all(feature = "mimalloc", any(not(debug_assertions), velt_rt_host)))]
mod raw {
    use libmimalloc_sys as mi;
    use std::ffi::c_void;

    /// Alignment of every `mi_malloc` block: blocks are whole words from a 16-aligned page start.
    const MIN_ALIGN: usize = 8;

    /// `align` is a power of two.
    pub(super) fn alloc(size: usize, align: usize) -> *mut u8 {
        // SAFETY: plain C allocation calls.
        unsafe {
            if align <= MIN_ALIGN {
                mi::mi_malloc(size) as *mut u8
            } else {
                mi::mi_malloc_aligned(size, align) as *mut u8
            }
        }
    }

    /// SAFETY: `p` is a live mimalloc block allocated with alignment `align`.
    pub(super) unsafe fn realloc(p: *mut u8, _old: usize, align: usize, new: usize) -> *mut u8 {
        if align <= MIN_ALIGN {
            mi::mi_realloc(p as *mut c_void, new) as *mut u8
        } else {
            mi::mi_realloc_aligned(p as *mut c_void, new, align) as *mut u8
        }
    }

    /// SAFETY: `p` is a live mimalloc block.
    pub(super) unsafe fn free(p: *mut u8, _size: usize, _align: usize) {
        mi::mi_free(p as *mut c_void);
    }
}

/// The allocator entry points over the Rust global allocator; each returns null on failure.
#[cfg(not(all(feature = "mimalloc", any(not(debug_assertions), velt_rt_host))))]
mod raw {
    use std::alloc::{self, Layout};

    fn layout(size: usize, align: usize) -> Layout {
        match Layout::from_size_align(size, align) {
            Ok(l) => l,
            Err(_) => crate::panic::fatal("invalid allocation layout"),
        }
    }

    /// `size` > 0 (callers handle zero-sized requests).
    pub(super) fn alloc(size: usize, align: usize) -> *mut u8 {
        // SAFETY: the layout is non-zero-sized.
        unsafe { alloc::alloc(layout(size, align)) }
    }

    /// SAFETY: `p` was allocated with `(old, align)` by this allocator; `new` > 0.
    pub(super) unsafe fn realloc(p: *mut u8, old: usize, align: usize, new: usize) -> *mut u8 {
        let _ = layout(new, align); // validate
        alloc::realloc(p, layout(old, align), new)
    }

    /// SAFETY: `p` was allocated with `(size, align)` by this allocator.
    pub(super) unsafe fn free(p: *mut u8, size: usize, align: usize) {
        alloc::dealloc(p, layout(size, align));
    }
}

#[no_mangle]
pub extern "C" fn velt_rt_alloc(size: u64, align: u64) -> *mut u8 {
    let a = check_align(align);
    if size == 0 {
        return dangling(align);
    }
    let p = raw::alloc(block_size(size, a), a);
    if p.is_null() {
        out_of_memory(size, a);
    }
    crate::str::stats::block_alloc();
    p
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_realloc(
    p: *mut u8,
    old_size: u64,
    align: u64,
    new_size: u64,
) -> *mut u8 {
    // Generated code reallocates only to grow an array: past the limit on an array's buffer
    // (rt_abi.md "Array length limit") it stops here, so growth stays one compare away from
    // the limit without a check in every inlined `push`. `velt_rt_alloc`, on every `new`, has
    // none.
    if new_size > crate::bytes::MAX_ARRAY_LEN {
        crate::bytes::array_too_long();
    }
    if p.is_null() || old_size == 0 {
        return velt_rt_alloc(new_size, align);
    }
    if new_size == 0 {
        velt_rt_free(p, old_size, align);
        return dangling(align);
    }
    let a = check_align(align);
    let q = raw::realloc(p, old_size as usize, a, block_size(new_size, a));
    if q.is_null() {
        out_of_memory(new_size, a);
    }
    q
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_free(p: *mut u8, size: u64, align: u64) {
    if p.is_null() || size == 0 {
        return;
    }
    crate::str::stats::block_free();
    raw::free(p, size as usize, check_align(align));
}

/// mimalloc's `mi_option_arena_eager_commit` (v3's `mimalloc.h`).
#[cfg(feature = "mimalloc")]
const ARENA_EAGER_COMMIT: std::ffi::c_int = 4;

#[cfg(feature = "mimalloc")]
extern "C" {
    /// In the mimalloc library `libmimalloc-sys` builds (bound only by its `extended` feature).
    fn mi_option_set_default(option: std::ffi::c_int, value: std::ffi::c_long);
}

/// Allocator settings, before the first allocation (`velt_rt_start`). Arenas commit memory as
/// it is used, not all at once: committed eagerly on Linux (mimalloc's default where memory is
/// overcommitted), an arena is marked for transparent huge pages, so each worker's first
/// allocations made 2 MiB resident (redis-clone on 10 workers: 44 MB for 100k keys, 22 MB
/// without; 11 MB for a server doing nothing, 4 MB without), at no throughput cost measured on
/// bench/web or redis-benchmark. `MIMALLOC_ARENA_EAGER_COMMIT` still overrides it.
#[cfg(feature = "mimalloc")]
pub(crate) fn configure() {
    // SAFETY: sets a default before mimalloc reads the option; no other effect.
    unsafe { mi_option_set_default(ARENA_EAGER_COMMIT, 0) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_realloc_free() {
        unsafe {
            let p = velt_rt_alloc(16, 8);
            assert!(!p.is_null() && (p as usize).is_multiple_of(8));
            for i in 0..16 {
                *p.add(i) = i as u8;
            }
            let q = velt_rt_realloc(p, 16, 8, 4096);
            assert!((q as usize).is_multiple_of(8));
            for i in 0..16 {
                assert_eq!(*q.add(i), i as u8);
            }
            let r = velt_rt_realloc(q, 4096, 8, 4);
            assert_eq!(*r.add(3), 3);
            velt_rt_free(r, 4, 8);

            let big = velt_rt_alloc(64, 64);
            assert!((big as usize).is_multiple_of(64));
            let big = velt_rt_realloc(big, 64, 64, 1000);
            assert!((big as usize).is_multiple_of(64));
            velt_rt_free(big, 1000, 64);
        }
    }

    #[test]
    fn small_blocks_keep_their_alignment() {
        unsafe {
            let blocks: Vec<(*mut u8, u64, u64)> = (1..=64u64)
                .flat_map(|size| [1u64, 2, 4, 8, 16].map(|align| (size, align)))
                .map(|(size, align)| (velt_rt_alloc(size, align), size, align))
                .collect();
            for &(p, size, align) in &blocks {
                assert!(
                    (p as usize).is_multiple_of(align as usize),
                    "{size}/{align}"
                );
            }
            for (p, size, align) in blocks {
                velt_rt_free(p, size, align);
            }
        }
    }

    #[test]
    fn runtime_and_velt_blocks_interoperate() {
        unsafe {
            // A block from Rust code in the runtime (a `Vec` handed to Velt) freed by Velt code.
            let mut v = std::mem::ManuallyDrop::new(vec![7u8; 100]);
            velt_rt_free(v.as_mut_ptr(), 100, 1);
            // A Velt block adopted by Rust code.
            let p = velt_rt_alloc(32, 8);
            drop(Vec::from_raw_parts(p, 0, 32));
        }
    }

    /// wasm32's blocks hold at most 2^31 - 1 bytes: a 64-bit size past that is refused, not
    /// truncated (`Array.from({ length: 2 ** 29 + 1 }, f)` asks for 2^32 + 8 bytes).
    #[test]
    fn sizes_past_a_32_bit_target_are_refused() {
        let max32 = i32::MAX as u64;
        assert!(block_fits(max32, max32));
        assert!(!block_fits(max32 + 1, max32));
        assert!(!block_fits((1 << 32) + 8, max32));
        assert!(!block_fits(crate::bytes::MAX_ARRAY_LEN, max32));
        assert!(block_fits(crate::bytes::MAX_ARRAY_LEN, MAX_BLOCK));
    }

    #[test]
    fn zero_sized() {
        unsafe {
            let z = velt_rt_alloc(0, 8);
            assert_eq!(z as usize, 8);
            velt_rt_free(z, 0, 8);
            let p = velt_rt_realloc(z, 0, 8, 32);
            assert!(!p.is_null());
            let d = velt_rt_realloc(p, 32, 8, 0);
            assert_eq!(d as usize, 8);
            velt_rt_free(std::ptr::null_mut(), 10, 1);
        }
    }
}
