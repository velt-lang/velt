//! Receiving a fetched response's body: all of it (`text()`, `bytes()`), into one buffer sized
//! from `content-length` up front so a large download is copied once, never re-grown; or chunk
//! by chunk (`res.body`), each frame decoded as it arrives (`decode.rs`).

use super::decode::Decoder;
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

/// A body being received, with the decoder its `content-encoding` asks for.
pub(super) struct Reader {
    pub incoming: Incoming,
    pub decoder: Decoder,
    /// The decoder has been finished: the body is complete.
    pub done: bool,
}

impl Reader {
    /// The next frame's data, `None` at the end (trailers are skipped).
    async fn frame(&mut self) -> Result<Option<Bytes>, VeltErr> {
        while let Some(frame) = self.incoming.frame().await {
            if let Ok(data) = frame.map_err(|e| body_failed(&e))?.into_data() {
                return Ok(Some(data));
            }
        }
        Ok(None)
    }

    /// The next decoded chunk, never empty; `None` once the body is complete.
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>, VeltErr> {
        while !self.done {
            let out = match self.frame().await? {
                Some(data) => self.decoder.push(&data)?,
                None => {
                    self.done = true;
                    self.decoder.finish()?
                }
            };
            if !out.is_empty() {
                return Ok(Some(out));
            }
        }
        Ok(None)
    }

    /// Read the rest of the body; `len` is the `content-length`, if any.
    pub async fn read_all(mut self, len: Option<u64>) -> Result<Whole, VeltErr> {
        if !self.decoder.is_identity() {
            let mut buf = Vec::new();
            while let Some(chunk) = self.next().await? {
                buf.extend_from_slice(&chunk);
            }
            return Ok(Whole::Gathered(buf));
        }
        let mut first: Option<Bytes> = None;
        let mut buf: Vec<u8> = Vec::new();
        while let Some(data) = self.frame().await? {
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
}
