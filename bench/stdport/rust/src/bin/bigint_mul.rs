use dashu_int::IBig;

fn low(x: &IBig) -> i64 {
    let mask = IBig::from(u64::MAX);
    u64::try_from(x & &mask).unwrap() as i64
}

fn main() {
    let mut acc: i64 = 0;
    let sizes = [100usize, 1000, 10000, 40000];
    let reps = [200000i64, 20000, 400, 40];
    for s in 0..4 {
        let mut a = IBig::from(3).pow(sizes[s]);
        let b = IBig::from(7).pow(sizes[s]);
        for _ in 0..reps[s] {
            a += 1;
            acc = acc ^ low(&(&a * &b)) ^ low(&(&a * &a));
        }
    }
    println!("{acc}");
}
