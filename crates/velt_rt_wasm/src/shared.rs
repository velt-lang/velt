//! Primitives behind `shared<T>` (rt_abi_async.md §9): reference counts, atomic integers and
//! the `Mutex<T>` lock word. The WebAssembly runtime runs one thread, so these are plain memory
//! operations; a lock word is 1 while held, and locking a held lock is a deadlock (reported).

/// `shared.clone()`: increment the refcount at `p`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rc_inc(p: *mut u64) {
    *p = (*p)
        .checked_add(1)
        .unwrap_or_else(|| crate::panic::fatal("reference count overflow"));
}

/// Drop one reference: 1 if it was the last one (the caller drops and frees), else 0.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rc_dec(p: *mut u64) -> u8 {
    *p -= 1;
    u8::from(*p == 0)
}

/// `shared<int>.add(delta)`: returns the new value (wrapping).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_atomic_add_i64(p: *mut i64, delta: i64) -> i64 {
    *p = (*p).wrapping_add(delta);
    *p
}

/// `shared<int>.get()`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_atomic_load_i64(p: *mut i64) -> i64 {
    *p
}

/// `shared<int>.set(v)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_atomic_store_i64(p: *mut i64, v: i64) {
    *p = v;
}

/// Initialize the 8-byte lock word at `p` (unlocked).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_init(p: *mut u64) {
    *p = 0;
}

/// Lock. With one thread a held lock can never be released while we wait.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_lock(p: *mut u64) {
    if *p != 0 {
        crate::panic::fatal("deadlock: Mutex locked again while held");
    }
    *p = 1;
}

/// Unlock.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_mutex_unlock(p: *mut u64) {
    *p = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_atomics_and_locks() {
        let mut rc = 1u64;
        let mut cell = 5i64;
        let mut lock = 7u64;
        unsafe {
            velt_rt_rc_inc(&mut rc);
            assert_eq!(velt_rt_rc_dec(&mut rc), 0);
            assert_eq!(velt_rt_rc_dec(&mut rc), 1);
            assert_eq!(velt_rt_atomic_add_i64(&mut cell, 2), 7);
            velt_rt_atomic_store_i64(&mut cell, -1);
            assert_eq!(velt_rt_atomic_load_i64(&mut cell), -1);
            velt_rt_mutex_init(&mut lock);
            velt_rt_mutex_lock(&mut lock);
            assert_eq!(lock, 1);
            velt_rt_mutex_unlock(&mut lock);
            assert_eq!(lock, 0);
        }
    }
}
