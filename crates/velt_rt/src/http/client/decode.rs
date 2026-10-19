//! Decoding a response body the server compressed (`content-encoding`), as Node's `fetch` does.
//!
//! Received frames are queued in a [`Feed`]; a chain of pull decoders (`flate2`'s gzip, zlib and
//! raw deflate readers, `brotli-decompressor`'s reader), one per listed coding and the last one
//! applied first, reads from it. Each pull decodes at most [`CHUNK`] bytes into one scratch
//! buffer and hands them over in a chunk of their own size, so a small body that
//! expands enormously still arrives in bounded chunks, never all at once. A decoder that runs
//! out of queued input reports `WouldBlock`, which the decoders resume from once more input is
//! queued. The headers stay as received (`content-encoding`, the encoded `content-length`).
//!
//! As in undici: `gzip` / `x-gzip` (several members), `deflate` (zlib-wrapped or raw, decided by
//! the first byte) and `br`, in any combination of up to [`MAX_CODINGS`]; a list naming another
//! coding is passed through undecoded; a coding with no input at all decodes to nothing. Unlike undici, a body cut off before its compressed
//! stream ends is an error, not partial text.

use crate::result::{code, VeltErr};
use bytes::{Buf, Bytes};
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read};
use std::sync::{Arc, Mutex};

/// The most a decoder hands back at once (Node's chunks are at most 16 KiB).
pub(super) const CHUNK: usize = 64 << 10;

/// The most codings one body may list (undici's limit).
const MAX_CODINGS: usize = 5;

/// What `fetch` asks servers for when the request names nothing (undici's values).
pub(super) fn accept_encoding(https: bool) -> &'static str {
    if https {
        "br, gzip, deflate"
    } else {
        "gzip, deflate"
    }
}

/// Received frames not decoded yet.
#[derive(Default)]
struct Feed {
    frames: VecDeque<Bytes>,
    /// The body is complete: no more frames will be queued.
    eof: bool,
}

/// The bottom of a decoder chain: reads the queued frames, `WouldBlock` when there are none yet.
#[derive(Clone, Default)]
pub(super) struct Input(Arc<Mutex<Feed>>);

impl Input {
    fn feed(&self) -> std::sync::MutexGuard<'_, Feed> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Read for Input {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut feed = self.feed();
        while let Some(front) = feed.frames.front_mut() {
            if front.is_empty() {
                feed.frames.pop_front();
                continue;
            }
            let n = front.len().min(out.len());
            out[..n].copy_from_slice(&front[..n]);
            front.advance(n);
            return Ok(n);
        }
        if feed.eof {
            Ok(0)
        } else {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
}

type Chain = Box<dyn Read + Send>;

/// A content coding `fetch` undoes.
#[derive(Clone, Copy)]
enum Coding {
    Gzip,
    Deflate,
    Brotli,
}

/// One coding undone. Its decoder is made once the first input byte arrived: an input that ends
/// with no bytes at all is an empty body, as in undici (zlib's and brotli's streams end empty),
/// not a missing gzip header or brotli stream; and `deflate` is zlib-wrapped (RFC 1950) when that
/// byte is a zlib header (compression method 8), else raw (RFC 1951), as browsers and undici
/// accept both.
struct Layer {
    coding: Coding,
    pending: Option<BufReader<Chain>>,
    decoder: Option<Chain>,
}

impl Layer {
    fn new(coding: Coding, input: Chain) -> Layer {
        Layer {
            coding,
            pending: Some(BufReader::with_capacity(16 << 10, input)),
            decoder: None,
        }
    }

    fn start(&mut self, first: u8) -> Chain {
        let inner = self.pending.take().expect("ICE: decoder input");
        match self.coding {
            Coding::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(inner)),
            Coding::Deflate if first & 0x0f == 8 => {
                Box::new(flate2::bufread::ZlibDecoder::new(inner))
            }
            Coding::Deflate => Box::new(flate2::bufread::DeflateDecoder::new(inner)),
            Coding::Brotli => Box::new(brotli_decompressor::Decompressor::new(inner, 16 << 10)),
        }
    }
}

impl Read for Layer {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.decoder.is_none() {
            let Some(inner) = self.pending.as_mut() else {
                return Ok(0);
            };
            let first = match inner.fill_buf()?.first() {
                Some(b) => *b,
                None => return Ok(0),
            };
            self.decoder = Some(self.start(first));
        }
        self.decoder.as_mut().expect("ICE: decoder").read(out)
    }
}

/// What a pull from the decoder gave.
pub(super) enum Pull {
    /// Decoded bytes (at most [`CHUNK`]).
    Data(Vec<u8>),
    /// Nothing until more input is pushed (or the input ends).
    NeedInput,
    /// The decoded body is complete.
    End,
}

/// A body decoder for one `content-encoding` header.
pub(super) enum Decoder {
    Identity,
    Chain {
        input: Input,
        out: Chain,
        /// What `out` decodes into; each chunk is copied out at its own size.
        scratch: Box<[u8]>,
    },
    /// More codings than [`MAX_CODINGS`]: reading the body fails.
    TooMany,
}

fn failed(e: &dyn std::fmt::Display) -> VeltErr {
    VeltErr::new(
        code::INVALID_DATA,
        &format!("fetch failed: invalid compressed body: {e}"),
    )
}

impl Decoder {
    /// The decoder for a `content-encoding` header value (`None`: none). A list naming a coding
    /// `fetch` does not decode leaves the body as it is, as in Node.
    pub fn for_encoding(encoding: Option<&str>) -> Decoder {
        let Some(list) = encoding else {
            return Decoder::Identity;
        };
        let codings: Vec<String> = list
            .split(',')
            .map(|c| c.trim().to_ascii_lowercase())
            .filter(|c| !c.is_empty() && c != "identity")
            .collect();
        if codings.is_empty() {
            return Decoder::Identity;
        }
        if codings.len() > MAX_CODINGS {
            return Decoder::TooMany;
        }
        if !codings
            .iter()
            .all(|c| matches!(c.as_str(), "gzip" | "x-gzip" | "deflate" | "br"))
        {
            return Decoder::Identity;
        }
        let input = Input::default();
        let mut out: Chain = Box::new(input.clone());
        // Listed in the order they were applied: undo the last one first.
        for coding in codings.iter().rev() {
            let coding = match coding.as_str() {
                "gzip" | "x-gzip" => Coding::Gzip,
                "deflate" => Coding::Deflate,
                _ => Coding::Brotli,
            };
            out = Box::new(Layer::new(coding, out));
        }
        Decoder::Chain {
            input,
            out,
            scratch: vec![0; CHUNK].into_boxed_slice(),
        }
    }

    pub fn is_identity(&self) -> bool {
        matches!(self, Decoder::Identity)
    }

    /// Queue `data`, the next received frame.
    pub fn push(&mut self, data: Bytes) {
        if let Decoder::Chain { input, .. } = self {
            input.feed().frames.push_back(data);
        }
    }

    /// The body is complete: no more input will be pushed.
    pub fn end_input(&mut self) {
        if let Decoder::Chain { input, .. } = self {
            input.feed().eof = true;
        }
    }

    /// Decode what the queued input allows, at most [`CHUNK`] bytes.
    pub fn pull(&mut self) -> Result<Pull, VeltErr> {
        let (input, out, scratch) = match self {
            Decoder::Chain {
                input,
                out,
                scratch,
            } => (input, out, scratch),
            Decoder::Identity => return Ok(Pull::End),
            Decoder::TooMany => {
                return Err(failed(&format_args!(
                    "more than {MAX_CODINGS} content codings"
                )))
            }
        };
        loop {
            match out.read(scratch) {
                Ok(0) => {
                    // A decoder may report its end before the input does (trailing bytes):
                    // a decoded body is complete only with its input.
                    return Ok(if input.feed().eof {
                        Pull::End
                    } else {
                        Pull::NeedInput
                    });
                }
                Ok(n) => return Ok(Pull::Data(scratch[..n].to_vec())),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(Pull::NeedInput),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(failed(&e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
    use flate2::Compression;
    use std::io::Write;

    const TEXT: &[u8] = b"hello hello hello hello, fetch decodes this body frame by frame";

    /// Decode `data` pushed in frames of `frame` bytes; the largest chunk is checked too.
    fn decode(enc: &str, data: &[u8], frame: usize) -> Result<Vec<u8>, VeltErr> {
        let mut d = Decoder::for_encoding(Some(enc));
        if d.is_identity() {
            return Ok(data.to_vec()); // `body::Reader` hands such frames over as they are
        }
        let mut out = vec![];
        let drain = |d: &mut Decoder, out: &mut Vec<u8>| -> Result<bool, VeltErr> {
            loop {
                match d.pull()? {
                    Pull::Data(v) => {
                        assert!(v.len() <= CHUNK);
                        assert_eq!(v.capacity(), v.len(), "a chunk holds only its data");
                        out.extend(v);
                    }
                    Pull::NeedInput => return Ok(false),
                    Pull::End => return Ok(true),
                }
            }
        };
        // As `body::Reader` does: a pull before any input arrived.
        assert!(!drain(&mut d, &mut out)? || data.is_empty());
        for chunk in data.chunks(frame) {
            d.push(Bytes::copy_from_slice(chunk));
            drain(&mut d, &mut out)?;
        }
        d.end_input();
        assert!(drain(&mut d, &mut out)?, "the body ends with its input");
        Ok(out)
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(vec![], Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = ZlibEncoder::new(vec![], Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn gzip_deflate_in_any_framing() {
        let gz = gzip(TEXT);
        let mut r = DeflateEncoder::new(vec![], Compression::default());
        r.write_all(TEXT).unwrap();
        let raw = r.finish().unwrap();
        for frame in [1, 7, 4096] {
            assert_eq!(decode("gzip", &gz, frame).unwrap(), TEXT);
            assert_eq!(decode("X-GZIP", &gz, frame).unwrap(), TEXT);
            assert_eq!(decode("deflate", &zlib(TEXT), frame).unwrap(), TEXT);
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
    fn several_codings_are_undone_last_first() {
        // `deflate, gzip`: deflated, then gzipped.
        let both = gzip(&zlib(TEXT));
        for frame in [1, 9, 4096] {
            assert_eq!(decode("deflate, gzip", &both, frame).unwrap(), TEXT);
        }
        // Node's `zlib.gzipSync(zlib.deflateSync("hello twice"))`.
        let node: &[u8] = &[
            31, 139, 8, 0, 0, 0, 0, 0, 0, 10, 171, 152, 115, 218, 227, 236, 201, 147, 225, 26, 154,
            231, 125, 252, 88, 25, 36, 63, 176, 4, 2, 0, 53, 123, 115, 204, 19, 0, 0, 0,
        ];
        for frame in [1, 40] {
            assert_eq!(
                decode("deflate, gzip", node, frame).unwrap(),
                b"hello twice"
            );
        }
        let thrice = gzip(&gzip(&zlib(TEXT)));
        assert_eq!(decode("deflate,gzip , gzip", &thrice, 5).unwrap(), TEXT);
        // An unknown coding in the list leaves the body as it is.
        assert_eq!(decode("gzip, zstd", b"as sent", 3).unwrap(), b"as sent");
        // More than five fail.
        let six = "gzip, gzip, gzip, gzip, gzip, gzip";
        let e = decode(six, &gzip(TEXT), 64).err().unwrap();
        assert_eq!(e.code, code::INVALID_DATA);
    }

    #[test]
    fn a_huge_expansion_comes_in_bounded_chunks() {
        // 100 MB of zeros: about 100 KB as gzip, about 1 KB as `gzip, gzip`.
        let zeros = vec![0u8; 100 << 20];
        let once = gzip(&zeros);
        let twice = gzip(&once);
        assert!(twice.len() < 2000, "{}", twice.len());
        for (enc, body) in [("gzip", &once), ("gzip, gzip", &twice)] {
            let mut d = Decoder::for_encoding(Some(enc));
            d.push(Bytes::copy_from_slice(body));
            d.end_input();
            let (mut total, mut largest) = (0usize, 0usize);
            loop {
                match d.pull().unwrap() {
                    Pull::Data(v) => {
                        largest = largest.max(v.len());
                        total += v.len();
                    }
                    Pull::NeedInput => panic!("the input is complete"),
                    Pull::End => break,
                }
            }
            assert_eq!(total, zeros.len());
            assert!(largest <= CHUNK, "{enc}: chunk of {largest} bytes");
        }
    }

    #[test]
    fn corrupt_and_truncated_bodies_fail() {
        let e = decode("gzip", b"not gzip at all", 4).err().unwrap();
        assert_eq!(e.code, code::INVALID_DATA);
        let gz = gzip(TEXT);
        let cut = &gz[..gz.len() - 6];
        assert_eq!(
            decode("gzip", cut, 8).err().unwrap().code,
            code::INVALID_DATA
        );
    }

    #[test]
    fn an_empty_body_decodes_to_nothing() {
        // As in Node: no bytes at all, with or without an empty frame, in every coding.
        for enc in [
            "gzip",
            "x-gzip",
            "br",
            "deflate",
            "deflate, gzip",
            "gzip, br",
        ] {
            assert_eq!(decode(enc, b"", 1).unwrap(), b"", "{enc}");
            let mut d = Decoder::for_encoding(Some(enc));
            d.push(Bytes::new());
            assert!(matches!(d.pull().unwrap(), Pull::NeedInput));
            d.end_input();
            assert!(matches!(d.pull().unwrap(), Pull::End), "{enc}");
        }
        // An inner coding whose input is empty: Node's `zlib.gzipSync("")`, gzipped or not.
        let empty_gz = gzip(b"");
        assert_eq!(decode("gzip", &empty_gz, 4).unwrap(), b"");
        assert_eq!(decode("gzip, gzip", &empty_gz, 4).unwrap(), b"");
        assert_eq!(decode("br, gzip", &empty_gz, 4).unwrap(), b"");
    }

    #[test]
    fn an_empty_frame_then_a_zlib_body() {
        let mut d = Decoder::for_encoding(Some("deflate"));
        d.push(Bytes::new());
        assert!(matches!(d.pull().unwrap(), Pull::NeedInput));
        d.push(Bytes::from(zlib(TEXT)));
        d.end_input();
        let mut out = vec![];
        loop {
            match d.pull().unwrap() {
                Pull::Data(v) => out.extend(v),
                Pull::NeedInput => panic!("the input is complete"),
                Pull::End => break,
            }
        }
        assert_eq!(out, TEXT);
    }

    #[test]
    fn identity_and_unknown_encodings_pass_through() {
        assert!(Decoder::for_encoding(None).is_identity());
        assert!(Decoder::for_encoding(Some("zstd")).is_identity());
        assert!(Decoder::for_encoding(Some("identity")).is_identity());
        assert_eq!(accept_encoding(true), "br, gzip, deflate");
        assert_eq!(accept_encoding(false), "gzip, deflate");
    }
}
