//! WeakMap, WeakSet and WeakRef operations, and entries vanishing with their keys.

use super::*;

#[test]
fn set_get_has_delete() {
    no_leak(|| {
        let m = obj_map();
        let k = new_obj(&[]);
        let v = new_obj(&[]);
        assert_eq!(get(m, k), None);
        set(m, k, retain(v));
        assert_eq!(get(m, k), Some(v));
        assert_eq!(velt_rt_weakmap_has(m, k), 1);
        assert_eq!(count(k), 1, "a weak key is not counted");
        assert_eq!(count(v), 2, "the map holds its value");
        assert!(is_marked(k) && !is_marked(v));
        // SAFETY: `m` is live.
        assert_eq!(unsafe { velt_rt_weakmap_delete(m, k) }, 1);
        assert_eq!(unsafe { velt_rt_weakmap_delete(m, k) }, 0);
        assert_eq!(velt_rt_weakmap_has(m, k), 0);
        assert_eq!(count(v), 1);
        assert!(!is_marked(k), "a key in no map loses its mark");
        release(k);
        release(v);
        finish(&[m]);
    });
}

#[test]
fn an_entry_vanishes_with_its_key() {
    no_leak(|| {
        let m = obj_map();
        let k = new_obj(&[]);
        let v = new_obj(&[]);
        set(m, k, v);
        let k2 = retain(k);
        release(k);
        assert_eq!(weakmap_len(m), 1, "still referenced");
        let before = live();
        release(k2);
        assert_eq!(weakmap_len(m), 0);
        assert_eq!(live(), before - 2, "the key and its value are freed");
        finish(&[m]);
    });
}

#[test]
fn set_replaces_and_releases_the_old_value() {
    no_leak(|| {
        let m = obj_map();
        let k = new_obj(&[]);
        let before = live();
        set(m, k, new_obj(&[]));
        set(m, k, new_obj(&[]));
        assert_eq!(live(), before + 1);
        assert_eq!(weakmap_len(m), 1);
        release(k);
        finish(&[m]);
    });
}

#[test]
fn dropping_the_map_releases_values_and_unmarks_keys() {
    no_leak(|| {
        let m = obj_map();
        let keys: Vec<_> = (0..10).map(|_| new_obj(&[])).collect();
        for &k in &keys {
            set(m, k, new_obj(&[]));
        }
        finish(&[m]);
        for k in keys {
            assert!(!is_marked(k));
            release(k);
        }
    });
}

#[test]
fn a_key_in_two_maps_leaves_both() {
    no_leak(|| {
        let (a, b) = (obj_map(), obj_map());
        let k = new_obj(&[]);
        set(a, k, new_obj(&[]));
        set(b, k, new_obj(&[]));
        release(k);
        assert_eq!(weakmap_len(a) + weakmap_len(b), 0);
        finish(&[a, b]);
    });
}

#[test]
fn plain_values_and_weak_sets() {
    no_leak(|| {
        let numbers = velt_rt_weakmap_new(Some(TraceFn(trace_obj)), None, None);
        let set_ = velt_rt_weakmap_new(Some(TraceFn(trace_obj)), None, None);
        let k = new_obj(&[]);
        // SAFETY: plain words need no ownership.
        unsafe {
            velt_rt_weakmap_set(numbers, k, 42);
            velt_rt_weakmap_set(set_, k, 0);
        }
        assert_eq!(get(numbers, k), Some(42 as *mut u8));
        assert_eq!(velt_rt_weakmap_has(set_, k), 1);
        release(k);
        assert_eq!(weakmap_len(numbers) + weakmap_len(set_), 0);
        finish(&[numbers, set_]);
    });
}

#[test]
fn weak_ref_deref_and_clearing() {
    no_leak(|| {
        let o = new_obj(&[]);
        // SAFETY: `o` is live; each ref is dropped once.
        let r = unsafe { velt_rt_weakref_new(o) };
        assert_eq!(count(o), 1, "a WeakRef does not count its target");
        let d = velt_rt_weakref_deref(r);
        assert_eq!(d, o);
        assert_eq!(count(o), 2, "deref returns a counted reference");
        release(d);
        release(o);
        assert!(
            velt_rt_weakref_deref(r).is_null(),
            "cleared when the target is freed"
        );
        unsafe { velt_rt_weakref_drop(r) };

        let o = new_obj(&[]);
        let r = unsafe { velt_rt_weakref_new(o) };
        unsafe { velt_rt_weakref_drop(r) };
        assert!(!is_marked(o), "the last WeakRef dropped unmarks its target");
        release(o);
        assert_eq!(tracked_objects(), 0);
    });
}

#[test]
fn handles_are_reused() {
    let a = obj_map();
    finish(&[a]);
    let b = obj_map();
    assert_eq!(a, b);
    finish(&[b]);
}
