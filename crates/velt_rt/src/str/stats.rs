//! `VELT_RC_STATS=1`: counts of string reference-count operations, printed to stderr at exit.
//!
//! Only a debug build of the runtime counts (release builds compile every hook to nothing). To
//! measure optimized code, build the program with `--release` and link the debug runtime
//! (`VELT_RT_LIB=<target>/debug/velt_rt.lib`). Report line:
//! `rc stats: retain=<n> release=<n> alloc=<n> free=<n>` — `retain` counts increments (a heap
//! string copied while its source stays alive), `release` decrements of a *shared* buffer (count
//! above 1; dropping the only reference is a plain `free`), `alloc`/`free` heap string buffers
//! (`alloc - free` = buffers still alive at exit). Refcount operations = `retain + release`.
//! `blocks=<allocated>/<freed>` counts every `velt_rt_alloc` / `velt_rt_free` of a non-empty
//! block (objects, array buffers, counted boxes, closure environments), plus the array buffers
//! the runtime hands to compiled code (`VeltBytes`, `VeltArray`, `VeltStrArray` `from_vec`) or
//! takes back from it (`VeltBytes::take_vec`), since compiled code frees and allocates those as
//! its own blocks: equal numbers at exit mean compiled code freed everything it allocated (leak
//! checks, semantics stage 2). `channel leftovers=<n>`: values still queued in channels when
//! `main` returned normally, which the runtime dropped then (docs/std/channel.md); a leak check
//! passes with them, so the count keeps them visible.

#[cfg(debug_assertions)]
mod counters {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::sync::OnceLock;

    pub static RETAIN: AtomicU64 = AtomicU64::new(0);
    pub static RELEASE: AtomicU64 = AtomicU64::new(0);
    pub static ALLOC: AtomicU64 = AtomicU64::new(0);
    pub static FREE: AtomicU64 = AtomicU64::new(0);
    pub static BLOCK_ALLOC: AtomicU64 = AtomicU64::new(0);
    pub static BLOCK_FREE: AtomicU64 = AtomicU64::new(0);
    pub static CHANNEL_LEFTOVERS: AtomicU64 = AtomicU64::new(0);

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
        pub(crate) fn $name() {
            #[cfg(debug_assertions)]
            counters::bump(&counters::$counter);
        }
    };
}

hook!(retain, RETAIN);
hook!(release, RELEASE);
hook!(alloc, ALLOC);
hook!(free, FREE);
hook!(block_alloc, BLOCK_ALLOC);
hook!(block_free, BLOCK_FREE);

/// Record the number of values left in channels at exit, dropped by the runtime (reported as
/// `channel leftovers=<n>`).
// velt_rt_wasm compiles this file too and drops no leftovers.
#[allow(dead_code)]
pub(crate) fn channel_leftovers(n: u64) {
    #[cfg(debug_assertions)]
    counters::CHANNEL_LEFTOVERS.store(n, std::sync::atomic::Ordering::Relaxed);
    #[cfg(not(debug_assertions))]
    let _ = n;
}

/// Before [`report`] when `main` returned normally (debug runtime with `VELT_RC_STATS=1` only):
/// tasks still running then (a task whose handle was dropped, the loser of a race) free their
/// blocks when they finish, so wait while blocks are unfreed and tasks are alive, instead of
/// counting a straggler as a leak. Up to a minute: a hang guard, which a real leak waits out.
/// `tasks_alive`: whether any task is still alive.
pub fn settle(tasks_alive: impl Fn() -> bool) {
    #[cfg(debug_assertions)]
    if counters::enabled() {
        use counters::*;
        use std::sync::atomic::Ordering::SeqCst;
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(60);
        while BLOCK_ALLOC.load(SeqCst) != BLOCK_FREE.load(SeqCst)
            && tasks_alive()
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = tasks_alive;
}

/// Print the counters (debug runtime with `VELT_RC_STATS=1` only). Called at process exit.
pub fn report() {
    #[cfg(debug_assertions)]
    if counters::enabled() {
        use counters::*;
        use std::sync::atomic::Ordering::Relaxed;
        eprintln!(
            "rc stats: retain={} release={} alloc={} free={} blocks={}/{} channel leftovers={}",
            RETAIN.load(Relaxed),
            RELEASE.load(Relaxed),
            ALLOC.load(Relaxed),
            FREE.load(Relaxed),
            BLOCK_ALLOC.load(Relaxed),
            BLOCK_FREE.load(Relaxed),
            CHANNEL_LEFTOVERS.load(Relaxed)
        );
    }
}
