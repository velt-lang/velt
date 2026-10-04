fn parse_age(s: &str) -> Result<f64, String> {
    match s.parse::<i64>() { Ok(n) if n >= 0 => Ok(n as f64), _ => Err(format!("bad age: {}", s)) }
}
fn find(xs: &[f64], v: f64) -> Option<usize> { xs.iter().position(|&x| x == v) }
fn divmod(a: f64, b: f64) -> (f64, f64) { ((a / b).floor(), a % b) }
fn main() {
    let inputs: Vec<String> = (0..1000).map(|i| if i % 50 == 0 { "x".to_string() } else { format!("{}", i % 120) }).collect();
    let (mut ok, mut bad) = (0.0, 0);
    for _ in 0..1000 { for s in &inputs { match parse_age(s) { Ok(n) => ok += n, Err(_) => bad += 1 } } }
    let xs: Vec<f64> = (0..1000).map(|i| ((i * 31) % 1000) as f64).collect();
    let mut hits = 0.0;
    for i in 0..20000 { if let Some(r) = find(&xs, (i % 1500) as f64) { hits += r as f64; } }
    let mut qs = 0.0;
    for i in 1..10000000i64 { let (q, r) = divmod(i as f64, 7.0); qs += q + r; }
    println!("{} {} {} {}", ok, bad, hits, qs);
}
