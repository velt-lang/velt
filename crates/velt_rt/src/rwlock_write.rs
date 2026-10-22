//! `RwLock.write` (rt_abi_async.md §9.2): a writer revokes the readers' bias, without holding
//! the lock, before it writes (shared.rs has the words and the readers).

use std::sync::atomic::Ordering;

use lock_api::RawRwLock as _;

use crate::shared::{
    lock_slow, mine, now_ns, this_reader, Take, Words, MAX_INHIBIT, MUTEX, REVOKING,
};

/// `rw.write(f)`: take the lock alone. Inside a `read` callback it stops the program, before
/// taking anything: of the same lock it would wait for itself, and of another the callback
/// could not tell the reader's uncounted objects from what it changes (read_mode.rs).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_write(p: *mut u64, reader: u64) -> u64 {
    // This thread's state as for `velt_rt_rwlock_read`, returned for the unlock.
    let r = match reader {
        0 => this_reader(),
        r => {
            mine(r);
            r as *const crate::hazard::Reader
        }
    };
    if (*r).read_depth.get() != 0 {
        write_in_read(p, &*r);
    }
    let w = &*(p as *const Words);
    // Fast: `lock` is free, and the bias is revoked once this holds it (no reader biases it
    // again then). Acquire: the revoker's `MUTEX` follows the slots it saw cleared.
    if !w.lock.try_lock_exclusive() {
        write_slow(p, w);
    } else if w.state.load(Ordering::Acquire) & MUTEX == 0 {
        // Biased, or another writer is waiting for its readers: let `lock` go.
        w.lock.unlock_exclusive();
        write_slow(p, w);
    }
    crate::task::local::mutex_locked();
    r as u64
}

/// `velt_rt_rwlock_write` past its fast path, not holding `lock`. The bias is revoked before
/// `lock` is taken, and never while it is held: a writer holding it while it waits for the
/// readers' slots would wait for a reader whose nested read of another lock waits for that
/// lock's writer, which may be waiting for this writer's readers in turn. A writer finding
/// another revoking waits for it the same way. `until` stays `u64::MAX` (no reader biases the
/// lock again) until `lock` is held.
#[cold]
#[inline(never)]
fn write_slow(p: *mut u64, w: &Words) {
    let mut until = None;
    loop {
        match w.state.load(Ordering::Acquire) {
            0 => match revoke(p, w) {
                Some(u) => until = Some(u),
                // Another writer started revoking first: wait for it.
                None => continue,
            },
            REVOKING => {
                let mut spins = 0u32;
                while w.state.load(Ordering::Acquire) == REVOKING {
                    spins += 1;
                    match spins < 64 {
                        true => std::hint::spin_loop(),
                        false => std::thread::yield_now(),
                    }
                }
                continue;
            }
            _ => {}
        }
        if !w.lock.try_lock_exclusive() {
            lock_slow(&w.lock, Take::Exclusive);
        }
        if w.state.load(Ordering::Acquire) & MUTEX != 0 {
            break;
        }
        // A reader biased the lock again before this took it, or another writer started
        // revoking: let `lock` go and look again (rare).
        // SAFETY: this thread took `lock` exclusively just above.
        unsafe { w.lock.unlock_exclusive() };
    }
    if let Some(u) = until {
        w.until.store(u, Ordering::Relaxed);
    }
}

#[cold]
fn write_in_read(p: *mut u64, r: &crate::hazard::Reader) -> ! {
    if r.reads(p) {
        crate::panic::fatal("deadlock: `write` on an `RwLock` inside its own `read` callback");
    }
    crate::read_mode::refuse_in_read("write");
    unreachable!("ICE: a write inside a read went on")
}

/// A writer about to take `lock` of a biased `RwLock`: readers take `lock` from now on; wait for
/// the ones in slots. Returns until when the bias stays revoked, which the writer stores once
/// it holds `lock`, or `None` when another writer started revoking first.
///
/// One writer at a time revokes, and each change of `state` has one owner: 0 to `REVOKING`
/// (the revoker's compare-exchange), `REVOKING` to `MUTEX` (that revoker alone), `MUTEX` to 0
/// (`rebias`, a compare-exchange by a reader holding `lock`). A revoker held up anywhere can
/// therefore never take a later revocation for its own (ABA): while it revokes, `state` stays
/// `REVOKING`, which no reader biases again.
#[cold]
fn revoke(p: *mut u64, w: &Words) -> Option<u64> {
    pause(Point::Revoking);
    // SeqCst: readers store their slot, then read `state`; this writer stores `state`, then
    // reads the slots.
    if w.state
        .compare_exchange(0, REVOKING, Ordering::SeqCst, Ordering::Relaxed)
        .is_err()
    {
        return None;
    }
    // No reader biases the lock again until the writer holds `lock` (`rebias` waits for
    // `until`, and for `MUTEX`); the writer stores the returned time then.
    let until = w.until.swap(u64::MAX, Ordering::Relaxed);
    let t0 = now_ns();
    let mut spins = 0u32;
    while crate::hazard::named(p) {
        spins += 1;
        match spins < 64 {
            true => std::hint::spin_loop(),
            false => std::thread::yield_now(),
        }
    }
    let t1 = now_ns();
    let biased_for = match until {
        u64::MAX => 0,
        u => t0.saturating_sub(u),
    };
    let last = w.inhibit.load(Ordering::Relaxed);
    let floor = 9 * (t1 - t0).max(1000);
    let inhibit = match biased_for < last {
        true => (last * 2).min(MAX_INHIBIT),
        false => last / 2,
    }
    .max(floor);
    w.inhibit.store(inhibit, Ordering::Relaxed);
    pause(Point::Scanned);
    // Writers holding `lock` may write from now on (Release: after the readers' reads).
    w.state.store(MUTEX, Ordering::Release);
    Some(t1 + inhibit)
}

/// Where a revocation can be held up in tests, to force the interleavings of several writers
/// and readers (`tests`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Point {
    /// About to start revoking.
    Revoking,
    /// The readers' slots are clear, before the state says the bias is revoked.
    Scanned,
}

#[cfg(test)]
thread_local! {
    /// This thread's test hook, called at each `Point`.
    pub(crate) static PAUSE: std::cell::RefCell<Option<Box<dyn Fn(Point)>>> =
        const { std::cell::RefCell::new(None) };
}

#[inline(always)]
fn pause(at: Point) {
    #[cfg(test)]
    PAUSE.with(|h| {
        if let Some(h) = &*h.borrow() {
            h(at)
        }
    });
    let _ = at;
}

/// Release the lock this thread took with `velt_rt_rwlock_write`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_write_unlock(p: *mut u64, reader: u64) {
    let _ = reader;
    crate::task::local::mutex_unlocked();
    (*(p as *const Words)).lock.unlock_exclusive();
}

#[cfg(test)]
#[path = "rwlock_write_tests.rs"]
mod tests;
