//! The last translated positions, per thread (#377 phase 2b, docs/internals/design/strings.md
//! "Sequential indexing"): `for (i < s.length) s.charCodeAt(i)`, `s.slice(i, i + 1)` and
//! `indexOf(x, pos)` loops translate positions next to the previous one, so a translation near a
//! remembered one steps from it (a one-unit step decodes one character) instead of starting from
//! a breadcrumb up to 63 units back.
//!
//! The entries live with the thread, not in the string: a field in the shared buffer would bounce
//! its cache line between threads reading one string. Only long non-ASCII heap strings (the ones
//! with breadcrumbs) are remembered, by buffer address and `w1`; an entry is valid while the
//! buffer it was taken from is alive and unchanged:
//! - a heap buffer is immutable while shared, and an in-place append changes `w1` (the byte
//!   length grows), so a matching `w1` means unchanged bytes;
//! - freeing or moving (growing) a buffer that has breadcrumbs bumps a global epoch first
//!   ([`buffer_gone`]), which invalidates every entry of every thread: a new string that later
//!   gets the same address and `w1` is not mistaken for the old one. The epoch is bumped before
//!   the memory is released, and a string reaches another thread only through synchronization,
//!   so that thread sees the bump.
//!
//! The bump costs one atomic increment per freed indexed string; nothing else is shared.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

use super::BytePos;

/// Bumped whenever a buffer that may be remembered is freed or moved.
static EPOCH: AtomicU64 = AtomicU64::new(0);

/// A remembered translation: unit `unit` of the string `{ptr, w1}` is at `pos`.
#[derive(Clone, Copy)]
pub(super) struct Entry {
    ptr: usize,
    w1: u64,
    epoch: u64,
    /// The code unit.
    pub unit: usize,
    /// Its byte position.
    pub pos: BytePos,
}

const EMPTY: Entry = Entry {
    ptr: 0,
    w1: 0,
    epoch: u64::MAX,
    unit: 0,
    pos: BytePos {
        byte: 0,
        low_half: false,
    },
};

thread_local! {
    /// Two entries, so a loop over two strings at once (`a.charCodeAt(i) == b.charCodeAt(i)`)
    /// keeps both; the newer one first.
    static RECENT: Cell<[Entry; 2]> = const { Cell::new([EMPTY; 2]) };
}

/// The remembered translation of the string `{ptr, w1}`, if any.
#[inline]
pub(super) fn find(ptr: usize, w1: u64) -> Option<Entry> {
    let epoch = EPOCH.load(Ordering::Relaxed);
    RECENT
        .try_with(|r| {
            r.get()
                .into_iter()
                .find(|e| e.ptr == ptr && e.w1 == w1 && e.epoch == epoch)
        })
        .ok()
        .flatten()
}

/// Remember that unit `unit` of the string `{ptr, w1}` is at `pos`.
#[inline]
pub(super) fn remember(ptr: usize, w1: u64, unit: usize, pos: BytePos) {
    let entry = Entry {
        ptr,
        w1,
        epoch: EPOCH.load(Ordering::Relaxed),
        unit,
        pos,
    };
    let _ = RECENT.try_with(|r| {
        let [first, second] = r.get();
        // Replace this string's entry, else the older one.
        let keep = if first.ptr == ptr { second } else { first };
        r.set([entry, keep]);
    });
}

/// A buffer with breadcrumbs is about to be freed or moved: forget every remembered position.
#[cold]
pub(super) fn buffer_gone() {
    EPOCH.fetch_add(1, Ordering::Release);
}
