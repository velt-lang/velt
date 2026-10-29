//! `brotli_decode.vlt` on `brotli-decompressor`, which the runtime's `fetch` decodes `br` bodies
//! with today: the 1 MB JSON of `json.q5.br` and `json.q11.br`, each decoded 50 times.

use std::io::Read;

fn main() {
    let dir = std::env::var("STDPORT_INPUTS").expect("STDPORT_INPUTS");
    let mut total = 0u64;
    let mut sum = 0u64;
    for quality in [5, 11] {
        let input = std::fs::read(format!("{dir}/json.q{quality}.br")).unwrap();
        for _ in 0..50 {
            let mut out = Vec::new();
            brotli_decompressor::Decompressor::new(&input[..], 1 << 16)
                .read_to_end(&mut out)
                .unwrap();
            total += out.len() as u64;
            sum += out[out.len() / 2] as u64;
        }
    }
    println!("{total} {sum}");
}
