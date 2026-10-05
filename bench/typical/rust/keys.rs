use std::collections::{HashMap, HashSet};
fn main() {
    let mut grid: HashMap<String, f64> = HashMap::new();
    for y in 0..300 { for x in 0..300 { grid.insert(format!("{},{}", x, y), ((x * y) % 10) as f64); } }
    let mut sum = 0.0;
    for round in 0..10 { for y in 1..299 { for x in 1..299 {
        let k = format!("{},{}", x + (round % 2), y);
        sum += grid.get(&k).copied().unwrap_or(0.0);
    } } }
    let mut seen: HashSet<String> = HashSet::new();
    let mut dup = 0;
    for i in 0..500000i64 { let k = format!("id-{}", (i * 7919) % 200000); if seen.contains(&k) { dup += 1; } else { seen.insert(k); } }
    println!("{} {} {}", sum, seen.len(), dup);
}
