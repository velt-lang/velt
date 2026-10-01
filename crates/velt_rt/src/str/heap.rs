//! Reference-counted string buffers: `[count: AtomicU64][cap bytes]`, allocated with align 8.
//! A heap `VeltStr` points at the bytes; the count lives in the 8 bytes before them.
//!
//! Counting is atomic because any string may be shared with another thread. The common case
//! costs no atomic read-modify-write: dropping a buffer whose count is 1 (the only reference —
//! nobody else can be adding one concurrently) frees it after a plain load.

use std::alloc::{self, Layout};
use std::sync::atomic::{fence, AtomicU64, Ordering};

use super::stats;

const HEADER: usize = 8;

/// Largest capacity whose layout is valid (size rounded up to 8 stays within `isize::MAX`).
const MAX_CAP: usize = isize::MAX as usize - 2 * HEADER;

/// The layout of a buffer of `cap` bytes. Checked by hand: `Layout::from_size_align` is an
/// out-of-line call on every allocation and free, measurable on string-heavy loops.
#[inline]
fn layout(cap: usize) -> Layout {
    if cap > MAX_CAP {
        crate::panic::fatal("string too large");
    }
    // SAFETY: align 8 is a power of two and HEADER + cap rounded up to 8 fits in isize.
    unsafe { Layout::from_size_align_unchecked(HEADER + cap, 8) }
}

unsafe fn count<'a>(data: *mut u8) -> &'a AtomicU64 {
    &*(data.sub(HEADER) as *const AtomicU64)
}

/// A new buffer of `cap` bytes (count 1); returns the address of its first byte.
pub(super) fn alloc(cap: usize) -> *mut u8 {
    let l = layout(cap);
    // SAFETY: the layout has a non-zero size (header).
    let base = unsafe { alloc::alloc(l) };
    if base.is_null() {
        alloc::handle_alloc_error(l);
    }
    stats::alloc();
    // SAFETY: fresh allocation, 8-aligned, large enough for the header.
    unsafe {
        (base as *mut AtomicU64).write(AtomicU64::new(1));
        base.add(HEADER)
    }
}

/// Resize the unique buffer at `data` from `cap` to `new_cap` bytes; returns the new address.
///
/// # Safety
/// `data` must come from [`alloc`] with capacity `cap` and have count 1.
pub(super) unsafe fn grow(data: *mut u8, cap: usize, new_cap: usize) -> *mut u8 {
    let base = alloc::realloc(data.sub(HEADER), layout(cap), HEADER + new_cap);
    if base.is_null() {
        alloc::handle_alloc_error(layout(new_cap));
    }
    base.add(HEADER)
}

/// Is `data`'s buffer referenced only by the caller?
///
/// # Safety
/// `data` must be a live buffer from [`alloc`].
pub(super) unsafe fn is_unique(data: *mut u8) -> bool {
    count(data).load(Ordering::Acquire) == 1
}

/// Add a reference.
///
/// # Safety
/// `data` must be a live buffer the caller holds a reference to.
pub(super) unsafe fn retain(data: *mut u8) {
    stats::retain();
    count(data).fetch_add(1, Ordering::Relaxed);
}

/// Drop a reference; frees the buffer (of `cap` bytes) with the last one.
///
/// # Safety
/// `data` must be a live buffer from [`alloc`] with capacity `cap`; the caller's reference is
/// gone afterwards.
pub(super) unsafe fn release(data: *mut u8, cap: usize) {
    let c = count(data);
    if c.load(Ordering::Acquire) != 1 {
        stats::release();
        if c.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        fence(Ordering::Acquire);
    }
    stats::free();
    alloc::dealloc(data.sub(HEADER), layout(cap));
}
