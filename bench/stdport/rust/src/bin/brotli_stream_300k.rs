//! `brotli_stream_300k.velt` on the `brotli` crate at quality 4 (window 2^22): 30 streams of 300 KB
//! from the 1 MB JSON of `json`, one `CompressorWriter` each.

use std::io::Write;

fn main() {
    let dir = std::env::var("STDPORT_INPUTS").expect("STDPORT_INPUTS");
    let input = std::fs::read(format!("{dir}/json")).unwrap();
    let params = brotli::enc::BrotliEncoderParams {
        quality: 4,
        lgwin: 22,
        ..Default::default()
    };
    let mut total = 0u64;
    let mut last = Vec::new();
    let mut start = 0;
    for i in 0..30 {
        start = i * 20000;
        last = Vec::new();
        {
            let mut w = brotli::CompressorWriter::with_params(&mut last, 4096, &params);
            w.write_all(&input[start..start + 307200]).unwrap();
        }
        total += 307200;
    }
    let mut back = Vec::new();
    brotli_decompressor::BrotliDecompress(&mut &last[..], &mut back).unwrap();
    println!("{total} {}", back == input[start..start + 307200]);
}
