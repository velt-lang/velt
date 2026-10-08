//! Reference-counted string buffers, allocated with align 8. A heap `VeltStr` points at the
//! bytes; the count lives in the 8 bytes before them, so retaining never needs to know more.
//!
//! - ASCII strings: `[count: AtomicU64][cap bytes]`.
//! - Non-ASCII strings: `[crumbs: AtomicPtr<u8>][lone: u64][count: AtomicU64][cap bytes]`.
//!   `crumbs` points at the breadcrumb table (`crumbs.rs`; null until a position in a long
//!   string is first translated, or [`REMEMBERED`]) and `lone` counts the lone surrogates.
//!   Which layout a buffer has follows from the string value (`units != bytes`, or a slice's
//!   own bit), so release, grow and free take it as `header`.
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

/// Largest capacity: below 2 GiB, so a length fits in the 31 bits that compiled code reads
/// (`w1` packs units and bytes in 32 bits each, and the length read sign-extends the low half),
/// and the layout (size rounded up to 8) stays within `isize::MAX`.
pub(super) const MAX_CAP: usize = {
    let fits = isize::MAX as usize - 2 * HEADER;
    if fits < i32::MAX as usize {
        fits
    } else {
        i32::MAX as usize
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

/// The `lone` field of a non-ASCII buffer. Atomic (relaxed: a plain load or store) because a
/// count left unknown is filled in when somebody first needs it, possibly on a shared buffer
/// (every thread computes the same number).
unsafe fn lone_field<'a>(data: *mut u8) -> &'a AtomicU64 {
    &*(data.sub(16) as *const AtomicU64)
}

/// The `crumbs` field of a non-ASCII buffer (`crumbs.rs` builds, publishes and frees the
/// table).
///
/// # Safety
/// `data` must be a live buffer from [`alloc`] with a header.
pub(super) unsafe fn crumbs<'a>(data: *mut u8) -> &'a AtomicPtr<u8> {
    &*(data.sub(HEADER) as *const AtomicPtr<u8>)
}

/// The `crumbs` field of a buffer that has no breadcrumb table but whose slices' positions a
/// thread remembers (`recent.rs`): like a table, it makes freeing the buffer bump the epoch.
pub(super) const REMEMBERED: *mut u8 = std::ptr::without_provenance_mut(1);

/// The breadcrumb table in a `crumbs` field value: null for none (or [`REMEMBERED`]).
#[inline]
pub(super) fn table_of(field: *mut u8) -> *mut u8 {
    if field == REMEMBERED {
        std::ptr::null_mut()
    } else {
        field
    }
}

/// Note that a thread remembers a position in a slice of the non-ASCII buffer at `data`, so
/// freeing it must bump the epoch (`recent.rs`): sets the `crumbs` field to [`REMEMBERED`]
/// unless it has a table.
///
/// # Safety
/// `data` must be a live buffer from [`alloc`] with a header that the caller holds a reference
/// to.
#[inline]
pub(super) unsafe fn mark_remembered(data: *mut u8) {
    let field = crumbs(data);
    if field.load(Ordering::Relaxed).is_null() {
        // Losing the race to a table is fine: a table bumps the epoch too.
        let _ = field.compare_exchange(
            std::ptr::null_mut(),
            REMEMBERED,
            Ordering::Release,
            Ordering::Relaxed,
        );
    }
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
            (data.sub(16) as *mut AtomicU64).write(AtomicU64::new(0));
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
    if header && !crumbs(data).load(Ordering::Relaxed).is_null() {
        // The buffer may move: positions remembered for it must not apply to a new string at
        // this address (`recent.rs`).
        super::recent::buffer_gone();
    }
    let new = layout(new_cap, header);
    let p = prefix(header);
    let base = alloc::realloc(data.sub(p), layout(cap, header), new.size());
    if base.is_null() {
        alloc::handle_alloc_error(new);
    }
    base.add(p)
}

/// Move the text of the unique ASCII buffer at `data` (capacity `cap`, `len` bytes used) into a
/// non-ASCII buffer of `new_cap` bytes: the allocation grows by the header (in place when the
/// allocator can) and the text moves up behind it. Returns the new address of the text.
///
/// # Safety
/// `data` must come from [`alloc`] without a header, with capacity `cap`, and have count 1;
/// `len <= cap` and `len <= new_cap`.
pub(super) unsafe fn add_header(data: *mut u8, cap: usize, len: usize, new_cap: usize) -> *mut u8 {
    let new = layout(new_cap, true);
    let base = alloc::realloc(data.sub(COUNT), layout(cap, false), new.size());
    if base.is_null() {
        alloc::handle_alloc_error(new);
    }
    let data = base.add(HEADER);
    std::ptr::copy(base.add(COUNT), data, len);
    (base as *mut AtomicPtr<u8>).write(AtomicPtr::new(std::ptr::null_mut()));
    (data.sub(16) as *mut AtomicU64).write(AtomicU64::new(0));
    (data.sub(COUNT) as *mut AtomicU64).write(AtomicU64::new(1));
    data
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
    lone_field(data).load(Ordering::Relaxed) as usize
}

/// Set the lone-surrogate count of a non-ASCII buffer the caller holds the only reference to.
///
/// # Safety
/// `data` must be a live buffer from [`alloc`] with a header and count 1.
pub(super) unsafe fn set_lone(data: *mut u8, n: usize) {
    lone_field(data).store(n as u64, Ordering::Relaxed);
}

/// Record the counted lone surrogates of a non-ASCII buffer whose count was unknown. The buffer
/// may be shared: every reader counts the same immutable text, so racing stores agree.
///
/// # Safety
/// `data` must be a live buffer from [`alloc`] with a header, and `n` its text's count.
pub(super) unsafe fn resolve_lone(data: *mut u8, n: usize) {
    lone_field(data).store(n as u64, Ordering::Relaxed);
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
    if header {
        return free_with_header(data, cap);
    }
    alloc::dealloc(data.sub(COUNT), layout(cap, false));
}

/// Free a non-ASCII buffer and its breadcrumb table: out of line, so dropping a string (inlined
/// into `velt_rt_str_drop` and generated code's drops) keeps a short register-light fast path.
///
/// # Safety
/// As for [`release`], for the last reference to a buffer with a header.
#[inline(never)]
unsafe fn free_with_header(data: *mut u8, cap: usize) {
    // The last reference: nobody reads the table any more.
    let table = crumbs(data).load(Ordering::Acquire);
    if !table.is_null() {
        // Positions remembered for this string must not apply to a new one at this address.
        super::recent::buffer_gone();
    }
    super::crumbs::free(table_of(table));
    alloc::dealloc(data.sub(HEADER), layout(cap, true));
}
