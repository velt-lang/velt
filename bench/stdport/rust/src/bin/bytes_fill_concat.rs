use memchr::memrchr;
use std::hint::black_box;

fn main() {
    let mut text = vec![0u8; 1048576];
    let (mut next, mut width) = (20usize, 20usize);
    for i in 0..text.len() {
        text[i] = (97 + i % 26) as u8;
        if i == next {
            text[i] = 10;
            width = 20 + (width * 7 + 13) % 81;
            next += width;
        }
    }
    text[0] = 33;
    let mut total: i64 = 0;
    for _ in 0..200 {
        let mut end = text.len();
        while let Some(i) = memrchr(10, &black_box(&text)[..end]) {
            total += i as i64;
            end = i;
        }
    }
    for _ in 0..2000 {
        total += memrchr(33, black_box(&text)).map_or(-1, |i| i as i64);
    }
    let mut small = vec![0u8; 64];
    for round in 0..2_000_000usize {
        black_box(&mut small).fill((round % 251) as u8);
        total += small[round % 64] as i64;
    }
    let mut page = vec![0u8; 4096];
    for round in 0..50_000usize {
        black_box(&mut page)[1..4095].fill((round % 251) as u8);
        total += page[round % 4096] as i64;
    }
    for round in 0..500usize {
        black_box(&mut text).fill((round % 251) as u8);
        total += text[round] as i64;
    }
    let mut a = vec![0u8; 48];
    let b = vec![0u8; 48];
    for round in 0..1_000_000usize {
        a[round % 48] = (round % 251) as u8;
        let mut c = Vec::with_capacity(96);
        c.extend_from_slice(black_box(&a));
        c.extend_from_slice(black_box(&b));
        total += c.len() as i64 + c[round % 96] as i64;
    }
    let mut big = vec![0u8; 65536];
    for round in 0..2000usize {
        big[round] = (round % 251) as u8;
        let mut c = Vec::with_capacity(131072);
        c.extend_from_slice(black_box(&big));
        c.extend_from_slice(black_box(&big));
        total += c.len() as i64 + c[65536 + round] as i64;
    }
    println!("{total}");
}
