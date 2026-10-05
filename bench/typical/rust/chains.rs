fn main() {
    let xs: Vec<f64> = (0..1000000).map(|i| i as f64 * 0.5).collect();
    let mut total = 0.0;
    for round in 0..20 {
        let k = (round + 1) as f64;
        let m: Vec<f64> = xs.iter().map(|x| x * k).collect();
        let f: Vec<f64> = m.into_iter().filter(|x| x % 3.0 < 1.0).collect();
        total += f.iter().fold(0.0, |a, b| a + b);
        if xs.iter().any(|&x| x > 499990.0 + round as f64) { total += 1.0; }
        total += xs.iter().copied().find(|&x| x > 1000.0 * k).unwrap_or(0.0);
    }
    let ys: Vec<f64> = (0..2000000).map(|i| (i % 1000) as f64).collect();
    let d: Vec<f64> = ys.iter().map(|y| y * 2.0).collect::<Vec<_>>().into_iter().filter(|&y| y > 500.0).collect();
    println!("{} {} {}", total, d.len(), d.iter().fold(0.0, |a, b| a + b));
}
