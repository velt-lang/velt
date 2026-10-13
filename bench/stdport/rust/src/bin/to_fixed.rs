use std::fmt::Write;

fn main() {
    let mut total: i64 = 0;
    let mut s = String::new();
    for i in 0..1_000_000i64 {
        let x = (2 * i + 1) as f64 / 14.0;
        s.clear();
        write!(s, "{:.2}", x).unwrap();
        total += s.len() as i64 + *s.as_bytes().last().unwrap() as i64;
        s.clear();
        write!(s, "{:.6}", x / 1024.0).unwrap();
        total += s.len() as i64 + *s.as_bytes().last().unwrap() as i64;
    }
    println!("{total}");
}
