//! Receiving a server request's body while the handler runs: all of it (`req.text()`,
//! `req.bytes()`), into one buffer sized from `content-length`, or chunk by chunk (`req.body`).
//!
//! The body is not read before the handler starts, so a handler that never reads it pays
//! nothing for it, and one that streams it (an upload written to a file) never holds it whole.
//! Request bodies are not decoded: a `content-encoding` the client sent is the handler's to
//! handle, as in Node, Deno and Bun.

use crate::result::{code, VeltErr};
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::{Body, Incoming};

/// The largest chunk `req.body` yields (a larger frame is split), as for fetched bodies.
const CHUNK: usize = 64 * 1024;
/// The most a `content-length` header makes us reserve before any data arrived.
const MAX_RESERVE: u64 = 128 << 20;

/// A request body being received.
pub struct ReqBody {
    incoming: Incoming,
    /// What is left of a frame larger than a chunk.
    rest: Bytes,
    /// The body is complete.
    done: bool,
}

/// A body that could not be received (the client went away, a malformed chunked body).
fn failed(e: &hyper::Error) -> VeltErr {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(s) = source {
        if let Some(io) = s.downcast_ref::<std::io::Error>() {
            let code = crate::result::code_of(io);
            return VeltErr::new(code, &format!("request body failed: {io}"));
        }
        source = s.source();
    }
    VeltErr::new(code::OTHER, &format!("request body failed: {e}"))
}

impl ReqBody {
    /// The body of a request; `None` when it has none (a GET, an empty POST).
    pub fn new(incoming: Incoming) -> Option<ReqBody> {
        (!incoming.is_end_stream()).then_some(ReqBody {
            incoming,
            rest: Bytes::new(),
            done: false,
        })
    }

    /// Whether everything has been received.
    pub fn is_done(&self) -> bool {
        self.done && self.rest.is_empty()
    }

    /// The next frame's data, `None` at the end (trailers are skipped).
    async fn frame(&mut self) -> Result<Option<Bytes>, VeltErr> {
        while let Some(frame) = self.incoming.frame().await {
            if let Ok(data) = frame.map_err(|e| failed(&e))?.into_data() {
                return Ok(Some(data));
            }
        }
        Ok(None)
    }

    /// The next chunk (never empty, at most [`CHUNK`] bytes); `None` once the body is complete.
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>, VeltErr> {
        while !self.is_done() {
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
        Ok(None)
    }

    /// The rest of the body in one buffer.
    pub async fn read_all(mut self) -> Result<Vec<u8>, VeltErr> {
        let len = self.incoming.size_hint().exact().unwrap_or(0).min(MAX_RESERVE);
        let mut buf = Vec::with_capacity(self.rest.len().max(len as usize));
        buf.extend_from_slice(&std::mem::take(&mut self.rest));
        while !self.done {
            match self.frame().await? {
                Some(data) => buf.extend_from_slice(&data),
                None => self.done = true,
            }
        }
        Ok(buf)
    }
}
