//! The body of a server response: either complete up front (a string or bytes) or streamed by
//! a writer that std fills from a `BodyStream` (`stream.rs`).
//!
//! An enum rather than a boxed body keeps the full-body hot path as it was: no allocation, no
//! dynamic dispatch, and hyper still sees the exact length (`Content-Length`). A streamed body
//! reports an unknown length, so hyper sends it with chunked transfer encoding (HTTP/1.1) or as
//! DATA frames (HTTP/2) and never computes a `Content-Length`.

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::{Body, Frame, SizeHint};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

/// A response body as hyper sends it.
pub enum RespBody {
    /// The whole body, known before the response is sent.
    Full(Full<Bytes>),
    /// Chunks from a writer (`stream.rs`), sent as they are flushed.
    Stream(StreamBody),
}

impl RespBody {
    /// A complete body.
    pub fn full(data: Bytes) -> RespBody {
        RespBody::Full(Full::new(data))
    }
}

/// The receiving end of a streamed body.
pub struct StreamBody {
    chunks: mpsc::Receiver<Bytes>,
    /// Set by the writer's `close()` before it lets go of the sender: the stream ended normally.
    /// A stream whose writer went away without it was aborted.
    finished: Arc<AtomicBool>,
}

impl StreamBody {
    /// The body fed by `chunks`; `finished` tells a normal end from an abort.
    pub(super) fn new(chunks: mpsc::Receiver<Bytes>, finished: Arc<AtomicBool>) -> StreamBody {
        StreamBody { chunks, finished }
    }

    fn poll_chunk(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Aborted>>> {
        match self.chunks.poll_recv(cx) {
            Poll::Ready(Some(chunk)) => Poll::Ready(Some(Ok(Frame::data(chunk)))),
            Poll::Ready(None) if self.finished.load(Ordering::Acquire) => Poll::Ready(None),
            // Failing the body makes hyper cut the connection (HTTP/1.1: no final chunk; HTTP/2:
            // RST_STREAM), so the client sees a truncated response instead of a complete one.
            Poll::Ready(None) => Poll::Ready(Some(Err(Aborted))),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// A streamed body whose source failed (`velt_rt_http_resp_stream_abort`).
#[derive(Debug)]
pub struct Aborted;

impl std::fmt::Display for Aborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the response stream was aborted")
    }
}

impl std::error::Error for Aborted {}

impl Body for RespBody {
    type Data = Bytes;
    type Error = Aborted;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Aborted>>> {
        match self.get_mut() {
            RespBody::Full(full) => Pin::new(full)
                .poll_frame(cx)
                .map_err(|never| match never {}),
            RespBody::Stream(stream) => stream.poll_chunk(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            RespBody::Full(full) => full.is_end_stream(),
            RespBody::Stream(_) => false,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            RespBody::Full(full) => full.size_hint(),
            // Unknown length: chunked transfer encoding, no `Content-Length`.
            RespBody::Stream(_) => SizeHint::default(),
        }
    }
}
