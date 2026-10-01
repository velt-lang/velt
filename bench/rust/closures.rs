// Closures (same workload as closures.vlt). Like JS `map`/`filter`, every step materializes a
// new Vec; the stored closure is a `Box<dyn Fn>` like an escaping Velt closure.
fn make_scaler(k: i64) -> Box<dyn Fn(i64) -> i64> {
    Box::new(move |x| x * k + 1)
}

fn main() {
    let mut xs: Vec<i64> = Vec::new();
    for i in 0..1_000_000 {
        xs.push(i);
    }
    let mut total = 0i64;
    for round in 0..20i64 {
        let k = round + 3;
        let m = round % 5 + 2;
        let mapped: Vec<i64> = xs.iter().map(|&x| x * k).collect();
        let kept: Vec<i64> = mapped.iter().copied().filter(|&x| x % m == 0).collect();
        total += kept.iter().fold(0i64, |acc, &x| (acc + x) % 1000000007);
        xs.iter().for_each(|&x| {
            total = (total + x * m) % 1000000007;
        });
    }
    println!("{}", total);

    let scale = make_scaler(7);
    let mut acc = 0i64;
    for i in 0..20_000_000i64 {
        acc = scale(acc + i) % 1000003;
    }
    println!("{}", acc);
}
