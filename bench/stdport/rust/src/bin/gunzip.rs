//! `gunzip.vlt` on `flate2` (its default `miniz_oxide` backend), which the runtime's `fetch`
//! decodes gzip bodies with today: the 1 MB JSON of `json.gz`, decoded 50 times.

use std::io::Read;

fn main() {
    let dir = std::env::var("STDPORT_INPUTS").expect("STDPORT_INPUTS");
    let input = std::fs::read(format!("{dir}/json.gz")).unwrap();
    let mut total = 0u64;
    let mut sum = 0u64;
    for _ in 0..50 {
        let mut out = Vec::new();
        flate2::bufread::MultiGzDecoder::new(&input[..])
            .read_to_end(&mut out)
            .unwrap();
        total += out.len() as u64;
        sum += out[out.len() / 2] as u64;
    }
    println!("{total} {sum}");
}
