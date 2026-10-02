//! A measure of the work the current thread has done, for tests that compare how the cost of
//! something grows with its input (`assert_linear`). Wall-clock time is the wrong measure there:
//! on a loaded machine a thread waits for a core for an unpredictable share of any interval, so
//! two runs of the same work can differ tenfold. A thread's own CPU clock only advances while it
//! runs.
//!
//! The unit differs by platform (nanoseconds of thread CPU time on Linux and macOS, CPU cycles on
//! Windows, whose thread times tick only every 15.6 ms), so only ratios of two readings mean
//! anything. Elsewhere it falls back to wall-clock nanoseconds.

/// The current thread's work counter; see the module documentation for its unit.
pub fn now() -> u64 {
    imp::now()
}

/// Runs `f` and returns how much the current thread's work counter advanced.
pub fn measure(f: impl FnOnce()) -> u64 {
    let start = now();
    f();
    now() - start
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_pointer_width = "64"
))]
mod imp {
    /// `struct timespec` on 64-bit Linux and macOS.
    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }

    extern "C" {
        fn clock_gettime(clock: i32, tp: *mut Timespec) -> i32;
    }

    #[cfg(target_os = "linux")]
    const CLOCK_THREAD_CPUTIME_ID: i32 = 3;
    #[cfg(target_os = "macos")]
    const CLOCK_THREAD_CPUTIME_ID: i32 = 16;

    pub fn now() -> u64 {
        let mut t = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `t` is a valid, writable `timespec` and the clock id is the platform's.
        let rc = unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut t) };
        assert_eq!(rc, 0, "clock_gettime(CLOCK_THREAD_CPUTIME_ID) failed");
        t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
    }
}

#[cfg(windows)]
mod imp {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThread() -> isize;
        fn QueryThreadCycleTime(thread: isize, cycles: *mut u64) -> i32;
    }

    pub fn now() -> u64 {
        let mut cycles = 0u64;
        // SAFETY: the pseudo handle of the current thread is always valid; `cycles` is writable.
        let ok = unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) };
        assert_ne!(ok, 0, "QueryThreadCycleTime failed");
        cycles
    }
}

#[cfg(not(any(
    windows,
    all(
        any(target_os = "linux", target_os = "macos"),
        target_pointer_width = "64"
    )
)))]
mod imp {
    use std::sync::OnceLock;
    use std::time::Instant;

    pub fn now() -> u64 {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_nanos() as u64
    }
}
