//! `brotli_stream_small.velt` on the `brotli` crate at quality 4 (window 2^22): 1,000 streams of
//! 1 KB from the 1 MB JSON of `json`, one `CompressorWriter` each.

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
    for i in 0..1000 {
        start = i * 1000;
        last = Vec::new();
        {
            let mut w = brotli::CompressorWriter::with_params(&mut last, 4096, &params);
            w.write_all(&input[start..start + 1024]).unwrap();
        }
        total += 1024;
    }
    let mut back = Vec::new();
    brotli_decompressor::BrotliDecompress(&mut &last[..], &mut back).unwrap();
    println!("{total} {}", back == input[start..start + 1024]);
}
