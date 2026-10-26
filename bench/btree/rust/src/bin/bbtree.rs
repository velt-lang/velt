//! bbtree's single-threaded `BBTreeMap` on btree.vlt's workloads (`get16`: its `get_many`).
use bbtree::unsync::BBTreeMap;
use btree_bench::{fold, ints, strings, Map};

struct Bb<K>(BBTreeMap<K, u64>);

impl Map<u64> for Bb<u64> {
    fn new() -> Self {
        Bb(BBTreeMap::new())
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn insert(&mut self, k: u64, v: u64) {
        self.0.insert(k, v);
    }
    fn get(&self, k: &u64) -> Option<u64> {
        self.0.get(k).copied()
    }
    fn get16(&self, keys: [&u64; 16]) -> [Option<u64>; 16] {
        self.0.get_many(keys).map(|v| v.copied())
    }
    fn walk(&self) -> u64 {
        fold(self.0.iter())
    }
    fn remove(&mut self, k: &u64) -> bool {
        self.0.remove(k).is_some()
    }
}

impl Map<String> for Bb<String> {
    fn new() -> Self {
        Bb(BBTreeMap::new())
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
        self.0.get_many(keys).map(|v| v.copied())
    }
    fn walk(&self) -> u64 {
        0
    }
    fn remove(&mut self, k: &String) -> bool {
        self.0.remove(k).is_some()
    }
}

fn main() {
    ints::<Bb<u64>>(1_000_000);
    strings::<Bb<String>>(200_000);
}
