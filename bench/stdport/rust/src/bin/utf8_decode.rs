use std::hint::black_box;

fn fill(n: usize, comma: bool) -> Vec<u8> {
    let mut d = Vec::with_capacity(n);
    let mut k = 0usize;
    while d.len() + 4 <= n {
        k += 1;
        if k % 6 == 0 {
            if comma {
                d.push(44);
            }
            d.push(32);
        } else {
            d.push(0xd0);
            d.push((0xb0 + k % 16) as u8);
        }
    }
    while d.len() < n {
        d.push(32);
    }
    d
}

/// The length in UTF-16 code units, as `string.length` in JS.
fn decode(d: &[u8]) -> i64 {
    let s = String::from_utf8_lossy(black_box(d));
    s.encode_utf16().count() as i64
}

fn main() {
    let mut total: i64 = 0;
    let mut words = fill(1048576, false);
    let mut commas = fill(1048576, true);
    for round in 0..100usize {
        let at = words.len() - 1 - round % 2;
        words[at] = (32 + round % 2) as u8;
        total += decode(&words);
        let at = commas.len() - 1 - round % 2;
        commas[at] = (32 + round % 2) as u8;
        total += decode(&commas);
    }
    let mut small = fill(40, false);
    for round in 0..2_000_000usize {
        small[39 - round % 2] = (32 + round % 2) as u8;
        total += decode(&small);
    }
    println!("{total}");
}
