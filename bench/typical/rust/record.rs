use std::collections::HashMap;
fn main() {
    let mut scores: HashMap<String, f64> = HashMap::new(); let mut order: Vec<String> = Vec::new();
    let names: Vec<String> = (0..2000).map(|i| format!("n{}", i)).collect();
    for round in 0..500 { let mut i = 0; while i < 2000 { let k = &names[i];
        match scores.get_mut(k) { Some(v) => *v += round as f64, None => { scores.insert(k.clone(), round as f64); order.push(k.clone()); } } i += 3; } }
    let mut total = 0.0; for k in &order { total += scores[k]; }
    let mut found = 0;
    for i in 0..20000 { let a = format!("n{}", i % 3000); if names.contains(&a) { found += 1; } let b = format!("n{}", i % 2500); if names.iter().position(|n| *n == b).is_some() { found += 1; } }
    let mut arr: Vec<f64> = Vec::new();
    for i in 0..2000 { let mut n = arr.clone(); n.push(i as f64); arr = n; }
    let mut joined: Vec<f64> = Vec::new();
    for i in 0..1000 { let mut n = joined.clone(); n.extend_from_slice(&[i as f64, (i + 1) as f64]); joined = n; }
    println!("{} {} {} {}", total, found, arr.len(), joined.len());
}
