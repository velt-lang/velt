//! `brotli_fast.velt` on the `brotli` crate at quality 1 (window 2^22): the 1 MB JSON of `json`,
//! compressed 20 times.

fn main() {
    let dir = std::env::var("STDPORT_INPUTS").expect("STDPORT_INPUTS");
    let input = std::fs::read(format!("{dir}/json")).unwrap();
    let params = brotli::enc::BrotliEncoderParams {
        quality: 1,
        lgwin: 22,
        ..Default::default()
    };
    let mut total = 0u64;
    let mut last = Vec::new();
    for _ in 0..20 {
        last.clear();
        brotli::BrotliCompress(&mut &input[..], &mut last, &params).unwrap();
        total += input.len() as u64;
    }
    let mut back = Vec::new();
    brotli_decompressor::BrotliDecompress(&mut &last[..], &mut back).unwrap();
    println!("{total} {}", back == input);
}
