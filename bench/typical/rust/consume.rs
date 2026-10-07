fn is_word(c: u8) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'
}

fn parse(text: &str) -> [f64; 3] {
    let mut rest = text;
    let (mut words, mut numbers, mut sum) = (0.0, 0.0, 0.0);
    while !rest.is_empty() {
        let b = rest.as_bytes();
        let c = b[0];
        if c == b' ' || c == b'\n' {
            rest = &rest[1..];
        } else if c.is_ascii_digit() {
            let mut i = 1;
            while i < b.len() && b[i].is_ascii_digit() { i += 1; }
            numbers += 1.0;
            sum += i as f64;
            rest = &rest[i..];
        } else if is_word(c) {
            let mut i = 1;
            while i < b.len() && is_word(b[i]) { i += 1; }
            words += 1.0;
            sum += i as f64;
            rest = &rest[i..];
        } else {
            sum += c as f64;
            rest = &rest[1..];
        }
    }
    [words, numbers, sum]
}

fn make_text(size: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    let (mut n, mut x) = (0usize, 1i64);
    while n < size {
        x = (x * 48271) % 2147483647;
        let piece = match x % 3 {
            0 => format!("{} ", x % 1000),
            1 => format!("name_{}, ", x % 97),
            _ => format!("v{} = (a + b);\n", x % 13),
        };
        n += piece.len();
        parts.push(piece);
    }
    let mut text = parts.join("");
    text.truncate(size);
    text
}

fn main() {
    let total = 10_000_000;
    for size in [1000, 1_000_000, 10_000_000] {
        let text = make_text(size);
        let (mut words, mut numbers, mut sum) = (0.0, 0.0, 0.0);
        for _ in 0..total / size {
            let r = parse(&text);
            words += r[0];
            numbers += r[1];
            sum += r[2];
        }
        println!("{} {} {} {}", size, words, numbers, sum);
    }
}
