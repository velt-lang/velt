//! Cost of the weak flag on retain/release/free, measured in instructions (cachegrind, see
//! `crates/velt_rt/scripts/weak_rc_cost.sh`). Each loop inlines the release sequence the compiler
//! would emit: `plain` is today's (a type never weakly held), `capable` adds the flag test (a
//! weak-capable type whose object is not weakly held), `weak` runs on an object that is a weak
//! key (the cold path). `VELT_WEAK_BENCH_N` sets the iterations.

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

/// Shared path (retain + release, the count never reaches zero) and unique path (allocate,
/// release: drop and free), `n` times each.
macro_rules! bench_loop {
    ($name:ident, $release:path) => {
        #[inline(never)]
        fn $name(obj: *mut u8, n: u64) {
            for _ in 0..n {
                let o = black_box(obj);
                retain_inline(o);
                $release(o);
            }
            for _ in 0..n {
                $release(black_box(new_obj(&[])));
            }
        }
    };
}

bench_loop!(bench_plain, release_plain);
bench_loop!(bench_capable, release);

/// The shared path on a weakly held object (each release is a call and a table probe).
#[inline(never)]
fn bench_weak(obj: *mut u8, n: u64) {
    for _ in 0..n {
        let o = black_box(obj);
        retain_inline(o);
        release(o);
    }
}

#[test]
#[ignore = "a measurement, run under cachegrind by scripts/weak_rc_cost.sh"]
fn rc_paths() {
    let n = std::env::var("VELT_WEAK_BENCH_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    no_leak(|| {
        let obj = new_obj(&[]);
        bench_plain(obj, n);
        bench_capable(obj, n);
        let m = obj_map();
        set(m, obj, new_obj(&[]));
        bench_weak(obj, n);
        release(obj);
        finish(&[m]);
    });
}
