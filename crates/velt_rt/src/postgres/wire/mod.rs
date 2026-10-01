//! The connection's wire: a stream wrapper between tokio-postgres and the socket (or the TLS
//! stream) that lets the runtime send its own message groups on the same connection.
//!
//! tokio-postgres ends every request with its own `Sync`, so N pipelined queries cost the
//! server N implicit transactions and N flushes. A batch (`super::batch`) instead sends
//! `Bind`/`Execute` × N and **one** `Sync`, as pgx's `Batch` does; tokio-postgres has no API
//! for that, so the batch's bytes are injected here:
//!
//! - [`WireStream`] passes the driver's traffic through and tracks message boundaries on both
//!   sides ([`outbound::Outbound`] for what the driver writes, a frame reader for what the server
//!   sends). It keeps a queue of *owners*, one per `ReadyForQuery` the server owes: the driver
//!   for each `Sync` / `Query` it wrote, a batch for each injected group.
//! - An injected group is written only between two driver requests, never inside one. The
//!   server's replies up to the group's `ReadyForQuery` go to the batch (never to
//!   tokio-postgres, which does not know the group exists); asynchronous messages (notices,
//!   notifications, parameter changes) always go to the driver.
//! - A `Sync` sent right after a `COPY … FROM STDIN` starts is ignored by the server; the wire
//!   sees the server's `CopyInResponse` and skips the driver's next `Sync`, and injects nothing
//!   until that `Sync` (the end of the copy) is out. Before the `CopyInResponse` arrives, the
//!   connection's copy gate keeps batches away.
//!
//! [`Wire`] is the connection's handle: [`Wire::send`] queues a group and returns the replies.

mod outbound;
mod stream;
#[cfg(test)]
mod tests;

pub use stream::WireStream;

use crate::postgres::error::PgError;
use bytes::{Bytes, BytesMut};
use futures_util::task::AtomicWaker;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::oneshot;

/// The server's replies to one injected group: every message up to and including its
/// `ReadyForQuery`, as received.
pub type Replies = oneshot::Receiver<BytesMut>;

/// A group waiting to be written.
struct Injection {
    bytes: Bytes,
    reply: oneshot::Sender<BytesMut>,
}

#[derive(Default)]
struct Queue {
    pending: VecDeque<Injection>,
    /// The stream is gone: nothing more can be sent.
    closed: bool,
}

/// State shared by the connection's [`WireStream`]s and its [`Wire`] handle.
#[derive(Default)]
pub struct Shared {
    queue: Mutex<Queue>,
    /// Whether `queue.pending` is non-empty (checked on every driver write without locking).
    queued: AtomicBool,
    /// The connection task, woken to write a newly queued group.
    task: AtomicWaker,
}

impl Shared {
    fn has_queued(&self) -> bool {
        self.queued.load(Ordering::Acquire)
    }

    fn pop(&self) -> Option<Injection> {
        let mut q = self.queue.lock();
        let next = q.pending.pop_front();
        self.queued.store(!q.pending.is_empty(), Ordering::Release);
        next
    }

    /// The stream ended: fail queued groups (their receivers see the sender dropped).
    fn close(&self) {
        let mut q = self.queue.lock();
        q.closed = true;
        q.pending.clear();
        self.queued.store(false, Ordering::Release);
    }
}

/// A connection's handle on its wire (see the module docs).
#[derive(Clone, Default)]
pub struct Wire {
    shared: Arc<Shared>,
}

impl Wire {
    /// A wire for a new connection.
    pub fn new() -> Wire {
        Wire::default()
    }

    /// The state the connection's streams share with this handle.
    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    /// Queue `bytes` (complete frontend messages ending with one `Sync`) for the connection;
    /// the replies arrive on the returned receiver (an error if the connection closes first).
    pub fn send(&self, bytes: Bytes) -> Result<Replies, PgError> {
        let (reply, replies) = oneshot::channel();
        {
            let mut q = self.shared.queue.lock();
            if q.closed {
                return Err(closed());
            }
            q.pending.push_back(Injection { bytes, reply });
            self.shared.queued.store(true, Ordering::Release);
        }
        self.shared.task.wake();
        Ok(replies)
    }
}

/// The error of a group whose connection closed before it was answered.
pub fn closed() -> PgError {
    PgError::new("ECONNRESET", "connection closed")
}
