//! The side table is the process's: a weakly held object may be released on another thread than
//! the one that put it in a map (an async task resumes on any worker; a `shared` `Mutex` hands
//! its value to whichever task locks it), and threads use their own maps at the same time. An
//! object with `RC_WEAK` set and no record is an ICE. Value releases that release other weakly
//! held objects, or drop other maps, never re-enter the table's lock.

use super::*;

/// Runs `f` on a new thread; returns its result and the test objects it left alive there
/// (`live` counts per thread, so objects made on one thread and freed on the other show up as
/// +n on one and -n on the other).
fn on_thread<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> (R, i64) {
    std::thread::spawn(move || {
        let before = live();
        let r = f();
        (r, live() - before)
    })
    .join()
    .expect("the thread finished")
}

/// A raw pointer that may cross to another thread (the test hands the object over, as a task
/// moving between workers does).
#[derive(Clone, Copy)]
struct Moved(usize);

impl Moved {
    fn get(self) -> *mut u8 {
        self.0 as *mut u8
    }
}

#[test]
fn a_weakly_held_object_released_on_another_thread() {
    no_leak(|| {
        let before = live();
        let m = obj_map();
        let raw = new_obj(&[]);
        set(m, raw, new_obj(&[]));
        // SAFETY: `raw` is live.
        let r = unsafe { velt_rt_weakref_new(raw) };
        retain(raw);
        let k = Moved(raw as usize);
        // The decrement path, then the last reference (the free path), each on its own thread.
        let ((), freed) = on_thread(move || release(k.get()));
        assert_eq!(freed, 0);
        assert_eq!(count(raw), 1);
        assert_eq!(weakmap_len(m), 1, "the entry is kept while raw lives");
        let ((), freed) = on_thread(move || release(k.get()));
        assert_eq!(freed, -2, "raw and its value are freed on the other thread");
        assert_eq!(weakmap_len(m), 0);
        assert!(velt_rt_weakref_deref(r).is_null());
        // SAFETY: `r` is dropped once.
        unsafe { velt_rt_weakref_drop(r) };
        freed_elsewhere(2);
        finish(&[m]);
        assert_eq!(live(), before);
    });
}

#[test]
fn an_ephemeron_cycle_freed_from_another_thread() {
    // Built on one thread, released on another (the `raw -> proxy` cache across an `await`):
    // the trial on the other thread finds the cycle in the shared table.
    for raw_first in [true, false] {
        no_leak(|| {
            let before = live();
            let m = obj_map();
            let raw = new_obj(&[]);
            let p = proxy_of(m, raw, std::ptr::null_mut());
            let (first, second) = if raw_first { (raw, p) } else { (p, raw) };
            let (a, b) = (Moved(first as usize), Moved(second as usize));
            let ((), freed) = on_thread(move || {
                release(a.get());
                release(b.get());
            });
            assert_eq!(freed, -2);
            assert_eq!(weakmap_len(m), 0);
            freed_elsewhere(2);
            finish(&[m]);
            assert_eq!(live(), before);
        });
    }
}

#[test]
fn threads_use_their_own_maps_at_once() {
    no_leak(|| {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    let before = live();
                    for _ in 0..200 {
                        let m = obj_map();
                        let raws: Vec<_> = (0..8).map(|_| new_obj(&[])).collect();
                        let proxies: Vec<_> = raws
                            .iter()
                            .map(|&raw| proxy_of(m, raw, std::ptr::null_mut()))
                            .collect();
                        for (&raw, &p) in raws.iter().zip(&proxies) {
                            let again = proxy_of(m, raw, std::ptr::null_mut());
                            assert_eq!(again, p, "the cached proxy");
                            release(again);
                        }
                        assert_eq!(weakmap_len(m), 8);
                        raws.into_iter().chain(proxies).for_each(release);
                        assert_eq!(weakmap_len(m), 0);
                        // SAFETY: `m` is dropped once.
                        unsafe { velt_rt_weakmap_drop(m) };
                    }
                    live() - before
                })
            })
            .collect();
        for w in workers {
            assert_eq!(w.join().expect("the worker finished"), 0);
        }
        assert_eq!(tracked_objects(), 0);
    });
}

#[test]
fn a_released_value_that_releases_other_weak_keys() {
    // m1: a -> v1, v1 holds b; m2: b -> v2, v2 holds c; m3: c -> v3. The last release of `a`
    // releases v1 (outside the lock), which releases b's last reference, and so on.
    no_leak(|| {
        let (m1, m2, m3) = (obj_map(), obj_map(), obj_map());
        let (a, b, c) = (new_obj(&[]), new_obj(&[]), new_obj(&[]));
        set(m3, c, new_obj(&[]));
        set(m2, b, new_obj(&[c]));
        set(m1, a, new_obj(&[b]));
        release(a);
        for m in [m1, m2, m3] {
            assert_eq!(weakmap_len(m), 0);
        }
        finish(&[m1, m2, m3]);
    });
}

/// Value glue of a map whose values are other maps' handles (a value that owns a map).
unsafe extern "C" fn retain_nothing(_: u64) {}

unsafe extern "C" fn drop_map_value(v: u64) {
    velt_rt_weakmap_drop(v as MapId);
}

#[test]
fn a_released_value_that_drops_another_map() {
    // outer: a -> (inner map: b -> proxy of b). Freeing a drops the inner map from its value's
    // release, which releases the proxy and frees the `b -> proxy` cycle.
    no_leak(|| {
        let outer = velt_rt_weakmap_new(
            Some(TraceFn(trace_obj)),
            Some(retain_nothing),
            Some(drop_map_value),
            None,
        );
        let inner = obj_map();
        let a = new_obj(&[]);
        let b = new_obj(&[]);
        let p = proxy_of(inner, b, std::ptr::null_mut());
        release(p);
        // SAFETY: `outer` is live; it takes over `inner`.
        unsafe { velt_rt_weakmap_set(outer, a, u64::from(inner)) };
        release(b);
        assert_eq!(
            weakmap_len(inner),
            0,
            "the cycle is freed with b's last outside release"
        );
        let b = new_obj(&[]);
        let p = proxy_of(inner, b, std::ptr::null_mut());
        release(p);
        release(a);
        // `inner` is gone: the proxy and b's entry with it, and b is unmarked.
        assert!(!is_marked(b));
        release(b);
        finish(&[outer]);
    });
}

#[test]
fn an_object_marked_without_a_record_is_an_ice() {
    no_leak(|| {
        let obj = new_obj(&[]);
        // SAFETY: `obj` is live; the flag is set without a record, as corruption would.
        unsafe { *rc_word(obj) |= RC_WEAK };
        for n in [1, 2] {
            if n == 2 {
                retain(obj);
            }
            // SAFETY: the release panics before it touches the object.
            let err = std::panic::catch_unwind(|| unsafe { weak_release(obj) })
                .expect_err("a release without a record must be reported");
            let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
            assert!(msg.starts_with("ICE: weakly held object"), "{msg}");
            assert_eq!(count(obj), n, "the count is untouched");
        }
        // SAFETY: as above; the object is then released as an ordinary one.
        unsafe { *rc_word(obj) &= !RC_WEAK };
        release(obj);
        release(obj);
        assert_eq!(tracked_objects(), 0);
    });
}
