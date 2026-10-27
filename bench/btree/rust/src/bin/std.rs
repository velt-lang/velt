//! Rust's std `BTreeMap` on btree.vlt's workloads (no batch lookup: `get16` is 16 `get`s).
use btree_bench::{fold, ints, strings, Map};
use std::collections::BTreeMap;

struct Std<K>(BTreeMap<K, u64>);

impl<K: Ord + Copy + Into<u64>> Map<K> for Std<K> {
    fn new() -> Self {
        Std(BTreeMap::new())
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn insert(&mut self, k: K, v: u64) {
        self.0.insert(k, v);
    }
    fn get(&self, k: &K) -> Option<u64> {
        self.0.get(k).copied()
    }
    fn get16(&self, keys: [&K; 16]) -> [Option<u64>; 16] {
        keys.map(|k| self.0.get(k).copied())
    }
    fn walk(&self) -> u64 {
        fold(self.0.iter())
    }
    fn remove(&mut self, k: &K) -> bool {
        self.0.remove(k).is_some()
    }
}

struct StdStr(BTreeMap<String, u64>);

impl Map<String> for StdStr {
    fn new() -> Self {
        StdStr(BTreeMap::new())
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn insert(&mut self, k: String, v: u64) {
        self.0.insert(k, v);
    }
    fn get(&self, k: &String) -> Option<u64> {
        self.0.get(k).copied()
    }
    fn get16(&self, keys: [&String; 16]) -> [Option<u64>; 16] {
        keys.map(|k| self.0.get(k).copied())
    }
    fn walk(&self) -> u64 {
        0
    }
    fn remove(&mut self, k: &String) -> bool {
        self.0.remove(k).is_some()
    }
}

fn main() {
    ints::<Std<u64>>(1_000_000);
    strings::<StdStr>(200_000);
}
