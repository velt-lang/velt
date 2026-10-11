//! The sending side of a connection: its queue and the writer task that sends it.

use super::frame::{self, Op};
use super::{closed_error, stopped_error, Role, WriteHalf, CLOSE_FLUSH, HIGH_WATER};
use std::io;
use std::sync::{Arc, Mutex as SyncMutex};
use tokio::sync::Notify;

/// The sending side's queue, shared by `send` and the writer task.
pub(super) struct Queue {
    pub(super) buf: Vec<u8>,
    /// A close frame is queued: nothing may follow it, and the writer shuts down after it.
    pub(super) closing: bool,
    /// The writer stopped (write error, or done after closing).
    pub(super) stopped: Option<io::ErrorKind>,
    pub(super) writer_running: bool,
    /// The connection was dropped: the writer sends what is queued, shuts down and ends.
    pub(super) dropped: bool,
}

pub(super) struct Writer {
    pub(super) role: Role,
    pub(super) q: SyncMutex<Queue>,
    /// Bytes were queued.
    pub(super) ready: Notify,
    /// The writer took the queue (it is empty now) or stopped.
    pub(super) drained: Notify,
    /// The connection started closing or was dropped: a write in progress gets [`CLOSE_FLUSH`].
    pub(super) ending: Notify,
}

impl Writer {
    /// The queue of a connection that isn't running yet: frames sent before a server-side
    /// upgrade completes wait here, in order, until [`Conn::start`] starts the writer.
    pub(super) fn new(role: Role) -> Arc<Writer> {
        Arc::new(Writer {
            role,
            q: SyncMutex::new(Queue {
                buf: Vec::new(),
                closing: false,
                stopped: None,
                writer_running: false,
                dropped: false,
            }),
            ready: Notify::new(),
            drained: Notify::new(),
            ending: Notify::new(),
        })
    }

    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.q.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Encodes one frame into the queue now (call order is wire order); true if more than
    /// [`HIGH_WATER`] bytes are queued after it (the send then completes in [`Writer::room`]).
    pub(super) fn enqueue(&self, op: Op, payload: &[u8]) -> io::Result<bool> {
        let over = {
            let mut q = self.lock();
            if let Some(kind) = q.stopped {
                return Err(stopped_error(kind));
            }
            if q.closing {
                return Err(closed_error());
            }
            frame::encode(&mut q.buf, op, payload, self.role == Role::Client);
            if op == Op::Close {
                q.closing = true;
            }
            q.writer_running = true;
            q.buf.len() > HIGH_WATER
        };
        self.ready.notify_one();
        if op == Op::Close {
            self.ending.notify_waiters();
        }
        Ok(over)
    }

    /// Waits until at most [`HIGH_WATER`] bytes are queued; an error if the writer stopped first.
    pub(super) async fn room(&self) -> io::Result<()> {
        loop {
            let mut drained = std::pin::pin!(self.drained.notified());
            drained.as_mut().enable();
            {
                let q = self.lock();
                if let Some(kind) = q.stopped {
                    return Err(stopped_error(kind));
                }
                if q.buf.len() <= HIGH_WATER {
                    return Ok(());
                }
            }
            drained.await;
        }
    }

    /// Stops the writer before it ran (the upgrade failed): queued sends report `kind`.
    pub(super) fn abort(&self, kind: io::ErrorKind) {
        self.lock().stopped.get_or_insert(kind);
        self.drained.notify_waiters();
    }
}

/// Writes `data`; once the connection is closing or was dropped (also while this write is in
/// progress: a peer that stopped reading), at most [`CLOSE_FLUSH`] more.
async fn write_bounded(writer: &Writer, half: &mut WriteHalf, data: &[u8]) -> io::Result<()> {
    let timed_out = || io::Error::new(io::ErrorKind::TimedOut, "the peer stopped reading");
    let mut ending = std::pin::pin!(writer.ending.notified());
    ending.as_mut().enable();
    let already = {
        let q = writer.lock();
        q.closing || q.dropped
    };
    if already {
        return tokio::time::timeout(CLOSE_FLUSH, half.write_all(data))
            .await
            .unwrap_or_else(|_| Err(timed_out()));
    }
    let write = std::pin::pin!(half.write_all(data));
    match futures_util::future::select(write, ending).await {
        futures_util::future::Either::Left((r, _)) => r,
        futures_util::future::Either::Right(((), write)) => {
            tokio::time::timeout(CLOSE_FLUSH, write)
                .await
                .unwrap_or_else(|_| Err(timed_out()))
        }
    }
}

/// What the writer does next.
enum Step {
    /// Wait for bytes to be queued.
    Wait,
    /// The connection is gone and nothing is queued: shut down and end.
    Finish,
    /// Send the batch now in `out`; `closing`: a close frame is in it (or was queued before).
    Write { closing: bool },
}

/// Sends whatever is queued, one write per batch, until the queue is closed and drained.
pub(super) async fn write_loop(writer: Arc<Writer>, mut half: WriteHalf) {
    let mut out = Vec::new();
    loop {
        let step = {
            let mut q = writer.lock();
            if q.buf.is_empty() && !q.closing {
                q.writer_running = false;
                if q.dropped {
                    Step::Finish
                } else {
                    Step::Wait
                }
            } else {
                std::mem::swap(&mut out, &mut q.buf);
                q.writer_running = true;
                Step::Write { closing: q.closing }
            }
        };
        let closing = match step {
            Step::Wait => {
                writer.drained.notify_waiters();
                writer.ready.notified().await;
                continue;
            }
            Step::Finish => {
                half.shutdown().await;
                return;
            }
            Step::Write { closing } => closing,
        };
        // The queue is empty now: sends waiting for room may complete.
        writer.drained.notify_waiters();
        if !out.is_empty() {
            if let Err(e) = write_bounded(&writer, &mut half, &out).await {
                half.shutdown().await;
                {
                    let mut q = writer.lock();
                    q.stopped = Some(e.kind());
                    q.writer_running = false;
                }
                writer.drained.notify_waiters();
                return;
            }
            // Keep a small buffer for the next batch; give large ones back.
            out.clear();
            if out.capacity() > 64 * 1024 {
                out = Vec::new();
            }
        }
        let done = closing && writer.lock().buf.is_empty();
        if done {
            half.shutdown().await;
            {
                let mut q = writer.lock();
                q.stopped = Some(io::ErrorKind::BrokenPipe);
                q.writer_running = false;
            }
            writer.drained.notify_waiters();
            return;
        }
    }
}
