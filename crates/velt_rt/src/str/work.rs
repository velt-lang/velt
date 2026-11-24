//! Code units scanned to translate positions (`crumbs.rs`, `wtf8::count_units`), counted per
//! thread in unit tests only: cost tests check that a loop over a long string stays linear by
//! counting this work, never by timing it. Other builds compile the hook to nothing.

#[cfg(test)]
thread_local! {
    static SCANNED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `n` more code units (or bytes, for a count) were scanned.
#[inline(always)]
pub(crate) fn scanned(_n: usize) {
    #[cfg(test)]
    SCANNED.with(|c| c.set(c.get() + _n));
}

/// The units this thread scanned so far.
#[cfg(test)]
pub(crate) fn total() -> usize {
    SCANNED.with(|c| c.get())
}

/// The least of a few runs of `run`, which returns the work it did. The runs see a heap epoch of
/// this thread's own (`recent::with_private_epoch`): other tests freeing indexed strings at the
/// same time bump the shared one, which expires remembered positions and adds work (#3617, under
/// parallel `cargo test` every run could see such bumps).
#[cfg(test)]
pub(crate) fn least_work(mut run: impl FnMut() -> usize) -> usize {
    super::recent::with_private_epoch(|| (0..5).map(|_| run()).min().unwrap_or(0))
}
