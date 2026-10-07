//! Receiving a fetched response's body (`text()`, `bytes()`): every frame into one buffer, sized
//! from `content-length` up front so a large download is copied once, never re-grown.

use super::send::body_failed;
use crate::result::VeltErr;
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Incoming;

/// The most a `content-length` header makes us reserve before any data arrived (a larger body
/// still arrives; the buffer grows as it does).
const MAX_RESERVE: u64 = 128 << 20;

/// A whole body: a single frame as it came, several gathered into one buffer.
pub(super) enum Whole {
    One(Bytes),
    Gathered(Vec<u8>),
}

impl Whole {
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Whole::One(b) => b,
            Whole::Gathered(v) => v,
        }
    }

    /// The bytes as a `Vec` (a copy only for a single frame shared with hyper).
    pub fn into_vec(self) -> Vec<u8> {
        match self {
            Whole::One(b) => Vec::from(b),
            Whole::Gathered(v) => v,
        }
    }
}

/// Read `body` to its end; `len` is the `content-length`, if any.
pub(super) async fn read_all(mut body: Incoming, len: Option<u64>) -> Result<Whole, VeltErr> {
    let mut first: Option<Bytes> = None;
    let mut buf: Vec<u8> = Vec::new();
    while let Some(frame) = body.frame().await {
        let Ok(data) = frame.map_err(|e| body_failed(&e))?.into_data() else {
            continue; // trailers
        };
        if data.is_empty() {
            continue;
        }
        match first.take() {
            None if buf.is_empty() => first = Some(data),
            prev => {
                if let Some(p) = prev {
                    let want = len.unwrap_or(0).min(MAX_RESERVE) as usize;
                    buf.reserve(want.max(p.len() + data.len()));
                    buf.extend_from_slice(&p);
                }
                buf.extend_from_slice(&data);
            }
        }
    }
    Ok(match first {
        Some(b) => Whole::One(b),
        None => Whole::Gathered(buf),
    })
}
