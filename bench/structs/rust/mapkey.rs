// A `HashMap` keyed by a small `Copy` struct (the default SipHash hasher): mapkey_*.vlt in Rust.
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Cell {
    x: i64,
    y: i64,
}

fn main() {
    let mut visits: HashMap<Cell, i64> = HashMap::new();
    let (mut x, mut y) = (0i64, 0i64);
    let mut seed: i64 = 1;
    for _ in 0..1000000 {
        seed = (seed * 48271) % 2147483647;
        match seed % 4 {
            0 => x += 1,
            1 => x -= 1,
            2 => y += 1,
            _ => y -= 1,
        }
        *visits.entry(Cell { x, y }).or_insert(0) += 1;
    }
    let most = visits.values().copied().max().unwrap_or(0);
    println!("{} {}", visits.len(), most);
}
