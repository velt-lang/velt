//! The last translated positions, per thread (#377 phase 2b, docs/internals/design/strings.md
//! "Sequential indexing"): `for (i < s.length) s.charCodeAt(i)`, `s.slice(i, i + 1)` and
//! `indexOf(x, pos)` loops translate positions next to the previous one, so a translation near a
//! remembered one steps from it (a one-unit step decodes one character) instead of starting from
//! a breadcrumb up to 63 units back, or, in a static string, which has no breadcrumbs, from one
//! end.
//!
//! The entries live with the thread, not in the string: a field in the shared buffer would bounce
//! its cache line between threads reading one string. Only long non-ASCII strings (more than
//! `STRIDE` units: heap strings with breadcrumbs, and static ones) are remembered, keyed by the
//! address, `w1` and the form, so a heap and a static string never share an entry. An entry is
//! valid while the bytes it was taken from are alive and unchanged:
//! - heap: a buffer is immutable while shared, and an in-place append changes `w1` (the byte
//!   length grows), so a matching `w1` means unchanged bytes. Freeing or moving (growing) a
//!   buffer that has breadcrumbs bumps a global epoch first ([`buffer_gone`]), which invalidates
//!   every heap entry of every thread: a new string that later gets the same address and `w1` is
//!   not mistaken for the old one. A heap string is remembered only after its first translation
//!   built its breadcrumbs, and a slice (which has none of its own) marks its buffer as
//!   remembered (`heap::mark_remembered`) at its first translation, before it is remembered
//!   (a translation that finds no entry), so every remembered buffer
//!   bumps the epoch when it goes. A slice's `w0` and `w1` locate its bytes as a plain heap
//!   string's do: two live strings with the same ones have the same bytes. The epoch is
//!   bumped before the memory is released, and a string reaches another thread only through
//!   synchronization, so that thread sees the bump.
//! - static: a long non-ASCII static string points at a literal (or into one), whose bytes are
//!   never freed or changed: loaded code stays loaded, also across hot reloads. The one producer
//!   of static strings into freeable memory, the JSON reader's borrowed object keys
//!   (`velt_rt_json_reader_next_key`), copies a key that is long and non-ASCII, and a
//!   sub-range of a shorter key is shorter still. So static entries never expire, and heap frees
//!   leave them alone: a loop over a long literal, which has no breadcrumbs to fall back on,
//!   keeps its position while other strings come and go.
//!
//! The epoch bump costs one atomic increment per freed indexed string; nothing else is shared.

use std::cell::Cell;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use super::{BytePos, VeltStr};

/// The heap epoch before the program hands over its own, and in what never does (the runtime's
/// tests, a host).
static FALLBACK_EPOCH: AtomicU64 = AtomicU64::new(0);

/// The heap epoch's word, bumped whenever a heap buffer that may be remembered is freed or moved:
/// std's process static `STR_EPOCH` once `velt_main` handed it over ([`use_epoch`], #2779), else
/// [`FALLBACK_EPOCH`].
static HEAP_EPOCH: AtomicPtr<AtomicU64> =
    AtomicPtr::new(&FALLBACK_EPOCH as *const AtomicU64 as *mut AtomicU64);

/// The heap epoch's word. A relaxed load: the handover happens before the program starts a
/// thread, and starting one synchronizes.
#[inline]
fn heap_epoch() -> &'static AtomicU64 {
    // SAFETY: the pointer is the fallback or the program's static (`use_epoch`), each of which
    // lives as long as the process.
    unsafe { &*HEAP_EPOCH.load(Ordering::Relaxed) }
}

/// From now on the heap epoch is the word at `word` (std's `STR_EPOCH`), raised first above every
/// epoch the fallback gave out, so no entry remembered before is valid after. `velt_main` calls it
/// (through `velt_rt_statics`) first thing, before the program starts a thread: no other thread
/// bumps the fallback meanwhile.
///
/// # Safety
/// `word` is the address of a word that lives as long as the process.
pub(crate) unsafe fn use_epoch(word: *const AtomicU64) {
    let w = &*word;
    w.fetch_max(FALLBACK_EPOCH.load(Ordering::Acquire) + 1, Ordering::AcqRel);
    HEAP_EPOCH.store(word as *mut AtomicU64, Ordering::Release);
}

/// A remembered translation: unit `unit` of the string `key` is at `pos`.
#[derive(Clone, Copy)]
pub(super) struct Entry {
    key: [u64; 2],
    epoch: u64,
    /// The code unit.
    pub unit: usize,
    /// Its byte position.
    pub pos: BytePos,
}

const EMPTY: Entry = Entry {
    key: [0; 2],
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

/// The address and `w1`, with the static form (`w2 == 0`) in bit 63 of `w1`, which a unit count
/// (below 2^31) never sets.
#[inline]
fn key(s: &VeltStr) -> [u64; 2] {
    [s.w0, s.w1 | ((s.w2 == 0) as u64) << 63]
}

/// The epoch an entry of `s` must have: the heap epoch, or 0 for a static string (never expires).
#[inline]
fn epoch_of(s: &VeltStr) -> u64 {
    if s.w2 == 0 {
        0
    } else {
        heap_epoch().load(Ordering::Relaxed)
    }
}

/// The remembered translation of `s`, if any.
#[inline]
pub(super) fn find(s: &VeltStr) -> Option<Entry> {
    let key = key(s);
    // The epoch only for an entry of this string: most lookups miss on the key.
    let found = RECENT
        .try_with(|r| r.get().into_iter().find(|e| e.key == key))
        .ok()
        .flatten()?;
    (found.epoch == epoch_of(s)).then_some(found)
}

/// Remember that unit `unit` of `s` is at `pos`.
#[inline]
pub(super) fn remember(s: &VeltStr, unit: usize, pos: BytePos) {
    let key = key(s);
    let entry = Entry {
        key,
        epoch: epoch_of(s),
        unit,
        pos,
    };
    let _ = RECENT.try_with(|r| {
        let [first, second] = r.get();
        // Replace this string's entry, else the older one.
        let keep = if first.key == key { second } else { first };
        r.set([entry, keep]);
    });
}

/// A buffer with breadcrumbs is about to be freed or moved: forget every remembered position in
/// a heap string.
#[cold]
pub(super) fn buffer_gone() {
    heap_epoch().fetch_add(1, Ordering::Release);
}

/// The heap epoch (tests: freeing a remembered buffer bumps it).
#[cfg(test)]
pub(super) fn epoch() -> u64 {
    heap_epoch().load(Ordering::Relaxed)
}
