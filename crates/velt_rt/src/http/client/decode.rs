//! Decoding a response body the server compressed (`content-encoding: gzip`, `deflate`, `br`),
//! as Node's `fetch` does, one received frame at a time: each frame goes into a push decoder whose
//! output is the next decoded chunk, so a large download is never held compressed and decoded
//! at once. The headers stay as received (`content-encoding`, `content-length` of the encoded
//! body), as in Node.

use crate::result::{code, VeltErr};
use std::io::Write;

/// What `fetch` asks servers for when the request names nothing (undici's values).
pub(super) fn accept_encoding(https: bool) -> &'static str {
    if https {
        "br, gzip, deflate"
    } else {
        "gzip, deflate"
    }
}

/// A body decoder for one `content-encoding`.
pub(super) enum Decoder {
    Identity,
    Gzip(flate2::write::MultiGzDecoder<Vec<u8>>),
    /// `deflate`: zlib-wrapped, or raw (some servers send it raw); decided by the first byte.
    Deflate(Option<DeflateKind>),
    Brotli(Box<brotli_decompressor::DecompressorWriter<Vec<u8>>>),
}

pub(super) enum DeflateKind {
    Zlib(flate2::write::ZlibDecoder<Vec<u8>>),
    Raw(flate2::write::DeflateDecoder<Vec<u8>>),
}

fn failed(e: &dyn std::fmt::Display) -> VeltErr {
    VeltErr::new(code::INVALID_DATA, &format!("fetch failed: invalid compressed body: {e}"))
}

impl Decoder {
    /// The decoder for a `content-encoding` header value (`None`: none); an encoding `fetch`
    /// does not decode leaves the body as it is, as in Node.
    pub fn for_encoding(encoding: Option<&str>) -> Decoder {
        match encoding.map(|e| e.trim().to_ascii_lowercase()).as_deref() {
            Some("gzip" | "x-gzip") => Decoder::Gzip(flate2::write::MultiGzDecoder::new(vec![])),
            Some("deflate") => Decoder::Deflate(None),
            Some("br") => Decoder::Brotli(Box::new(
                brotli_decompressor::DecompressorWriter::new(vec![], 16 << 10),
            )),
            _ => Decoder::Identity,
        }
    }

    pub fn is_identity(&self) -> bool {
        matches!(self, Decoder::Identity)
    }

    /// Decode `data`, the next frame; returns what it decoded to (possibly nothing yet).
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<u8>, VeltErr> {
        let out = match self {
            Decoder::Identity => return Ok(data.to_vec()),
            Decoder::Gzip(d) => d.write_all(data).map(|()| d.get_mut()),
            Decoder::Deflate(kind) => {
                let k = kind.get_or_insert_with(|| deflate_kind(data));
                match k {
                    DeflateKind::Zlib(d) => d.write_all(data).map(|()| d.get_mut()),
                    DeflateKind::Raw(d) => d.write_all(data).map(|()| d.get_mut()),
                }
            }
            Decoder::Brotli(d) => d.write_all(data).map(|()| d.get_mut()),
        };
        Ok(std::mem::take(out.map_err(|e| failed(&e))?))
    }

    /// The end of the body: what is left, or an error if the encoded body was cut off.
    pub fn finish(&mut self) -> Result<Vec<u8>, VeltErr> {
        let out = match self {
            Decoder::Identity | Decoder::Deflate(None) => return Ok(vec![]),
            Decoder::Gzip(d) => d.try_finish().map(|()| d.get_mut()),
            Decoder::Deflate(Some(DeflateKind::Zlib(d))) => d.try_finish().map(|()| d.get_mut()),
            Decoder::Deflate(Some(DeflateKind::Raw(d))) => d.try_finish().map(|()| d.get_mut()),
            Decoder::Brotli(d) => match d.close() {
                Ok(()) => Ok(d.get_mut()),
                Err(e) => Err(e),
            },
        };
        Ok(std::mem::take(out.map_err(|e| failed(&e))?))
    }
}

/// zlib (RFC 1950) when the first byte is a zlib header (compression method 8), else raw
/// deflate (RFC 1951), as browsers and undici accept both for `deflate`.
fn deflate_kind(first: &[u8]) -> DeflateKind {
    if first.first().is_some_and(|b| b & 0x0f == 8) {
        DeflateKind::Zlib(flate2::write::ZlibDecoder::new(vec![]))
    } else {
        DeflateKind::Raw(flate2::write::DeflateDecoder::new(vec![]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
    use flate2::Compression;

    const TEXT: &[u8] = b"hello hello hello hello, fetch decodes this body frame by frame";

    fn decode(enc: &str, data: &[u8], frame: usize) -> Result<Vec<u8>, VeltErr> {
        let mut d = Decoder::for_encoding(Some(enc));
        let mut out = vec![];
        for chunk in data.chunks(frame) {
            out.extend(d.push(chunk)?);
        }
        out.extend(d.finish()?);
        Ok(out)
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(vec![], Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn gzip_deflate_in_any_framing() {
        let gz = gzip(TEXT);
        let mut z = ZlibEncoder::new(vec![], Compression::default());
        z.write_all(TEXT).unwrap();
        let zlib = z.finish().unwrap();
        let mut r = DeflateEncoder::new(vec![], Compression::default());
        r.write_all(TEXT).unwrap();
        let raw = r.finish().unwrap();
        for frame in [1, 7, 4096] {
            assert_eq!(decode("gzip", &gz, frame).unwrap(), TEXT);
            assert_eq!(decode("X-GZIP", &gz, frame).unwrap(), TEXT);
            assert_eq!(decode("deflate", &zlib, frame).unwrap(), TEXT);
            assert_eq!(decode("deflate", &raw, frame).unwrap(), TEXT);
        }
        // Concatenated gzip members decode as one body, as zlib's gunzip does.
        let two = [gzip(b"ab"), gzip(b"cd")].concat();
        assert_eq!(decode("gzip", &two, 3).unwrap(), b"abcd");
    }

    #[test]
    fn node_gzip() {
        // Node's `zlib.gzipSync("hello gzip")` (OS byte 10).
        let gz: &[u8] = &[
            31, 139, 8, 0, 0, 0, 0, 0, 0, 10, 203, 72, 205, 201, 201, 87, 72, 175, 202, 44, 0, 0,
            25, 106, 210, 223, 10, 0, 0, 0,
        ];
        for frame in [1, 30] {
            assert_eq!(decode("gzip", gz, frame).unwrap(), b"hello gzip");
        }
    }

    #[test]
    fn brotli() {
        // Node's `zlib.brotliCompressSync("hello brotli")`.
        let br: &[u8] = &[
            0x8b, 0x05, 0x80, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x20, 0x62, 0x72, 0x6f, 0x74, 0x6c,
            0x69, 0x03,
        ];
        for frame in [1, 5, 64] {
            assert_eq!(decode("br", br, frame).unwrap(), b"hello brotli");
        }
    }

    #[test]
    fn corrupt_and_truncated_bodies_fail() {
        assert_eq!(decode("gzip", b"not gzip at all", 4).err().unwrap().code, code::INVALID_DATA);
        let gz = gzip(TEXT);
        let cut = &gz[..gz.len() - 6];
        assert_eq!(decode("gzip", cut, 8).err().unwrap().code, code::INVALID_DATA);
    }

    #[test]
    fn identity_and_unknown_encodings_pass_through() {
        assert!(Decoder::for_encoding(None).is_identity());
        assert!(Decoder::for_encoding(Some("zstd")).is_identity());
        assert_eq!(decode("identity", TEXT, 5).unwrap(), TEXT);
        assert_eq!(accept_encoding(true), "br, gzip, deflate");
        assert_eq!(accept_encoding(false), "gzip, deflate");
    }
}
