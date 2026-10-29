//! The Rust twin of the Velt variants (dataview.vlt, ...): the same pages, decoded from `&[u8]` with `u16::from_be_bytes`
//! and `u32::from_be_bytes` (bounds-checked slices, as safe Rust writes it).

const PAGE: usize = 4096;
const PAGES: usize = 64;

fn build() -> Vec<u8> {
    let mut all = vec![0u8; PAGE * PAGES];
    let mut x: u32 = 2463534242;
    for p in 0..PAGES {
        let base = p * PAGE;
        let mut end = PAGE;
        let mut n = 0usize;
        let mut key: u64 = p as u64 * 100000;
        loop {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            key += u64::from(x & 0xffff) + 1;
            let child = x >> 3;
            let mut groups = vec![];
            let mut k = key;
            loop {
                groups.insert(0, (k % 128) as u8);
                k /= 128;
                if k == 0 {
                    break;
                }
            }
            let size = 4 + groups.len();
            let cell_at = end - size;
            if cell_at < 12 + 2 * (n + 1) {
                break;
            }
            all[base + cell_at..base + cell_at + 4].copy_from_slice(&child.to_be_bytes());
            for (g, &v) in groups.iter().enumerate() {
                all[base + cell_at + 4 + g] = v | if g + 1 < groups.len() { 128 } else { 0 };
            }
            all[base + 12 + 2 * n..base + 14 + 2 * n].copy_from_slice(&(cell_at as u16).to_be_bytes());
            end = cell_at;
            n += 1;
        }
        all[base] = 5;
        all[base + 3..base + 5].copy_from_slice(&(n as u16).to_be_bytes());
    }
    all
}

fn be16(page: &[u8], at: usize) -> usize {
    u16::from_be_bytes(page[at..at + 2].try_into().unwrap()) as usize
}

fn be32(page: &[u8], at: usize) -> u64 {
    u64::from(u32::from_be_bytes(page[at..at + 4].try_into().unwrap()))
}

#[inline(never)]
fn decode(all: &[u8]) -> u64 {
    let mut sum = 0u64;
    for page in all.chunks_exact(PAGE) {
        let n = be16(page, 3);
        for i in 0..n {
            let at = be16(page, 12 + 2 * i);
            let child = be32(page, at);
            let mut key = 0u64;
            let mut q = at + 4;
            for _ in 0..8 {
                let b = page[q];
                q += 1;
                key = key * 128 + u64::from(b & 127);
                if b < 128 {
                    break;
                }
            }
            sum += child + key;
        }
    }
    sum
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rounds: u64 = args[1].parse().unwrap();
    let all = std::hint::black_box(build());
    let cells: usize = (0..PAGES).map(|p| be16(&all[p * PAGE..], 3)).sum();
    let mut check = 0u64;
    for _ in 0..rounds {
        check += decode(std::hint::black_box(&all));
    }
    println!("{cells} {check}");
}
