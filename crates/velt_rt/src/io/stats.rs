//! `VELT_STDOUT_STATS=1` (debug runtime only): how many times stdout was written to the OS,
//! printed to stderr at exit as `stdout stats: writes=<n>`.
//!
//! A count, unlike a duration, does not depend on the machine: a test can check that piped output
//! goes out in large blocks (far fewer writes than lines) instead of timing it, and a write per
//! line shows up as such on every platform, however cheap the platform makes small writes.

#[cfg(debug_assertions)]
mod counter {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::sync::OnceLock;

    pub static WRITES: AtomicU64 = AtomicU64::new(0);

    pub fn enabled() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("VELT_STDOUT_STATS").is_some_and(|v| v == "1"))
    }

    pub fn bump() {
        if enabled() {
            WRITES.fetch_add(1, Relaxed);
        }
    }
}

/// Count one write of stdout bytes to the OS.
#[inline(always)]
pub(super) fn write() {
    #[cfg(debug_assertions)]
    counter::bump();
}

/// Print the count (debug runtime with `VELT_STDOUT_STATS=1` only). Called at process exit,
/// after the last flush.
pub fn report() {
    #[cfg(debug_assertions)]
    if counter::enabled() {
        eprintln!(
            "stdout stats: writes={}",
            counter::WRITES.load(std::sync::atomic::Ordering::Relaxed)
        );
    }
}
