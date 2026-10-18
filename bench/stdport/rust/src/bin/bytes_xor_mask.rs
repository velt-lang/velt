use std::hint::black_box;

/// The runtime's `velt_rt_bytes_xor_mask` before the Velt port.
#[inline(never)]
fn xor_mask(s: &mut [u8], mask: u32) {
    let m = mask.to_be_bytes();
    let wide = u64::from_ne_bytes([m[0], m[1], m[2], m[3], m[0], m[1], m[2], m[3]]);
    let mut chunks = s.chunks_exact_mut(8);
    for c in &mut chunks {
        let v = u64::from_ne_bytes((&*c).try_into().unwrap()) ^ wide;
        c.copy_from_slice(&v.to_ne_bytes());
    }
    for (i, byte) in chunks.into_remainder().iter_mut().enumerate() {
        *byte ^= m[i & 3];
    }
}

fn mix(len: usize, rounds: usize) -> i64 {
    let mut data = vec![0u8; len];
    let mut mask: u32 = 0x9e3779b9;
    for _ in 0..rounds {
        xor_mask(black_box(&mut data), mask);
        mask = mask.wrapping_mul(1664525).wrapping_add(1013904223);
    }
    data.iter()
        .enumerate()
        .map(|(i, &b)| b as i64 * ((i % 7) as i64 + 1))
        .sum()
}

fn main() {
    println!(
        "{}",
        mix(125, 20_000_000) + mix(4096, 1_000_000) + mix(65536, 60_000)
    );
}
