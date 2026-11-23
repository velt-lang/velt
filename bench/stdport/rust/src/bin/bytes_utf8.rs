use std::hint::black_box;

/// A string's length in UTF-16 code units, as JS's `length`.
fn units(s: &str) -> i64 {
    s.encode_utf16().count() as i64
}

fn main() {
    let text: Vec<u8> = (0..8192).map(|i| (97 + i % 26) as u8).collect();
    let wide = "añ€".repeat(800).into_bytes();
    let mut bad = "0123456789012345678901234567890123456789".repeat(4).into_bytes();
    bad[20] = 0xff;
    let mut total: i64 = 0;
    for round in 0..2_000_000usize {
        let at = round % 4000;
        let s = String::from_utf8_lossy(&black_box(&text)[at..at + 12]).into_owned();
        total += s.len() as i64;
    }
    for round in 0..1_000_000usize {
        let at = round % 4000;
        let s = String::from_utf8_lossy(&black_box(&text)[at..at + 40]).into_owned();
        total += s.len() as i64;
    }
    for round in 0..50_000usize {
        let at = round % 4000;
        let s = String::from_utf8_lossy(&black_box(&text)[at..at + 4096]).into_owned();
        total += s.len() as i64;
    }
    for round in 0..20_000usize {
        let at = (round % 64) * 6;
        let s = String::from_utf8_lossy(&black_box(&wide)[at..at + 4096]).into_owned();
        total += units(&s);
    }
    for round in 0..200_000usize {
        let at = round % 20;
        let s = String::from_utf8_lossy(&black_box(&bad)[at..at + 40]).into_owned();
        total += units(&s);
    }
    println!("{total}");
}
