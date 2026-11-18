use dashu_int::IBig;

fn low(x: &IBig) -> i64 {
    let mask = IBig::from(u64::MAX);
    u64::try_from(x & &mask).unwrap() as i64
}

fn main() {
    let mut acc: i64 = 0;
    let sizes = [100usize, 1000, 10000, 40000];
    let reps = [100000i64, 5000, 100, 10];
    for s in 0..4 {
        let mut x = IBig::from(3).pow(sizes[s]);
        for _ in 0..reps[s] {
            x += 1;
            let text = x.to_string();
            let hex = x.in_radix(16).to_string();
            acc = acc
                ^ low(&IBig::from_str_radix(&text, 10).unwrap())
                ^ low(&IBig::from_str_radix(&hex, 16).unwrap());
            acc += (text.len() + hex.len()) as i64;
        }
    }
    println!("{acc}");
}
