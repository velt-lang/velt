// Ordered maps: Rust's std BTreeMap, the reference for btree.vlt (same workloads and output).
use std::collections::BTreeMap;
use std::time::Instant;

fn next(x: u64) -> u64 {
    let y = x ^ (x << 13);
    let y = y ^ (y >> 7);
    y ^ (y << 17)
}

fn keys_of(n: usize, seed: u64) -> Vec<u64> {
    let mut x = seed;
    (0..n).map(|_| { x = next(x); x >> 1 }).collect()
}

fn report(name: &str, start: Instant, check: u64) {
    println!("{} {} {}", name, start.elapsed().as_millis(), check);
}

fn ints(n: usize) {
    let keys = keys_of(n, 88172645463325252);
    let misses = keys_of(n / 2, 1234567);
    let t = Instant::now();
    let mut m = BTreeMap::new();
    for (i, k) in keys.iter().enumerate() {
        m.insert(*k, i as u64);
    }
    report("u64_insert", t, m.len() as u64);
    let t = Instant::now();
    let mut sum = 0u64;
    for i in 0..n / 2 {
        if let Some(v) = m.get(&keys[(i * 7919) % n]) { sum = sum.wrapping_add(*v); }
        if let Some(v) = m.get(&misses[i]) { sum = sum.wrapping_add(*v); }
    }
    report("u64_get", t, sum);
    let t = Instant::now();
    let mut walk = 0u64;
    for (k, v) in &m { walk ^= k.wrapping_add(*v); }
    report("u64_walk", t, walk);
    let t = Instant::now();
    let mut gone = 0u64;
    for k in &keys { gone += m.remove(k).is_some() as u64; }
    report("u64_delete", t, gone + m.len() as u64);
}

fn strings(n: usize) {
    let mut x = 42u64;
    let keys: Vec<String> = (0..n).map(|_| { x = next(x); format!("user/{}/profile", x % 1000000) }).collect();
    let t = Instant::now();
    let mut m = BTreeMap::new();
    for (i, k) in keys.iter().enumerate() { m.insert(k.clone(), i as u64); }
    report("str_insert", t, m.len() as u64);
    let t = Instant::now();
    let mut sum = 0u64;
    for i in 0..n { if let Some(v) = m.get(&keys[(i * 7919) % n]) { sum = sum.wrapping_add(*v); } }
    report("str_get", t, sum);
    let t = Instant::now();
    let mut gone = 0u64;
    for k in &keys { gone += m.remove(k).is_some() as u64; }
    report("str_delete", t, gone + m.len() as u64);
}

fn main() {
    ints(1_000_000);
    strings(200_000);
}
