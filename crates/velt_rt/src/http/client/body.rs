//! Receiving a fetched response's body: all of it (`text()`, `bytes()`), into one buffer sized
//! from `content-length` up front so a large download is copied once, never re-grown; or chunk
//! by chunk (`res.body`), each frame decoded as it arrives (`decode.rs`).

use super::decode::{Decoder, Pull, CHUNK};
use super::send::body_failed;
use crate::result::VeltErr;
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Body;
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
    /// What is left of a frame larger than a chunk.
    pub rest: Bytes,
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

    /// The next chunk, never empty (a decoded one at most [`CHUNK`] bytes); `None` once
    /// the body is complete.
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>, VeltErr> {
        if self.decoder.is_identity() {
            while !self.done || !self.rest.is_empty() {
                if !self.rest.is_empty() {
                    let n = self.rest.len().min(CHUNK);
                    return Ok(Some(self.rest.split_to(n).to_vec()));
                }
                match self.frame().await? {
                    Some(data) if data.len() > CHUNK => self.rest = data,
                    Some(data) if !data.is_empty() => return Ok(Some(Vec::from(data))),
                    Some(_) => {}
                    None => self.done = true,
                }
            }
            return Ok(None);
        }
        while !self.done {
            match self.decoder.pull()? {
                Pull::Data(chunk) => return Ok(Some(chunk)),
                Pull::End => self.done = true,
                // Empty frames are skipped: `deflate` decides its format on the first byte.
                Pull::NeedInput => match self.frame().await? {
                    Some(data) if !data.is_empty() => self.decoder.push(data),
                    Some(_) => {}
                    None => self.decoder.end_input(),
                },
            }
        }
        Ok(None)
    }

    /// Read the rest of the body.
    pub async fn read_all(mut self) -> Result<Whole, VeltErr> {
        // The `content-length`, if the server sent one (hyper has parsed it).
        let len = self.incoming.size_hint().exact();
        if !self.decoder.is_identity() {
            let mut buf = Vec::new();
            while let Some(chunk) = self.next().await? {
                buf.extend_from_slice(&chunk);
            }
            return Ok(Whole::Gathered(buf));
        }
        let rest = std::mem::take(&mut self.rest);
        let mut first: Option<Bytes> = (!rest.is_empty()).then_some(rest);
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
