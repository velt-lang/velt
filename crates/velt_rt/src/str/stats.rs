//! `VELT_RC_STATS=1`: counts of string reference-count operations, printed to stderr at exit.
//!
//! Only a debug build of the runtime counts (release builds compile every hook to nothing). To
//! measure optimized code, build the program with `--release` and link the debug runtime
//! (`VELT_RT_LIB=<target>/debug/velt_rt.lib`). Report line:
//! `rc stats: retain=<n> release=<n> alloc=<n> free=<n>` — `retain` counts increments (a heap
//! string copied while its source stays alive), `release` decrements of a *shared* buffer (count
//! above 1; dropping the only reference is a plain `free`), `alloc`/`free` heap string buffers
//! (`alloc - free` = buffers still alive at exit). Refcount operations = `retain + release`.

#[cfg(debug_assertions)]
mod counters {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::sync::OnceLock;

    pub static RETAIN: AtomicU64 = AtomicU64::new(0);
    pub static RELEASE: AtomicU64 = AtomicU64::new(0);
    pub static ALLOC: AtomicU64 = AtomicU64::new(0);
    pub static FREE: AtomicU64 = AtomicU64::new(0);

    pub fn enabled() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("VELT_RC_STATS").is_some_and(|v| v == "1"))
    }

    pub fn bump(c: &AtomicU64) {
        if enabled() {
            c.fetch_add(1, Relaxed);
        }
    }
}

macro_rules! hook {
    ($name:ident, $counter:ident) => {
        #[inline(always)]
        pub(super) fn $name() {
            #[cfg(debug_assertions)]
            counters::bump(&counters::$counter);
        }
    };
}

hook!(retain, RETAIN);
hook!(release, RELEASE);
hook!(alloc, ALLOC);
hook!(free, FREE);

/// Print the counters (debug runtime with `VELT_RC_STATS=1` only). Called at process exit.
pub fn report() {
    #[cfg(debug_assertions)]
    if counters::enabled() {
        use counters::*;
        use std::sync::atomic::Ordering::Relaxed;
        eprintln!(
            "rc stats: retain={} release={} alloc={} free={}",
            RETAIN.load(Relaxed),
            RELEASE.load(Relaxed),
            ALLOC.load(Relaxed),
            FREE.load(Relaxed)
        );
    }
}
