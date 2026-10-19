// A closure created per iteration and only called (same workload as closures_borrowed.vlt).
fn score(name: &str, base: f64, scale: f64) -> f64 {
    let weigh = |k: f64| -> f64 {
        let n = name.len() as f64 + k;
        (base * scale + n) % 9973.0
    };
    weigh(1.0) + weigh(2.0)
}

fn main() {
    let names = ["ada", "grace", "linus", "barbara", "ken"];
    let mut total = 0.0f64;
    for i in 0..3_000_000u32 {
        let i = i as f64;
        total = (total + score(names[(i % 5.0) as usize], i % 1000.0, (i % 13.0) + 1.0)) % 1000003.0;
    }
    println!("{}", total);
}
