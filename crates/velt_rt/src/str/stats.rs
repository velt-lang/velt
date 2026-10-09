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
//! checks, semantics stage 2); the counts are frozen at a moment when they were equal, or else
//! once no task is left that could make them so, which [`settle`] waits for. `channel
//! leftovers=<n>`: values still queued in channels when `main` returned normally, which the
//! runtime dropped then (docs/std/channel.md); a leak check passes with them, so the count keeps
//! them visible.

#[cfg(debug_assertions)]
mod counters {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::sync::OnceLock;

    pub static RETAIN: AtomicU64 = AtomicU64::new(0);
    pub static RELEASE: AtomicU64 = AtomicU64::new(0);
    pub static ALLOC: AtomicU64 = AtomicU64::new(0);
    pub static FREE: AtomicU64 = AtomicU64::new(0);
    pub static BLOCKS: super::BlockCount = super::BlockCount::new();
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
/// A block was allocated (`velt_rt_alloc`, or an array buffer handed to compiled code).
#[inline(always)]
pub(crate) fn block_alloc() {
    #[cfg(debug_assertions)]
    if counters::enabled() {
        counters::BLOCKS.alloc();
    }
}

/// A block was freed (`velt_rt_free`, or an array buffer taken back from compiled code).
#[inline(always)]
pub(crate) fn block_free() {
    #[cfg(debug_assertions)]
    if counters::enabled() {
        counters::BLOCKS.free();
    }
}

/// The `blocks=<allocated>/<freed>` counts, which [`settle`] freezes at exit.
///
/// Tasks still running when `main` returns go on allocating and freeing while the count is
/// taken, so allocated and freed numbers read one after the other were no count of any one
/// moment: a straggler that allocated its result between the two reads (or between the check
/// that found them equal and the report) showed up as a leak now and then (#804). The counts
/// therefore change together under one lock, and once frozen they stay as they were: blocks a
/// straggler allocates after a moment when every block was freed are neither leaked by compiled
/// code nor part of the count (the process ends with that task).
#[cfg_attr(not(debug_assertions), allow(dead_code))]
pub(crate) struct BlockCount(std::sync::Mutex<Blocks>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(debug_assertions), allow(dead_code))]
pub(crate) struct Blocks {
    pub allocated: u64,
    pub freed: u64,
    frozen: bool,
}

#[cfg_attr(not(debug_assertions), allow(dead_code))]
impl BlockCount {
    pub(crate) const fn new() -> Self {
        Self(std::sync::Mutex::new(Blocks {
            allocated: 0,
            freed: 0,
            frozen: false,
        }))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Blocks> {
        // The counts are plain numbers: a panic while holding the lock leaves them consistent.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn alloc(&self) {
        let mut b = self.lock();
        if !b.frozen {
            b.allocated += 1;
        }
    }

    pub(crate) fn free(&self) {
        let mut b = self.lock();
        if !b.frozen {
            b.freed += 1;
        }
    }

    /// The counts now (frozen ones once [`Self::settle`] returned).
    pub(crate) fn get(&self) -> Blocks {
        *self.lock()
    }

    /// Wait until every block allocated so far is freed, or no task that could free one is
    /// alive (`tasks_alive`), or `deadline` passed; then freeze the counts. The equal counts
    /// are checked and frozen in one step, so no straggler's allocation slips in between.
    pub(crate) fn settle(&self, tasks_alive: impl Fn() -> bool, deadline: std::time::Instant) {
        loop {
            {
                let mut b = self.lock();
                if b.allocated == b.freed {
                    b.frozen = true;
                    return;
                }
            }
            // Unfreed blocks: only a task still alive can free them (checked without the lock,
            // which the tasks need to count).
            if !tasks_alive() || std::time::Instant::now() >= deadline {
                self.lock().frozen = true;
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

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
/// Then the block counts are frozen ([`BlockCount`]): what tasks still running allocate after
/// that is not counted. `tasks_alive`: whether any task is still alive.
pub fn settle(tasks_alive: impl Fn() -> bool) {
    #[cfg(debug_assertions)]
    if counters::enabled() {
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(60);
        counters::BLOCKS.settle(tasks_alive, deadline);
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
        let blocks = BLOCKS.get();
        eprintln!(
            "rc stats: retain={} release={} alloc={} free={} blocks={}/{} channel leftovers={}",
            RETAIN.load(Relaxed),
            RELEASE.load(Relaxed),
            ALLOC.load(Relaxed),
            FREE.load(Relaxed),
            blocks.allocated,
            blocks.freed,
            CHANNEL_LEFTOVERS.load(Relaxed)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::BlockCount;
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    fn counts(c: &BlockCount) -> (u64, u64) {
        let b = c.get();
        (b.allocated, b.freed)
    }

    /// #804: a task still running after `main` returned allocated its result between the moment
    /// the counts were seen equal and the report, which then showed a leak.
    #[test]
    fn a_straggler_allocating_after_the_counts_settled_is_not_a_leak() {
        let c = BlockCount::new();
        c.alloc();
        c.free();
        let far = Instant::now() + Duration::from_secs(60);
        c.settle(|| true, far);
        // The straggler finishes now: it allocates its result, and the process exits before
        // anything frees it.
        c.alloc();
        assert_eq!(counts(&c), (1, 1));
        c.free();
        assert_eq!(counts(&c), (1, 1));
    }

    #[test]
    fn settling_waits_for_a_task_that_frees_its_blocks() {
        let c = BlockCount::new();
        c.alloc();
        c.alloc();
        c.free();
        // The task frees its block on the third look and then ends.
        let looks = Cell::new(0);
        let alive = || {
            looks.set(looks.get() + 1);
            if looks.get() == 3 {
                c.free();
            }
            looks.get() < 3
        };
        c.settle(alive, Instant::now() + Duration::from_secs(60));
        assert_eq!(counts(&c), (2, 2));
        assert_eq!(looks.get(), 3);
    }

    #[test]
    fn a_leak_with_no_task_left_is_counted_and_frozen() {
        let c = BlockCount::new();
        c.alloc();
        c.settle(|| false, Instant::now() + Duration::from_secs(60));
        c.alloc();
        c.free();
        assert_eq!(counts(&c), (1, 0));
    }
}
