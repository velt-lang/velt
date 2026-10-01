// Hash maps (same workload as hashmap.vlt), with std's HashMap (SipHash, the default hasher).
use std::collections::HashMap;

fn int_map(n: i64) -> i64 {
    let mut m: HashMap<i64, i64> = HashMap::new();
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
    let mut counts: HashMap<String, i64> = HashMap::new();
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
