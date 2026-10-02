//! Streamed response bodies (`Response.stream`, rt_abi_async.md §14.17): a writer that the
//! handler's producer fills while hyper sends the response.
//!
//! `write` appends to a buffer (like the string builder, §12.1) and never waits; `flush` hands
//! the buffered text to the body as one chunk through a bounded channel, so a producer that is
//! faster than its client waits in `flush` (backpressure) instead of piling up memory. A buffer
//! that grows past [`EAGER_BYTES`] is also handed over by `write` when the channel has room.
//! `close` sends the rest and ends the body; `abort` ends it with an error, so the client sees a
//! truncated response. Once the client has gone away (hyper dropped the body), writes and flushes
//! report `false` and discard their data.
//!
//! Writers are Copy structs in Velt, so their handles are registry keys (§3.2); `close` and
//! `abort` release them.

use super::body::{RespBody, StreamBody};
use super::response::RespHandle;
use crate::bytes::VeltBytes;
use crate::registry::{Key, Registry};
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use bytes::{Bytes, BytesMut};
use hyper::header::{HeaderValue, CONTENT_TYPE};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Flushed chunks that may wait for the client before `flush` waits too.
const QUEUED_CHUNKS: usize = 8;

/// A buffer this large is sent by `write` itself when the channel has room, so a producer that
/// rarely flushes still streams.
const EAGER_BYTES: usize = 16 * 1024;

/// The sending side of a streamed body (`VeltRespWriter` in the ABI docs).
pub struct WriterObj {
    pending: parking_lot::Mutex<Pending>,
    /// Held while a chunk is on its way into the channel, so chunks keep the order they were
    /// written in even when copies of the writer flush concurrently.
    sending: tokio::sync::Mutex<()>,
    /// Shared with the body: set before the sender is dropped by `close`.
    finished: Arc<AtomicBool>,
}

/// Text written since the last chunk was sent, and the channel (`None` once the stream ended or
/// the client went away).
struct Pending {
    buf: BytesMut,
    chunks: Option<mpsc::Sender<Bytes>>,
}

/// Opaque writer handle.
pub type WriterHandle = Key<WriterObj>;

static WRITERS: Registry<WriterObj> = Registry::new();

impl WriterObj {
    /// A writer and the body it feeds.
    pub fn new() -> (WriterObj, StreamBody) {
        let (tx, rx) = mpsc::channel(QUEUED_CHUNKS);
        let finished = Arc::new(AtomicBool::new(false));
        let writer = WriterObj {
            pending: parking_lot::Mutex::new(Pending {
                buf: BytesMut::new(),
                chunks: Some(tx),
            }),
            sending: tokio::sync::Mutex::new(()),
            finished: finished.clone(),
        };
        (writer, StreamBody::new(rx, finished))
    }

    /// Buffers `data`; false (discarding it) once the stream ended or the client went away.
    pub fn write(&self, data: &[u8]) -> bool {
        let mut pending = self.pending.lock();
        let Pending { buf, chunks } = &mut *pending;
        match chunks {
            Some(tx) if !tx.is_closed() => {}
            _ => {
                *chunks = None;
                return false;
            }
        }
        buf.extend_from_slice(data);
        if buf.len() >= EAGER_BYTES {
            self.send_if_room(&mut pending);
        }
        true
    }

    /// Hands the buffer to the body without waiting: only when no flush is sending an earlier
    /// chunk (order) and the channel has room.
    fn send_if_room(&self, pending: &mut Pending) {
        let Ok(_order) = self.sending.try_lock() else {
            return;
        };
        let Pending { buf, chunks } = pending;
        if let Some(Ok(permit)) = chunks.as_ref().map(mpsc::Sender::try_reserve) {
            permit.send(buf.split().freeze());
        }
    }

    /// Sends the buffered text as one chunk, waiting while the channel is full. False once the
    /// stream ended or the client went away. Cancel-safe: the text is taken only once there is
    /// room for it (so text written while waiting goes out in the same chunk).
    pub async fn flush(&self) -> bool {
        let _order = self.sending.lock().await;
        let tx = {
            let pending = self.pending.lock();
            match &pending.chunks {
                None => return false,
                Some(tx) if pending.buf.is_empty() => return !tx.is_closed(),
                Some(tx) => tx.clone(),
            }
        };
        let Ok(permit) = tx.reserve().await else {
            self.pending.lock().chunks = None;
            return false;
        };
        let chunk = self.pending.lock().buf.split().freeze();
        if !chunk.is_empty() {
            permit.send(chunk);
        }
        true
    }

    /// Sends the rest and ends the body normally. False if the client went away first.
    pub async fn close(&self) -> bool {
        let delivered = self.flush().await;
        self.finished.store(true, Ordering::Release);
        self.pending.lock().chunks = None;
        delivered
    }

    /// Ends the body with an error (the rest of the buffer is discarded).
    pub fn abort(&self) {
        let mut pending = self.pending.lock();
        pending.chunks = None;
        pending.buf = BytesMut::new();
    }
}

/// `Response.stream(...)`: makes `r`'s body a stream (default `content-type: text/plain;
/// charset=utf-8` unless one is set) and returns its writer. A bodiless status (1xx, 204, 304)
/// keeps its empty body and gets no `content-type`: the writer's body end is dropped at once,
/// so its writes return 0 as if the client had gone away.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_stream_open(r: RespHandle) -> WriterHandle {
    let (writer, body) = WriterObj::new();
    super::response::with(r, |r| {
        if super::response::bodiless(r.status()) {
            return;
        }
        *r.body_mut() = RespBody::Stream(body);
        r.headers_mut()
            .entry(CONTENT_TYPE)
            .or_insert(HeaderValue::from_static("text/plain; charset=utf-8"));
    });
    WRITERS.insert(writer)
}

/// `w.write(text)`: buffers a copy of `text`; 0 once the stream ended or the client went away.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_stream_write(
    w: WriterHandle,
    text: *const VeltStr,
) -> u8 {
    WRITERS
        .get(w)
        .is_some_and(|obj| obj.write((*text).as_bytes())) as u8
}

/// `w.writeBytes(data)`: like `velt_rt_http_resp_stream_write` for a `u8[]`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_stream_write_bytes(
    w: WriterHandle,
    data: *const VeltBytes,
) -> u8 {
    WRITERS
        .get(w)
        .is_some_and(|obj| obj.write((*data).as_bytes())) as u8
}

/// `await w.flush()` → `bool`: the buffered text was handed to the body (waits while the client
/// is behind); false once the stream ended or the client went away.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_stream_flush(w: WriterHandle) -> *mut VeltFut {
    let obj = WRITERS.get(w);
    new_leaf(async move {
        match obj {
            Some(obj) => obj.flush().await as u8,
            None => 0u8,
        }
    })
}

/// `await w.close()` → `bool`: sends the rest, ends the body and releases the handle; false if
/// the client went away first or the writer was already closed.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_stream_close(w: WriterHandle) -> *mut VeltFut {
    let obj = WRITERS.remove(w);
    new_leaf(async move {
        match obj {
            Some(obj) => obj.close().await as u8,
            None => 0u8,
        }
    })
}

/// `w.abort()`: ends the body with an error (the client sees a truncated response) and releases
/// the handle. No-op on a closed writer.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_resp_stream_abort(w: WriterHandle) {
    if let Some(obj) = WRITERS.remove(w) {
        obj.abort();
    }
}
