//! The workloads of `bench/btree/btree.vlt` over an ordered map from Rust: the same keys, in the
//! same order, and the same output (name, milliseconds, checksum), for `run.sh` to compare.
use std::time::Instant;

/// What the workloads need of a map.
pub trait Map<K>: Sized {
    fn new() -> Self;
    fn len(&self) -> usize;
    fn insert(&mut self, k: K, v: u64);
    fn get(&self, k: &K) -> Option<u64>;
    /// `get` of each of 16 keys (one call where the map has a batch lookup).
    fn get16(&self, keys: [&K; 16]) -> [Option<u64>; 16];
    fn walk(&self) -> u64;
    fn remove(&mut self, k: &K) -> bool;
}

fn next(x: u64) -> u64 {
    let y = x ^ (x << 13);
    let y = y ^ (y >> 7);
    y ^ (y << 17)
}

fn keys_of(n: usize, seed: u64) -> Vec<u64> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = next(x);
            x >> 1
        })
        .collect()
}

fn report(name: &str, start: Instant, check: u64) {
    println!(
        "{} {:.1} {}",
        name,
        start.elapsed().as_secs_f64() * 1000.0,
        check
    );
}

pub fn ints<M: Map<u64>>(n: usize) {
    let keys = keys_of(n, 88172645463325252);
    let misses = keys_of(n / 2, 1234567);
    let t = Instant::now();
    let mut m = M::new();
    for (i, k) in keys.iter().enumerate() {
        m.insert(*k, i as u64);
    }
    report("u64_insert", t, m.len() as u64);
    let t = Instant::now();
    let mut sum = 0u64;
    for i in 0..n / 2 {
        if let Some(v) = m.get(&keys[(i * 7919) % n]) {
            sum = sum.wrapping_add(v);
        }
        if let Some(v) = m.get(&misses[i]) {
            sum = sum.wrapping_add(v);
        }
    }
    report("u64_get", t, sum);
    let probes: Vec<u64> = (0..n / 2)
        .flat_map(|i| [keys[(i * 7919) % n], misses[i]])
        .collect();
    let t = Instant::now();
    let mut got: Vec<Option<u64>> = Vec::with_capacity(probes.len());
    for chunk in probes.chunks_exact(16) {
        got.extend(m.get16(std::array::from_fn(|j| &chunk[j])));
    }
    let many = got.iter().flatten().fold(0u64, |a, v| a.wrapping_add(*v));
    report("u64_get_many", t, many);
    let t = Instant::now();
    let walk = m.walk();
    report("u64_walk", t, walk);
    let t = Instant::now();
    let mut gone = 0u64;
    for k in &keys {
        gone += m.remove(k) as u64;
    }
    report("u64_delete", t, gone + m.len() as u64);
}

pub fn strings<M: Map<String>>(n: usize) {
    let mut x = 42u64;
    let keys: Vec<String> = (0..n)
        .map(|_| {
            x = next(x);
            format!("user/{}/profile", x % 1000000)
        })
        .collect();
    let t = Instant::now();
    let mut m = M::new();
    // The map owns its keys: a `String` each (Velt's map shares the caller's).
    for (i, k) in keys.iter().enumerate() {
        m.insert(k.clone(), i as u64);
    }
    report("str_insert", t, m.len() as u64);
    let t = Instant::now();
    let mut sum = 0u64;
    for i in 0..n {
        if let Some(v) = m.get(&keys[(i * 7919) % n]) {
            sum = sum.wrapping_add(v);
        }
    }
    report("str_get", t, sum);
    let t = Instant::now();
    let mut gone = 0u64;
    for k in &keys {
        gone += m.remove(k) as u64;
    }
    report("str_delete", t, gone + m.len() as u64);
}

/// `walk`'s fold of the entries in key order, as btree.vlt's `forEach` computes it.
pub fn fold<'a, K: 'a + Copy + Into<u64>>(entries: impl Iterator<Item = (&'a K, &'a u64)>) -> u64 {
    let mut walk = 0u64;
    for (k, v) in entries {
        walk ^= (*k).into().wrapping_add(*v);
    }
    walk
}
