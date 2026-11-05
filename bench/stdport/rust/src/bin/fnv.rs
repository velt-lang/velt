// FNV-1a: the same program as bench/stdport/fnv.velt, on the fold the runtime ran.
use std::hint::black_box;

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn main() {
    let keys: Vec<String> = (0..1024).map(|i| format!("user:{}", i * 7919)).collect();
    let mut total: u64 = 0;
    for i in 0..2_000_000 {
        total = total.wrapping_add(fnv1a64(black_box(keys[i % 1024].as_bytes())));
    }
    let mut data: Vec<u8> = (0..65536).map(|i| (i % 96 + 32) as u8).collect();
    let text = data.clone();
    for round in 0..300 {
        data[round] = 65;
        total = total
            .wrapping_add(fnv1a64(black_box(&text)))
            .wrapping_add(fnv1a64(black_box(&data)));
    }
    println!("{total}");
}
