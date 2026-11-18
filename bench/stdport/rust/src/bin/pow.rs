fn main() {
    let mut sum = 0.0f64;
    let mut x = 0.75f64;
    for i in 0..2_000_000i64 {
        let y = (i % 997) as f64 * 0.013 - 6.5;
        sum += libm::pow(x, y);
        x += 0.0000013;
    }
    println!("{}", sum as i64);
}
