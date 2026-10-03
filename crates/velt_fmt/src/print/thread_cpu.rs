//! The calling thread's CPU time, for tests that bound the cost of a computation. Wall-clock time
//! is the wrong measure there: on a loaded machine a thread waits for a core for an unpredictable
//! share of any interval. A thread's CPU clock only advances while it runs, and other tests
//! running in parallel don't count.
//!
//! Linux and macOS (64-bit): `CLOCK_THREAD_CPUTIME_ID`. Windows: `GetThreadTimes` (kernel + user,
//! in steps of about 15.6 ms). Elsewhere it falls back to wall-clock time.

use std::time::Duration;

/// Runs `f` and returns the CPU time the calling thread spent meanwhile.
pub fn measure(f: impl FnOnce()) -> Duration {
    let start = imp::now();
    f();
    imp::now().saturating_sub(start)
}

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    target_pointer_width = "64"
))]
mod imp {
    use std::time::Duration;

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

    pub fn now() -> Duration {
        let mut t = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `t` is a valid, writable `timespec` and the clock id is the platform's.
        let rc = unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut t) };
        assert_eq!(rc, 0, "clock_gettime(CLOCK_THREAD_CPUTIME_ID) failed");
        Duration::new(t.tv_sec as u64, t.tv_nsec as u32)
    }
}

#[cfg(windows)]
mod imp {
    use std::time::Duration;

    /// `FILETIME`: 100 ns ticks.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThread() -> isize;
        fn GetThreadTimes(
            thread: isize,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }

    pub fn now() -> Duration {
        let [mut c, mut e, mut k, mut u] = [FileTime::default(); 4];
        // SAFETY: the pseudo handle of the current thread is always valid; outputs are writable.
        let ok = unsafe { GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u) };
        assert_ne!(ok, 0, "GetThreadTimes failed");
        let ticks = |t: FileTime| (t.high as u64) << 32 | t.low as u64;
        Duration::from_nanos((ticks(k) + ticks(u)) * 100)
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
    use std::time::{Duration, Instant};

    pub fn now() -> Duration {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed()
    }
}
