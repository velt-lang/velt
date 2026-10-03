//! `Math.random()` and `std/random` on WebAssembly: the wyrand generator of velt_rt's `prng`
//! (one 64-bit state, one multiply per number), seeded on first use from std's `RandomState`
//! (WASI's `random_get` under wasm32-wasip1) mixed with the clocks, which is all the browser
//! target has. Not for secrets, like the native one.

use std::cell::Cell;
use std::hash::{BuildHasher, Hasher};

/// wyrand's increment.
const WY_ADD: u64 = 0xa076_1d64_78bd_642f;
const WY_XOR: u64 = 0xe703_7ed1_a0b4_28db;

thread_local! {
    /// 0 = not seeded yet (a seed is never 0 after `seed()`).
    static STATE: Cell<u64> = const { Cell::new(0) };
}

fn seed() -> u64 {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_i64(crate::platform::epoch_ms());
    h.write_u64(crate::platform::monotonic_ms().to_bits());
    h.finish() | 1
}

fn next_u64() -> u64 {
    STATE.with(|s| {
        let mut state = s.get();
        if state == 0 {
            state = seed();
        }
        state = state.wrapping_add(WY_ADD);
        s.set(state);
        let t = (state as u128).wrapping_mul((state ^ WY_XOR) as u128);
        ((t >> 64) as u64) ^ (t as u64)
    })
}

/// `random()` / `Math.random()`: uniform in `[0, 1)` with 53 random bits.
#[no_mangle]
pub extern "C" fn velt_rt_prng_f64() -> f64 {
    (next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

/// `randomInt(min, max)` → uniform in `[min, max)` (unbiased; `max <= min` gives `min`).
#[no_mangle]
pub extern "C" fn velt_rt_prng_range(min: i64, max: i64) -> i64 {
    if max <= min {
        return min;
    }
    let span = max.wrapping_sub(min) as u64;
    // Lemire's multiply-shift with rejection of the biased low range.
    let threshold = span.wrapping_neg() % span;
    loop {
        let m = next_u64() as u128 * span as u128;
        if (m as u64) >= threshold {
            return min.wrapping_add((m >> 64) as i64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_in_range_and_varied() {
        let xs: Vec<f64> = (0..1000).map(|_| velt_rt_prng_f64()).collect();
        assert!(xs.iter().all(|x| (0.0..1.0).contains(x)));
        assert!(xs.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn ints_in_range() {
        for _ in 0..1000 {
            let n = velt_rt_prng_range(-3, 4);
            assert!((-3..4).contains(&n));
        }
        assert_eq!(velt_rt_prng_range(5, 5), 5);
    }
}
