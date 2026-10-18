use std::hint::black_box;

fn fill(kind: usize) -> Vec<u8> {
    let mut d = vec![0u8; 1048576 + 2];
    let (mut i, mut k) = (0usize, 0usize);
    while i < 1048576 {
        k += 1;
        if kind == 2 {
            d[i] = 0xe4;
            d[i + 1] = (0xb8 + k % 4) as u8;
            d[i + 2] = (0x80 + k % 64) as u8;
            i += 3;
        } else if kind == 1 && k % 8 == 0 {
            d[i] = 0xc3;
            d[i + 1] = (0xa0 + k % 30) as u8;
            i += 2;
        } else {
            d[i] = (97 + k % 26) as u8;
            i += 1;
        }
    }
    d.truncate(i);
    d
}

fn valid(d: &[u8]) -> bool {
    std::str::from_utf8(black_box(d)).is_ok()
}

fn main() {
    let mut ok: i64 = 0;
    let mut ascii = fill(0);
    for round in 0..300usize {
        ascii[round * 3001] = (65 + round % 26) as u8;
        ok += valid(&ascii) as i64;
    }
    let mut accents = fill(1);
    let mut cjk = fill(2);
    for round in 0..100usize {
        let at = accents.len() - 1 - round;
        accents[at] = (65 + round % 26) as u8;
        ok += valid(&accents) as i64;
        cjk[round * 3 + 2] = (0x80 + round % 64) as u8;
        ok += valid(&cjk) as i64;
    }
    let last = cjk.len() - 1;
    cjk[last] = 0x20;
    ok += if valid(&cjk) { 100 } else { 0 };
    let mut small = vec![0u8; 40];
    for round in 0..10_000_000usize {
        small[round % 40] = (32 + round % 90) as u8;
        ok += valid(&small) as i64;
    }
    println!("{ok} {} {} {}", ascii.len(), accents.len(), cjk.len());
}
