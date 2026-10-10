//! Leak soak: reactive-like objects created and dropped in a loop, the live object count checked
//! at every step (counted, not timed). `cargo test -p velt_rt weak::tests::soak -- --ignored`
//! runs the long version.

use super::*;

/// One reactive object as sigx makes it: a raw object (with a nested raw child every third
/// time), a handler whose closure captures the raw object, and the cached proxy of each raw.
struct Reactive {
    refs: Vec<*mut u8>,
}

/// Objects a `Reactive` with and without a child allocates.
const PLAIN: i64 = 3;
const NESTED: i64 = 6;

fn make(cache: MapId, i: u64) -> Reactive {
    let raw = new_obj(&[]);
    let handler = new_obj(&[retain(raw)]);
    let p = proxy_of(cache, raw, handler);
    let mut refs = vec![raw, p];
    if i.is_multiple_of(3) {
        let child = new_obj(&[]);
        set_field(raw, 1, retain(child));
        let child_handler = new_obj(&[retain(child)]);
        refs.push(proxy_of(cache, child, child_handler));
        refs.push(child);
    }
    // The outside lets go in an order that varies with `i`.
    let len = refs.len() as u64;
    refs.rotate_left((i % len) as usize);
    Reactive { refs }
}

fn objects(r: &Reactive) -> i64 {
    if r.refs.len() == 2 {
        PLAIN
    } else {
        NESTED
    }
}

/// Keeps a window of `window` reactive objects alive while `n` pass through it; the number of
/// live objects, side-table records and map entries must follow the window exactly.
fn soak(n: u64, window: usize) {
    no_leak(|| {
        let cache = obj_map();
        let base = live();
        let mut alive: std::collections::VecDeque<Reactive> = Default::default();
        let (mut expected, mut created, mut max_live, mut max_tracked) = (0, 0, 0, 0);
        for i in 0..n {
            let r = make(cache, i);
            expected += objects(&r);
            created += objects(&r);
            alive.push_back(r);
            if alive.len() > window {
                let r = alive.pop_front().expect("window is full");
                expected -= objects(&r);
                r.refs.into_iter().for_each(release);
            }
            assert_eq!(live() - base, expected, "iteration {i}");
            max_live = max_live.max(expected);
            max_tracked = max_tracked.max(tracked_objects());
        }
        // Every live object is a raw object, proxy or handler: at most all of them recorded.
        assert!(
            max_tracked as i64 <= max_live,
            "side table grew: {max_tracked}"
        );
        eprintln!(
            "soak: {n} reactive objects ({created} objects) through a window of {window}: \
             live objects at most {max_live}, side-table records at most {max_tracked}"
        );
        let entries: usize = alive.iter().map(|r| r.refs.len() / 2).sum();
        assert_eq!(velt_rt_weakmap_len(cache) as usize, entries);
        for r in alive {
            r.refs.into_iter().for_each(release);
        }
        assert_eq!(velt_rt_weakmap_len(cache), 0);
        finish(&[cache]);
    });
}

#[test]
fn soak_short() {
    soak(20_000, 64);
}

#[test]
#[ignore = "long: 1e6 reactive objects (run with --ignored)"]
fn soak_long() {
    soak(1_000_000, 1_000);
}
