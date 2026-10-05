use std::collections::HashMap;
fn main() {
    let vocab: Vec<String> = (0..5000i64).map(|i| format!("w{}", (i * 7919) % 100003)).collect();
    let mut parts: Vec<&str> = Vec::new();
    let mut x: i64 = 1;
    for _ in 0..1000000 { x = (x * 48271) % 2147483647; parts.push(&vocab[(x % 5000) as usize]); }
    let text = parts.join(" ");
    let mut counts: HashMap<String, f64> = HashMap::new();
    for _ in 0..3 {
        for w in text.split(' ') { *counts.entry(w.to_string()).or_insert(0.0) += 1.0; }
    }
    let mut entries: Vec<(String, f64)> = counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
    entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
    println!("{} {} {} {} {}", counts.len(), entries[0].0, entries[0].1, entries[4999].0, entries[4999].1);
}
