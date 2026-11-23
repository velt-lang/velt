use std::hint::black_box;

fn main() {
    let mut total: i64 = 0;
    for round in 0..2_000_000usize {
        let mut b = black_box(vec![0u8; 16]);
        b[round % 16] = 1;
        total += b[(round + 1) % 16] as i64;
    }
    for round in 0..500_000usize {
        let b = black_box(vec![0u8; 1024]);
        total += b[round % 1024] as i64 + b.len() as i64;
    }
    for round in 0..2000usize {
        let b = black_box(vec![0u8; 1_048_576]);
        total += b[(round * 4096) % 1_048_576] as i64 + b.len() as i64;
    }
    for round in 0..16usize {
        let b = black_box(vec![0u8; 67_108_864]);
        total += b[round * 4096] as i64 + b.len() as i64;
    }
    println!("{total}");
}
