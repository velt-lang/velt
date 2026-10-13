fn main() {
    let mut total: i64 = 0;
    let mut buf = ryu::Buffer::new();
    for i in 0..1_000_000i64 {
        let x = (2 * i + 1) as f64 / 14.0;
        total += buf.format(x).len() as i64;
        total += buf.format(x / 1024.0).len() as i64;
    }
    println!("{total}");
}
