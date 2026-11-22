//! The OS generator through `getrandom`, as velt_rt's `velt_rt_random_bytes` /
//! `velt_rt_random_u64` drew it: a `Vec` per draw.

fn random_vec(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    getrandom::fill(&mut v).expect("the OS random number generator failed");
    v
}

fn random_u64() -> u64 {
    let v = random_vec(8);
    u64::from_le_bytes(v.try_into().unwrap())
}

fn random_int(min: i64, max: i64) -> i64 {
    let range = (max - min) as u64;
    let reject = range.wrapping_neg() % range;
    loop {
        let r = random_u64();
        if r >= reject {
            return min + (r % range) as i64;
        }
    }
}

fn uuidv4() -> String {
    let mut b = random_vec(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(36);
    for (i, x) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push(HEX[(x >> 4) as usize] as char);
        s.push(HEX[(x & 15) as usize] as char);
    }
    s
}

fn main() {
    let mut len = 0i64;
    for _ in 0..300_000 {
        len += random_vec(16).len() as i64;
    }
    let mut inside = 0i64;
    for i in 0..300_000i64 {
        let v = random_int(-i, 1000 + i);
        if v >= -i && v < 1000 + i {
            inside += 1;
        }
    }
    let mut dashes = 0i64;
    for _ in 0..300_000 {
        let u = uuidv4();
        if u.as_bytes()[8] == b'-' && u.as_bytes()[14] == b'4' {
            dashes += 1;
        }
    }
    println!("{len} {inside} {dashes}");
}
