// Building and summing a `Vec` of 1M `Copy` points, five times over: points_*.vlt in Rust.
#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}

fn build(n: usize, round: usize) -> Vec<Point> {
    let mut ps = Vec::new();
    for i in 0..n {
        ps.push(Point { x: i as f64 * 0.5 + round as f64, y: (i % 1000) as f64 * 0.25 });
    }
    ps
}

fn sum(ps: &[Point]) -> f64 {
    let mut s = 0.0;
    for p in ps {
        s += p.x * 0.001 + p.y;
    }
    s
}

fn main() {
    let mut total = 0.0;
    for r in 0..5 {
        let ps = build(1000000, r);
        total += sum(&ps);
    }
    println!("{:.3}", total);
}
