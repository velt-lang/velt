fn main() {
    let mut texts: Vec<String> = Vec::new();
    let mut buf = ryu::Buffer::new();
    for i in 0..100_000i64 {
        let x = (2 * i + 1) as f64 / 14.0;
        texts.push(buf.format(x).to_string());
        texts.push(buf.format(x / 1024.0).to_string());
    }
    let mut sum = 0.0f64;
    for _ in 0..10 {
        for t in &texts {
            sum += t.parse::<f64>().unwrap();
        }
    }
    println!("{}", sum as i64);
}
