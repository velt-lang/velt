fn main() {
    let mut x: i64 = 1;
    let mut total: i64 = 0;
    let mut buf = itoa::Buffer::new();
    for i in 0..5_000_000i64 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let v = x >> (i % 60);
        total += buf.format(v).len() as i64;
    }
    println!("{total}");
}
