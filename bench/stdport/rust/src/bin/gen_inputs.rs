//! Writes the compressed inputs of the decompression benchmarks into the directory given as the
//! only argument (run.sh passes `$OUT/inputs` and exports it as `STDPORT_INPUTS`): `json.gz` is
//! a 1 MB JSON array of users (the body of bench/http/fetch's `/json`), gzip-compressed at the
//! default level; `json.q5.br` and `json.q11.br` the same JSON compressed with Brotli at quality 5
//! and 11 (window 2^22, the `brotli` crate's defaults otherwise).

use std::io::Write;

/// The JSON document: deterministic, about 1 MB.
pub fn users() -> Vec<u8> {
    let mut s = String::from("[");
    let mut i = 0u64;
    while s.len() < 1 << 20 {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"id\":{i},\"name\":\"user {i}\",\"email\":\"user{i}@example.com\",\"active\":{},\"score\":{}.5}}",
            i % 3 == 0,
            (i * 7919) % 1000
        ));
        i += 1;
    }
    s.push(']');
    s.into_bytes()
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: gen_inputs <dir>");
    std::fs::create_dir_all(&dir).unwrap();
    let json = users();
    let mut gz = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    gz.write_all(&json).unwrap();
    std::fs::write(format!("{dir}/json.gz"), gz.finish().unwrap()).unwrap();
    for quality in [5, 11] {
        let mut br = vec![];
        let params = brotli::enc::BrotliEncoderParams {
            quality,
            lgwin: 22,
            ..Default::default()
        };
        brotli::BrotliCompress(&mut &json[..], &mut br, &params).unwrap();
        std::fs::write(format!("{dir}/json.q{quality}.br"), br).unwrap();
    }
}
