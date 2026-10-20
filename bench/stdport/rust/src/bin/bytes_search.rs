use memchr::memchr;
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
    let last = text.len() - 1;
    text[last] = 33;
    let mut total: i64 = 0;
    for _ in 0..200 {
        let mut at = 0usize;
        while let Some(i) = memchr(10, &black_box(&text)[at..]) {
            total += (at + i) as i64;
            at += i + 1;
        }
    }
    for _ in 0..2000 {
        total += memchr(33, black_box(&text)).map_or(-1, |i| i as i64);
    }
    let mut header = vec![0u8; 64];
    for round in 0..5_000_000usize {
        header[round % 64] = (round % 251) as u8;
        if memchr(250, black_box(&header)).is_some() {
            total += 1;
        }
    }
    println!("{total}");
}
