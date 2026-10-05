fn dot(a: &[f64], b: &[f64]) -> f64 { let mut s = 0.0; for i in 0..a.len() { s += a[i] * b[i]; } s }
fn sieve(n: usize) -> usize {
    let mut flags = vec![true; n + 1];
    let mut count = 0;
    for i in 2..=n { if flags[i] { count += 1; let mut j = i * i; while j <= n { flags[j] = false; j += i; } } }
    count
}
fn main() {
    let mut a = Vec::new(); let mut b = Vec::new();
    for i in 0..1000000i64 { a.push(((i * 7919) % 1000) as f64 / 1000.0 - 0.5); b.push(((i * 104729i64) % 1000) as f64 / 1000.0 - 0.5); }
    let mut d = 0.0;
    for _ in 0..50 { d += dot(&a, &b); }
    let mut integral = 0.0;
    let n = 20000000; let h = 1.0 / n as f64;
    for i in 0..n { let x = (i as f64 + 0.5) * h; integral += (1.0 - x * x).sqrt() * h; }
    println!("{:.6} {:.9} {}", d, integral, sieve(10000000));
}
