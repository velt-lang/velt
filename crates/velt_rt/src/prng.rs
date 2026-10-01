//! `std/random`: fast pseudo-random numbers for simulations, sampling, jitter and benchmarks
//! (`Math.random()`-like; **not** for secrets, which use `std/crypto`).
//!
//! Every thread has its own wyrand generator (64-bit state, one multiply per number; passes
//! PractRand/BigCrush), seeded from the OS generator on first use, so calls from concurrent tasks
//! never contend. Sequences are not reproducible across runs.

use std::cell::Cell;

/// wyrand's increment.
const WY_ADD: u64 = 0xa076_1d64_78bd_642f;
const WY_XOR: u64 = 0xe703_7ed1_a0b4_28db;

thread_local! {
    /// 0 = not seeded yet (a seed is never 0 after `seed()`).
    static STATE: Cell<u64> = const { Cell::new(0) };
}

fn seed() -> u64 {
    crate::random::velt_rt_random_u64() | 1
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

/// `random()`: uniform in `[0, 1)` with 53 random bits.
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
    fn floats_are_in_the_unit_interval_and_vary() {
        let xs: Vec<f64> = (0..1000).map(|_| velt_rt_prng_f64()).collect();
        assert!(xs.iter().all(|x| (0.0..1.0).contains(x)));
        let mean = xs.iter().sum::<f64>() / xs.len() as f64;
        assert!((0.4..0.6).contains(&mean), "mean {mean}");
    }

    #[test]
    fn ranges_cover_exactly_their_bounds() {
        let mut seen = [0u32; 6];
        for _ in 0..6000 {
            let v = velt_rt_prng_range(1, 7);
            assert!((1..7).contains(&v));
            seen[(v - 1) as usize] += 1;
        }
        assert!(seen.iter().all(|&n| n > 700), "{seen:?}");
        assert_eq!(velt_rt_prng_range(5, 5), 5);
        assert!((i64::MIN..i64::MAX).contains(&velt_rt_prng_range(i64::MIN, i64::MAX)));
    }
}
