// Closures held in locals (same workload as closures_held.vlt): plain Rust closures, which
// live on the stack.
fn main() {
    let words = ["alpha", "beta", "gamma", "delta"];
    let mut total = 0i64;
    for i in 0..3_000_000i64 {
        let k = i % 7 + 1;
        let m = i % 3;
        let scale = |x: i64| x * k + m;
        let w = words[(i % 4) as usize];
        let score = |n: i64| n + w.len() as i64 * k;
        total = (total + scale(i) + scale(i + 1) + score(i)) % 1000000007;
    }
    println!("{}", total);
}
