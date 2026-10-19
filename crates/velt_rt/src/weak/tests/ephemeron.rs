//! The ephemeron rule: an entry whose value holds its key strongly (`raw -> proxy`) is freed with
//! its key and value once nothing outside refers to either, in whatever order the outside
//! references go.

use super::*;

/// Every order of `0..n`.
fn orders(n: usize) -> Vec<Vec<usize>> {
    if n == 0 {
        return vec![Vec::new()];
    }
    let mut out = Vec::new();
    for rest in orders(n - 1) {
        for i in 0..=rest.len() {
            let mut o = rest.clone();
            o.insert(i, n - 1);
            out.push(o);
        }
    }
    out
}

/// Releases `refs` in each order, after `build` made them; asserts everything is freed.
fn in_every_order(n: usize, build: impl Fn(MapId) -> Vec<*mut u8>) {
    for order in orders(n) {
        no_leak(|| {
            let m = obj_map();
            let refs = build(m);
            for &i in &order {
                release(refs[i]);
            }
            assert_eq!(weakmap_len(m), 0, "order {order:?}");
            finish(&[m]);
        });
    }
}

#[test]
fn raw_and_proxy_are_both_freed() {
    in_every_order(2, |m| {
        let raw = new_obj(&[]);
        let p = proxy_of(m, raw, std::ptr::null_mut());
        assert!(is_marked(raw) && is_marked(p));
        vec![raw, p]
    });
}

#[test]
fn a_key_kept_alive_keeps_its_proxy() {
    no_leak(|| {
        let m = obj_map();
        let raw = new_obj(&[]);
        let p = proxy_of(m, raw, std::ptr::null_mut());
        let before = live();
        release(p);
        assert_eq!(live(), before, "the proxy lives while its key does");
        let again = proxy_of(m, raw, std::ptr::null_mut());
        assert_eq!(again, p, "signal(raw) === signal(raw)");
        release(again);
        release(raw);
        assert_eq!(live(), before - 2);
        finish(&[m]);
    });
}

#[test]
fn a_value_kept_alive_keeps_its_key() {
    no_leak(|| {
        let m = obj_map();
        let raw = new_obj(&[]);
        let p = proxy_of(m, raw, std::ptr::null_mut());
        let before = live();
        release(raw);
        assert_eq!(live(), before);
        assert_eq!(field(p, 0), raw);
        let got = get(m, raw);
        assert_eq!(got, Some(p), "the entry stays while its key is reachable");
        release(p);
        release(p);
        assert_eq!(live(), before - 2);
        finish(&[m]);
    });
}

#[test]
fn a_chain_of_entries() {
    // A -> B (B holds A), B -> C (C holds B): the outside may hold any of them.
    in_every_order(3, |m| {
        let a = new_obj(&[]);
        let b = new_obj(&[retain(a)]);
        set(m, a, retain(b));
        let c = new_obj(&[retain(b)]);
        set(m, b, retain(c));
        vec![a, b, c]
    });
}

#[test]
fn holding_the_end_of_a_chain_keeps_it_all() {
    no_leak(|| {
        let m = obj_map();
        let a = new_obj(&[]);
        let b = new_obj(&[retain(a)]);
        set(m, a, retain(b));
        let c = new_obj(&[retain(b)]);
        set(m, b, retain(c));
        release(a);
        release(b);
        assert_eq!(weakmap_len(m), 2);
        let got = get(m, a);
        assert_eq!(got, Some(b));
        release(b);
        release(c);
        assert_eq!(weakmap_len(m), 0);
        finish(&[m]);
    });
}

#[test]
fn nested_reactive_objects() {
    // raw1.child = raw2, and both have proxies in the same cache.
    in_every_order(4, |m| {
        let raw2 = new_obj(&[]);
        let raw1 = new_obj(&[retain(raw2)]);
        let p1 = proxy_of(m, raw1, std::ptr::null_mut());
        let p2 = proxy_of(m, raw2, std::ptr::null_mut());
        vec![raw1, raw2, p1, p2]
    });
}

#[test]
fn a_handler_that_captures_the_target() {
    // proxy -> handler -> raw: the handler is on a path back to the key.
    in_every_order(3, |m| {
        let raw = new_obj(&[]);
        let handler = new_obj(&[retain(raw)]);
        let p = proxy_of(m, raw, retain(handler));
        vec![raw, handler, p]
    });
}

#[test]
fn a_shared_handler_outside_the_cycle() {
    no_leak(|| {
        let m = obj_map();
        let handler = new_obj(&[]);
        for _ in 0..3 {
            let raw = new_obj(&[]);
            let p = proxy_of(m, raw, retain(handler));
            release(raw);
            release(p);
        }
        assert_eq!(weakmap_len(m), 0);
        assert_eq!(count(handler), 1);
        release(handler);
        finish(&[m]);
    });
}

#[test]
fn a_key_that_holds_its_proxy_leaves_a_strong_cycle() {
    // raw.self = proxy(raw): the entry goes once nothing outside refers to either, but raw and
    // the proxy still hold each other strongly. That is an ordinary strong cycle, which leaks as
    // every strong cycle does without a collector until one side is `weak` (#11).
    for order in orders(2) {
        no_leak(|| {
            let m = obj_map();
            let raw = new_obj(&[]);
            let p = proxy_of(m, raw, std::ptr::null_mut());
            set_field(raw, 0, retain(p));
            let before = live();
            let refs = [raw, p];
            for &i in &order {
                release(refs[i]);
            }
            assert_eq!(weakmap_len(m), 0, "order {order:?}");
            assert_eq!(live(), before, "the strong cycle remains");
            set_field(raw, 0, std::ptr::null_mut()); // what `weak` would do
            assert_eq!(live(), before - 2);
            finish(&[m]);
        });
    }
}

#[test]
fn a_key_mapped_to_itself() {
    no_leak(|| {
        let m = obj_map();
        let k = new_obj(&[]);
        set(m, k, retain(k));
        release(k);
        assert_eq!(weakmap_len(m), 0);
        finish(&[m]);
    });
}

#[test]
fn many_entries() {
    no_leak(|| {
        let m = obj_map();
        let handler = new_obj(&[]);
        let pairs: Vec<_> = (0..10_000)
            .map(|_| {
                let raw = new_obj(&[]);
                (raw, proxy_of(m, raw, retain(handler)))
            })
            .collect();
        assert_eq!(weakmap_len(m), 10_000);
        for (i, &(raw, p)) in pairs.iter().enumerate() {
            if i % 2 == 0 {
                release(raw);
                release(p);
            } else {
                release(p);
                release(raw);
            }
        }
        assert_eq!(weakmap_len(m), 0);
        release(handler);
        finish(&[m]);
    });
}

#[test]
fn a_value_graph_past_the_limit_is_kept() {
    // The documented gap: an insert that cannot see the value's whole graph records no cycle,
    // so the pair leaks (and is never freed early) until the entry is deleted.
    no_leak(|| {
        let m = obj_map();
        let raw = new_obj(&[]);
        let mut tail = retain(raw);
        for _ in 0..trial::LIMIT {
            tail = new_obj(&[tail]);
        }
        let p = new_obj(&[tail]);
        set(m, raw, retain(p));
        let before = live();
        release(raw);
        release(p);
        assert_eq!(live(), before, "kept: a leak, not a use-after-free");
        finish(&[m]);
    });
}

#[test]
fn a_path_added_after_insert_is_found_by_the_next_trial() {
    // The proxy gains a second reference to its key after insert. The release of the proxy runs
    // a trial (the key is still held outside), which records the key's new level.
    in_every_order(2, |m| {
        let raw = new_obj(&[]);
        let p = proxy_of(m, raw, std::ptr::null_mut());
        set_field(p, 2, retain(raw));
        vec![raw, p]
    });
}

#[test]
fn a_path_added_after_the_last_trial_delays_the_free() {
    // The gap: references to the key that a value gains after the last trial through it raise
    // the key's count above what the table knows, so dropping the outside reference runs no
    // trial and the pair is kept (a leak, never a use-after-free). It is freed once the extra
    // reference goes (that release runs the trial), or with the entry or the map.
    no_leak(|| {
        let m = obj_map();
        let raw = new_obj(&[]);
        release(proxy_of(m, raw, std::ptr::null_mut()));
        let p = get(m, raw).expect("the key is held");
        release(p); // the entry keeps it
        set_field(p, 2, retain(raw));
        let before = live();
        release(raw);
        assert_eq!(live(), before, "kept while the extra path exists");
        set_field(p, 2, std::ptr::null_mut());
        assert_eq!(live(), before - 2);
        finish(&[m]);
    });
}
