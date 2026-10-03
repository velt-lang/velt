//! Reference-counted string buffers, allocated with align 8. A heap `VeltStr` points at the
//! bytes; the count lives in the 8 bytes before them, so retaining never needs to know more.
//!
//! - ASCII strings: `[count: AtomicU64][cap bytes]`.
//! - Non-ASCII strings: `[crumbs: AtomicPtr<u8>][lone: u64][count: AtomicU64][cap bytes]`.
//!   `crumbs` will point at the breadcrumb table (#377 phase 2; always null for now) and `lone`
//!   counts the lone surrogates. Which layout a buffer has follows from the string value
//!   (`units != bytes`), so release, grow and free take it as `header`.
//!
//! Counting is atomic because any string may be shared with another thread. The common case
//! costs no atomic read-modify-write: dropping a buffer whose count is 1 (the only reference —
//! nobody else can be adding one concurrently) frees it after a plain load.

use std::alloc::{self, Layout};
use std::sync::atomic::{fence, AtomicPtr, AtomicU64, Ordering};

use super::stats;

/// Bytes before the text of an ASCII buffer (the count).
const COUNT: usize = 8;
/// Bytes before the text of a non-ASCII buffer (crumbs, lone, count).
const HEADER: usize = 24;

/// Largest capacity: string lengths are 32-bit (`w1` packs units and bytes), and the layout
/// (size rounded up to 8) must stay within `isize::MAX`.
pub(super) const MAX_CAP: usize = {
    let fits = isize::MAX as usize - 2 * HEADER;
    if fits < u32::MAX as usize {
        fits
    } else {
        u32::MAX as usize
    }
};

#[inline]
fn prefix(header: bool) -> usize {
    if header {
        HEADER
    } else {
        COUNT
    }
}

/// The layout of a buffer of `cap` bytes. Checked by hand: `Layout::from_size_align` is an
/// out-of-line call on every allocation and free, measurable on string-heavy loops. The one
/// place that enforces the length limit.
#[inline]
fn layout(cap: usize, header: bool) -> Layout {
    if cap > MAX_CAP {
        crate::panic::fatal("string too long");
    }
    // SAFETY: align 8 is a power of two and the prefix + cap rounded up to 8 fits in isize.
    unsafe { Layout::from_size_align_unchecked(prefix(header) + cap, 8) }
}

unsafe fn count<'a>(data: *mut u8) -> &'a AtomicU64 {
    &*(data.sub(COUNT) as *const AtomicU64)
}

/// The `lone` field of a non-ASCII buffer.
unsafe fn lone_field(data: *mut u8) -> *mut u64 {
    data.sub(16) as *mut u64
}

/// The `crumbs` field of a non-ASCII buffer.
#[cfg(debug_assertions)]
unsafe fn crumbs<'a>(data: *mut u8) -> &'a AtomicPtr<u8> {
    &*(data.sub(HEADER) as *const AtomicPtr<u8>)
}

/// A new buffer of `cap` bytes (count 1, and for a `header` buffer no crumbs and no lone
/// surrogates); returns the address of its first byte.
pub(super) fn alloc(cap: usize, header: bool) -> *mut u8 {
    let l = layout(cap, header);
    // SAFETY: the layout has a non-zero size (the prefix).
    let base = unsafe { alloc::alloc(l) };
    if base.is_null() {
        alloc::handle_alloc_error(l);
    }
    stats::alloc();
    // SAFETY: fresh allocation, 8-aligned, large enough for the prefix.
    unsafe {
        let data = base.add(prefix(header));
        if header {
            (base as *mut AtomicPtr<u8>).write(AtomicPtr::new(std::ptr::null_mut()));
            lone_field(data).write(0);
        }
        (data.sub(COUNT) as *mut AtomicU64).write(AtomicU64::new(1));
        data
    }
}

/// Resize the unique buffer at `data` from `cap` to `new_cap` bytes; returns the new address.
///
/// # Safety
/// `data` must come from [`alloc`] with capacity `cap` and the same `header`, and have count 1.
pub(super) unsafe fn grow(data: *mut u8, cap: usize, new_cap: usize, header: bool) -> *mut u8 {
    let new = layout(new_cap, header);
    let p = prefix(header);
    let base = alloc::realloc(data.sub(p), layout(cap, header), new.size());
    if base.is_null() {
        alloc::handle_alloc_error(new);
    }
    base.add(p)
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

/// The lone surrogates of the non-ASCII buffer at `data`.
///
/// # Safety
/// `data` must be a live buffer from [`alloc`] with a header.
pub(super) unsafe fn lone(data: *mut u8) -> usize {
    lone_field(data).read() as usize
}

/// Set the lone-surrogate count of a non-ASCII buffer the caller holds the only reference to.
///
/// # Safety
/// `data` must be a live buffer from [`alloc`] with a header and count 1.
pub(super) unsafe fn set_lone(data: *mut u8, n: usize) {
    lone_field(data).write(n as u64);
}

/// Drop a reference; frees the buffer (of `cap` bytes) with the last one.
///
/// # Safety
/// `data` must be a live buffer from [`alloc`] with capacity `cap` and the same `header`; the
/// caller's reference is gone afterwards.
pub(super) unsafe fn release(data: *mut u8, cap: usize, header: bool) {
    let c = count(data);
    if c.load(Ordering::Acquire) != 1 {
        stats::release();
        if c.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        fence(Ordering::Acquire);
    }
    stats::free();
    #[cfg(debug_assertions)]
    assert!(
        !header || crumbs(data).load(Ordering::Relaxed).is_null(),
        "ICE: string breadcrumbs are not built before #377 phase 2"
    );
    alloc::dealloc(data.sub(prefix(header)), layout(cap, header));
}
