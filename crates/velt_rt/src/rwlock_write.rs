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
            0 => until = Some(revoke(p, w)),
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
/// it holds `lock`.
#[cold]
fn revoke(p: *mut u64, w: &Words) -> u64 {
    // No reader biases the lock again until the writer holds `lock` (`rebias` waits for
    // `until`); the writer stores the returned time then.
    let until = w.until.swap(u64::MAX, Ordering::Relaxed);
    w.state.store(REVOKING, Ordering::SeqCst);
    let t0 = now_ns();
    let mut spins = 0u32;
    while crate::hazard::named(p) {
        spins += 1;
        match spins < 64 {
            true => std::hint::spin_loop(),
            false => std::thread::yield_now(),
        }
    }
    // Writers holding `lock` may write from now on (Release: after the readers' reads). Not
    // when a reader biased the lock again meanwhile (another writer revoking with this one
    // wrote and stored `until`): this writer then finds `state` 0 holding `lock`, and revokes
    // once more.
    let _ = w
        .state
        .compare_exchange(REVOKING, MUTEX, Ordering::Release, Ordering::Relaxed);
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
    t1 + inhibit
}

/// Release the lock this thread took with `velt_rt_rwlock_write`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rwlock_write_unlock(p: *mut u64, reader: u64) {
    let _ = reader;
    crate::task::local::mutex_unlocked();
    (*(p as *const Words)).lock.unlock_exclusive();
}
