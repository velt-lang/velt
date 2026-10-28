//! pread.vlt's workloads in Rust: `FileExt::read_at` / `write_at`, `File::sync_all`.
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::time::Instant;

const PAGE: usize = 4096;
const READS: usize = 200_000;
const PASSES: usize = 8;
const SYNCS: usize = 200;

fn report(name: &str, start: Instant, check: u64) {
    println!("{name} {:.1} {check}", start.elapsed().as_secs_f64() * 1000.0);
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: pread_bench <file>");
    let f = File::open(&path).unwrap();
    let pages = (f.metadata().unwrap().len() as usize / PAGE) as u32;
    let mut page = vec![0u8; PAGE];

    let mut x: u32 = 2463534242;
    let mut sum: u64 = 0;
    let start = Instant::now();
    for _ in 0..READS {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let at = (x % pages) as u64;
        let n = f.read_at(&mut page, at * PAGE as u64).unwrap();
        sum += n as u64 + page[0] as u64;
    }
    report("random_4k", start, sum);

    let mut chunk = vec![0u8; 65536];
    sum = 0;
    let start = Instant::now();
    for _ in 0..PASSES {
        let mut at = 0u64;
        loop {
            let n = f.read_at(&mut chunk, at).unwrap();
            if n == 0 {
                break;
            }
            at += n as u64;
            sum += n as u64 + chunk[n - 1] as u64;
        }
    }
    report("sequential_64k", start, sum);
    drop(f);

    let wpath = format!("{path}.w");
    let w = OpenOptions::new().write(true).create(true).truncate(true).open(&wpath).unwrap();
    let mut rec = vec![0u8; 100];
    let start = Instant::now();
    for i in 0..SYNCS {
        rec[0] = (i & 255) as u8;
        w.write_at(&rec, (i * 100) as u64).unwrap();
        w.sync_all().unwrap();
    }
    report("pwrite_fsync", start, w.metadata().unwrap().len());
    drop(w);
    std::fs::remove_file(&wpath).unwrap();
}
