use dashu_int::IBig;

fn main() {
    let mut digits: i64 = 0;
    let mut tail = String::new();
    for round in 0..200i64 {
        let mut f = IBig::from(1);
        for k in 2..=2000 + round {
            f *= k;
        }
        let g = &f * &f;
        let h = &g * &g;
        let q = &h / &f;
        let r: IBig = &h % (&g + IBig::from(12345));
        let text = q.to_string();
        digits += (text.len() + r.to_string().len()) as i64;
        tail = text[..24].to_string();
    }
    println!("{digits} {tail}");
}
