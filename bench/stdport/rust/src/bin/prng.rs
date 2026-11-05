// The fast generator: the same program as bench/stdport/prng.velt, on the thread-local wyrand the
// runtime ran.
use std::cell::Cell;
use std::hash::{BuildHasher, Hasher};

const WY_ADD: u64 = 0xa076_1d64_78bd_642f;
const WY_XOR: u64 = 0xe703_7ed1_a0b4_28db;

thread_local! {
    static STATE: Cell<u64> = const { Cell::new(0) };
}

fn seed() -> u64 {
    std::collections::hash_map::RandomState::new().build_hasher().finish() | 1
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

#[inline(never)]
fn random() -> f64 {
    (next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

#[inline(never)]
fn random_int(min: i64, max: i64) -> i64 {
    if max <= min {
        return min;
    }
    let span = max.wrapping_sub(min) as u64;
    let threshold = span.wrapping_neg() % span;
    loop {
        let m = next_u64() as u128 * span as u128;
        if (m as u64) >= threshold {
            return min.wrapping_add((m >> 64) as i64);
        }
    }
}

fn main() {
    let mut inside: i64 = 0;
    for _ in 0..30_000_000 {
        if random() < 1.0 {
            inside += 1;
        }
    }
    for i in 0..10_000_000i64 {
        let v = random_int(-i, 1_000_000 + i);
        if v >= -i && v < 1_000_000 + i {
            inside += 1;
        }
    }
    println!("{inside}");
}
