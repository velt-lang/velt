// Hash maps (same workload as hashmap.vlt), with std's HashMap and an FxHash-style hasher (as
// in rustc-hash: rotate / xor / multiply per 8-byte word) instead of SipHash. Not part of the
// harness table (run.ps1 pairs each .vlt with bench/rust/<name>.rs); RESULTS.md reports it as a
// second Rust reference for the hashmap benchmark.
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default)]
struct FxHasher(u64);

const K: u64 = 0x517c_c1b7_2722_0a95;

impl FxHasher {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(K);
    }
}

impl Hasher for FxHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut words = bytes.chunks_exact(8);
        for w in &mut words {
            self.add(u64::from_le_bytes(w.try_into().expect("8-byte chunk")));
        }
        for &b in words.remainder() {
            self.add(b as u64);
        }
    }
    fn write_u8(&mut self, x: u8) {
        self.add(x as u64);
    }
    fn write_u64(&mut self, x: u64) {
        self.add(x);
    }
    fn write_i64(&mut self, x: i64) {
        self.add(x as u64);
    }
    fn write_usize(&mut self, x: usize) {
        self.add(x as u64);
    }
}

type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;

fn int_map(n: i64) -> i64 {
    let mut m: FxMap<i64, i64> = FxMap::default();
    let mut x = 1i64;
    for i in 0..n {
        x = (x * 48271) % 2147483647;
        m.insert(x % 2000000, i);
    }
    let mut hits = 0i64;
    let mut sum = 0i64;
    for k in 0..n {
        if let Some(v) = m.get(&(k * 2)) {
            hits += 1;
            sum += v;
        }
    }
    println!("{} {} {}", m.len(), hits, sum);
    hits
}

fn word_count(n: i64) {
    let mut counts: FxMap<String, i64> = FxMap::default();
    let mut x = 7i64;
    for _ in 0..n {
        x = (x * 48271) % 2147483647;
        let word = format!("w{}", x % 50000);
        *counts.entry(word).or_insert(0) += 1;
    }
    let mut best = 0i64;
    let mut total = 0i64;
    for (_, &c) in &counts {
        total += c;
        if c > best {
            best = c;
        }
    }
    println!("{} {} {} {}", counts.len(), total, best, counts.get("w123").copied().unwrap_or(0));
}

fn main() {
    int_map(1_000_000);
    word_count(1_000_000);
}
