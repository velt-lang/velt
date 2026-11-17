use dashu_int::IBig;

fn low(x: &IBig) -> i64 {
    let mask = IBig::from(u64::MAX);
    u64::try_from(x & &mask).unwrap() as i64
}

fn main() {
    let mut acc: i64 = 0;
    let sizes = [100usize, 1000, 10000, 40000];
    let reps = [100000i64, 10000, 200, 20];
    let p = IBig::from(1000000007);
    for s in 0..4 {
        let mut x = IBig::from(7).pow(2 * sizes[s]);
        let y = IBig::from(3).pow(sizes[s]);
        for _ in 0..reps[s] {
            x += 1;
            acc = acc ^ low(&(&x / &y)) ^ low(&(&x % &y)) ^ low(&(&x % &p));
        }
    }
    println!("{acc}");
}
