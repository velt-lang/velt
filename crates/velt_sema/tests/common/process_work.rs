//! A measure of the work this process has done, for tests that compare how the cost of something
//! grows with its input (`scaling.rs`). Wall-clock time is the wrong measure there: on a loaded
//! machine a thread waits for a core for an unpredictable share of any interval, so two runs of
//! the same work can differ tenfold. The process's CPU clock only advances while one of its
//! threads runs. It is the process's, not the calling thread's, because `velt_sema::check` does
//! its work on a thread of its own; the caller must keep other threads of the test binary idle
//! meanwhile (`scaling.rs` serializes its tests).
//!
//! The unit differs by platform (nanoseconds of CPU time on Linux and macOS, CPU cycles on
//! Windows, whose process times tick only every 15.6 ms), so only ratios of readings mean
//! anything. Elsewhere it falls back to wall-clock nanoseconds.

/// The process's work counter; see the module documentation for its unit.
pub fn now() -> u64 {
    imp::now()
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
    const CLOCK_PROCESS_CPUTIME_ID: i32 = 2;
    #[cfg(target_os = "macos")]
    const CLOCK_PROCESS_CPUTIME_ID: i32 = 12;

    pub fn now() -> u64 {
        let mut t = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `t` is a valid, writable `timespec` and the clock id is the platform's.
        let rc = unsafe { clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &mut t) };
        assert_eq!(rc, 0, "clock_gettime(CLOCK_PROCESS_CPUTIME_ID) failed");
        t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
    }
}

#[cfg(windows)]
mod imp {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn QueryProcessCycleTime(process: isize, cycles: *mut u64) -> i32;
    }

    pub fn now() -> u64 {
        let mut cycles = 0u64;
        // SAFETY: the pseudo handle of the current process is always valid; `cycles` is writable.
        let ok = unsafe { QueryProcessCycleTime(GetCurrentProcess(), &mut cycles) };
        assert_ne!(ok, 0, "QueryProcessCycleTime failed");
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
