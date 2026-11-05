//! The seed of `Math.random()`'s generator (`std/lang/random/wyrand.velt`) on WebAssembly: std's
//! `RandomState` (WASI's `random_get` under wasm32-wasip1) mixed with the clocks, which is all
//! the browser target has. Not for secrets.

use std::hash::{BuildHasher, Hasher};

/// 64 bits to seed a generator with.
#[no_mangle]
pub extern "C" fn velt_rt_prng_seed() -> u64 {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_i64(crate::platform::epoch_ms());
    h.write_u64(crate::platform::monotonic_ms().to_bits());
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_differ() {
        assert_ne!(velt_rt_prng_seed(), velt_rt_prng_seed());
    }
}
