use std::hint::black_box;

/// Node's `src.copy(dst, targetStart, sourceStart, sourceEnd)`: as many bytes as fit.
fn copy(src: &[u8], dst: &mut [u8], target: usize, start: usize, end: usize) -> usize {
    let end = end.min(src.len());
    if target >= dst.len() || start >= end {
        return 0;
    }
    let count = (end - start).min(dst.len() - target);
    dst[target..target + count].copy_from_slice(&src[start..start + count]);
    count
}

fn main() {
    let mut total: i64 = 0;
    for round in 0..1_000_000usize {
        let mut b = black_box(vec![0u8; 16]);
        b[round % 16] = 1;
        total += b[(round + 1) % 16] as i64;
    }
    for round in 0..200_000usize {
        let b = black_box(vec![0u8; 1024]);
        total += b[round % 1024] as i64 + b.len() as i64;
    }
    for round in 0..500usize {
        let b = black_box(vec![0u8; 1048576]);
        total += b[round * 1000] as i64 + b.len() as i64;
    }
    for round in 0..8usize {
        let b = black_box(vec![0u8; 67108864]);
        total += b[round * 4096] as i64 + b.len() as i64;
    }
    for round in 0..200_000usize {
        let mut b: Vec<u8> = Vec::with_capacity(1024);
        // SAFETY: u8 has no invalid values; the one byte read is written first.
        unsafe { b.set_len(1024) };
        let b = black_box(&mut b);
        b[0] = (round % 251) as u8;
        total += b[0] as i64;
    }
    let sizes = [16usize, 1024, 1048576];
    let rounds = [1_000_000usize, 200_000, 300];
    for k in 0..3 {
        let n = sizes[k];
        let mut src: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
        let mut dst = vec![0u8; n * 2];
        for round in 0..rounds[k] {
            src[round % n] = (round % 7) as u8;
            let at = round % (n + 1);
            if at + n <= dst.len() {
                dst[at..at + n].copy_from_slice(black_box(&src));
            }
            total += copy(black_box(&src), &mut dst, round % n, 1, n) as i64;
            dst.copy_within(0..n, 1);
            let s = black_box(&src)[1..n].to_vec();
            total += dst[round % (n * 2)] as i64 + s[s.len() - 1] as i64 + s.len() as i64;
        }
    }
    println!("{total}");
}
