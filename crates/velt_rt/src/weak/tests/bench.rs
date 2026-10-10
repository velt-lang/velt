//! Cost of the weak flag on retain/release/free, measured in instructions (valgrind, see
//! `crates/velt_rt/scripts/weak_rc_cost.sh`). Each loop inlines the release sequence the compiler
//! would emit: `plain` is today's (a type never weakly held), `capable` the proposed one for a
//! weak-capable type whose object is not weakly held (a signed compare), `bittest` the
//! alternative (a separate test of the flag), and `weak_shared` runs the `capable` loop on an
//! object that is a weak key (the cold path).

use super::*;
use std::hint::black_box;

/// Today's release: `if c == 1 { destroy } else { c - 1 }`.
#[inline(always)]
fn release_plain(obj: *mut u8) {
    // SAFETY: the loops own the reference they release.
    unsafe {
        let rc = rc_word(obj);
        let c = *rc;
        if c == 1 {
            destroy(obj);
        } else {
            *rc = c - 1;
        }
    }
}

#[inline(always)]
fn retain_inline(obj: *mut u8) {
    // SAFETY: the loops pass live objects.
    unsafe { *rc_word(obj) += 1 };
}

/// The shared path (retain + release; the count never reaches zero) and the unique path
/// (allocate, release: drop and free), `n` times each, with release sequence `$release`.
macro_rules! bench_loops {
    ($shared:ident, $unique:ident, $release:path) => {
        #[inline(never)]
        fn $shared(obj: *mut u8, n: u64) {
            for _ in 0..n {
                let o = black_box(obj);
                retain_inline(o);
                $release(o);
            }
        }

        #[inline(never)]
        fn $unique(n: u64) {
            for _ in 0..n {
                $release(black_box(new_obj(&[])));
            }
        }
    };
}

bench_loops!(bench_plain_shared, bench_plain_unique, release_plain);
bench_loops!(bench_capable_shared, bench_capable_unique, release);
bench_loops!(bench_bittest_shared, bench_bittest_unique, release_bit_test);

/// The alternative to `release`: today's `c == 1` test first, then a test of the flag.
#[inline(always)]
fn release_bit_test(obj: *mut u8) {
    // SAFETY: the loops own the reference they release.
    unsafe {
        let rc = rc_word(obj);
        let c = *rc;
        if c == 1 {
            destroy(obj);
        } else if c & RC_WEAK != 0 {
            if velt_rt_weak_release(obj) != 0 {
                destroy(obj);
            }
        } else {
            *rc = c - 1;
        }
    }
}

/// Runs the loop `VELT_WEAK_BENCH` names (all of them when unset), `VELT_WEAK_BENCH_N` times.
/// `weak_shared` is `bench_capable_shared` on an object that is a weak key: the cold path.
#[test]
#[ignore = "a measurement, run under valgrind by scripts/weak_rc_cost.sh"]
fn rc_paths() {
    let n = std::env::var("VELT_WEAK_BENCH_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let only = std::env::var("VELT_WEAK_BENCH").ok();
    let run = |name: &str| only.as_deref().is_none_or(|o| o == name);
    no_leak(|| {
        let obj = new_obj(&[]);
        if run("plain_shared") {
            bench_plain_shared(obj, n);
        }
        if run("plain_unique") {
            bench_plain_unique(n);
        }
        if run("capable_shared") {
            bench_capable_shared(obj, n);
        }
        if run("capable_unique") {
            bench_capable_unique(n);
        }
        if run("bittest_shared") {
            bench_bittest_shared(obj, n);
        }
        if run("bittest_unique") {
            bench_bittest_unique(n);
        }
        let m = obj_map();
        set(m, obj, new_obj(&[]));
        if run("weak_shared") {
            bench_capable_shared(obj, n);
        }
        release(obj);
        finish(&[m]);
    });
}
