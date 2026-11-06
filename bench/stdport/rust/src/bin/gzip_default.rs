//! `gzip_default.velt` on `flate2` (its default `miniz_oxide` backend) at level 6: the 1 MB JSON of
//! `json`, compressed 20 times.

use std::io::{Read, Write};

fn main() {
    let dir = std::env::var("STDPORT_INPUTS").expect("STDPORT_INPUTS");
    let input = std::fs::read(format!("{dir}/json")).unwrap();
    let mut total = 0u64;
    let mut last = Vec::new();
    for _ in 0..20 {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        gz.write_all(&input).unwrap();
        last = gz.finish().unwrap();
        total += input.len() as u64;
    }
    let mut back = Vec::new();
    flate2::bufread::MultiGzDecoder::new(&last[..])
        .read_to_end(&mut back)
        .unwrap();
    println!("{total} {}", back == input);
}
