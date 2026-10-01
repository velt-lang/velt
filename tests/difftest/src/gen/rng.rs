//! Deterministic xorshift64* generator: the same seed yields the same program on every platform,
//! which is what makes a failing seed a reproducible bug report.

/// Seeded pseudo-random source (no external dependency, stable across Rust versions).
pub struct Rng(u64);

impl Rng {
    /// A generator for `seed` (any value, including 0).
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03 | 1)
    }

    /// The next raw 64-bit value.
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n` (`n` > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// True with probability `percent`/100.
    pub fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo + 1) as u64) as i64
    }

    /// A uniformly chosen element (`xs` non-empty).
    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn same_seed_same_stream() {
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        assert!((0..100).all(|_| a.next() == b.next()));
        assert_ne!(Rng::new(0).next(), Rng::new(1).next());
    }

    #[test]
    fn range_is_inclusive() {
        let mut r = Rng::new(3);
        let xs: Vec<i64> = (0..1000).map(|_| r.range(-2, 2)).collect();
        assert!(xs.contains(&-2) && xs.contains(&2) && xs.iter().all(|x| (-2..=2).contains(x)));
    }
}
