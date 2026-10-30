//! [`WireStream`]: the stream tokio-postgres reads and writes (see the module docs of `wire`).
//!
//! Reads go through a buffer: each server message is routed whole to its owner, the driver
//! (copied into tokio-postgres' read buffer, possibly across several reads) or a batch (kept
//! until its `ReadyForQuery`). Writes from the driver pass straight through; while a group is
//! queued, a driver write is cut short at the end of its current request so the group can go
//! next. Groups are also written from `poll_read` and `poll_flush`, which the connection task
//! polls even when the driver has nothing to send.

use super::outbound::{Outbound, Sent};
use super::Shared;
use bytes::{Bytes, BytesMut};
use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::oneshot;

/// Read buffer size; any message larger than this is streamed through in pieces.
const READ_BUFFER: usize = 16 * 1024;
/// A backend message header: tag and length.
const HEADER: usize = 5;

/// Who receives the server's messages up to the next `ReadyForQuery`.
enum Owner {
    Driver,
    Batch {
        reply: oneshot::Sender<BytesMut>,
        replies: BytesMut,
    },
}

/// The server message being routed.
#[derive(Clone, Copy, Default)]
struct Frame {
    /// Bytes of it not yet routed.
    left: usize,
    /// It goes to the batch at the head of the owner queue.
    to_batch: bool,
    /// It is a `ReadyForQuery`: its owner is done after it.
    ends_request: bool,
}

/// A stream with batch injection (see the module docs).
pub struct WireStream<S> {
    inner: S,
    shared: Arc<Shared>,
    /// Closes the wire when the stream is dropped (disarmed by `into_parts`).
    closer: Closer,
    out: Outbound,
    owners: VecDeque<Owner>,
    /// The server answered the startup (its first `ReadyForQuery`).
    started: bool,
    /// Driver `Sync`s the server will ignore (sent while a `COPY FROM STDIN` started).
    ignored_syncs: u32,
    /// Single-byte replies (to `SSLRequest`) still to come.
    negotiation_replies: u32,
    read_buf: Box<[u8]>,
    read_pos: usize,
    read_end: usize,
    frame: Frame,
    /// A group being written, and how much of it is out.
    injecting: Option<(Bytes, usize)>,
    flush_pending: bool,
}

impl<S> WireStream<S> {
    /// Wrap `inner` (a fresh connection: nothing sent or received yet).
    pub fn new(inner: S, shared: Arc<Shared>) -> WireStream<S> {
        WireStream {
            inner,
            closer: Closer(Some(shared.clone())),
            shared,
            out: Outbound::new(),
            owners: VecDeque::new(),
            started: false,
            ignored_syncs: 0,
            negotiation_replies: 0,
            read_buf: vec![0; READ_BUFFER].into_boxed_slice(),
            read_pos: 0,
            read_end: 0,
            frame: Frame::default(),
            injecting: None,
            flush_pending: false,
        }
    }

    /// The wrapped stream and the shared state, to wrap again after a TLS handshake (only
    /// the negotiation has passed through so far).
    pub fn into_parts(self) -> (S, Arc<Shared>) {
        let WireStream {
            inner,
            shared,
            mut closer,
            ..
        } = self;
        closer.0 = None;
        (inner, shared)
    }

    /// Account for driver bytes the inner stream accepted.
    fn sent(&mut self, bytes: &[u8]) {
        let mut sent = Sent::default();
        self.out.advance(bytes, false, &mut sent);
        let skipped = sent.syncs.min(self.ignored_syncs);
        self.ignored_syncs -= skipped;
        for _ in 0..(sent.syncs - skipped + sent.queries) {
            self.owners.push_back(Owner::Driver);
        }
        self.negotiation_replies += sent.negotiations;
    }

    /// A `ReadyForQuery` was routed: its owner is done.
    fn request_done(&mut self) {
        if !self.started {
            self.started = true;
            self.out.startup_done();
            return;
        }
        if let Some(Owner::Batch { reply, replies }) = self.owners.pop_front() {
            // The batch may have been cancelled; then its replies are dropped.
            let _ = reply.send(replies);
        }
    }

    /// Start routing the next message at `read_pos`; false if its header is incomplete.
    fn next_frame(&mut self) -> bool {
        if self.negotiation_replies > 0 {
            self.negotiation_replies -= 1;
            self.frame = Frame {
                left: 1,
                ..Frame::default()
            };
            return true;
        }
        if self.read_end - self.read_pos < HEADER {
            return false;
        }
        let h = &self.read_buf[self.read_pos..self.read_pos + HEADER];
        let tag = h[0];
        let len = u32::from_be_bytes([h[1], h[2], h[3], h[4]]) as usize;
        // NoticeResponse, NotificationResponse and ParameterStatus can come at any time.
        let asynchronous = matches!(tag, b'N' | b'A' | b'S');
        let to_batch = !asynchronous && matches!(self.owners.front(), Some(Owner::Batch { .. }));
        if tag == b'G' && !to_batch {
            // CopyInResponse: the Sync the driver sent behind its Execute is ignored.
            self.ignored_syncs += 1;
        }
        self.frame = Frame {
            left: len.max(4) + 1,
            to_batch,
            ends_request: tag == b'Z',
        };
        true
    }

    /// Route buffered server bytes: the driver's into `out` (while it has room), batches' to
    /// their owners.
    fn route(&mut self, out: &mut ReadBuf<'_>) {
        while self.read_pos < self.read_end {
            if self.frame.left == 0 && !self.next_frame() {
                break;
            }
            let available = self.read_end - self.read_pos;
            let mut n = self.frame.left.min(available);
            if !self.frame.to_batch {
                n = n.min(out.remaining());
            }
            if n == 0 {
                break;
            }
            let chunk = &self.read_buf[self.read_pos..self.read_pos + n];
            match (self.frame.to_batch, self.owners.front_mut()) {
                (true, Some(Owner::Batch { replies, .. })) => replies.extend_from_slice(chunk),
                _ => out.put_slice(chunk),
            }
            self.read_pos += n;
            self.frame.left -= n;
            if self.frame.left == 0 && self.frame.ends_request {
                self.request_done();
            }
        }
        if self.read_pos == self.read_end {
            self.read_pos = 0;
            self.read_end = 0;
        }
    }

    /// Make room at the end of the read buffer (a header never straddles its end).
    fn compact(&mut self) {
        if self.read_pos > 0 && self.read_buf.len() - self.read_end < READ_BUFFER / 2 {
            self.read_buf.copy_within(self.read_pos..self.read_end, 0);
            self.read_end -= self.read_pos;
            self.read_pos = 0;
        }
    }

    /// Fail every batch still waiting (the connection is gone).
    fn fail_batches(&mut self) {
        self.owners.clear();
        self.shared.close();
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> WireStream<S> {
    /// Write queued groups while the driver is between requests, then flush them.
    fn poll_inject(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        loop {
            if let Some((bytes, at)) = &mut self.injecting {
                while *at < bytes.len() {
                    let n = ready!(Pin::new(&mut self.inner).poll_write(cx, &bytes[*at..]))?;
                    if n == 0 {
                        return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
                    }
                    *at += n;
                }
                self.injecting = None;
                self.flush_pending = true;
            }
            // `ignored_syncs > 0`: a COPY FROM STDIN runs until the driver's next Sync.
            let between_requests =
                self.started && self.ignored_syncs == 0 && self.out.at_request_end();
            if !between_requests || !self.shared.has_queued() {
                break;
            }
            let Some(group) = self.shared.pop() else {
                break;
            };
            if group.reply.is_closed() {
                continue;
            }
            self.owners.push_back(Owner::Batch {
                reply: group.reply,
                replies: BytesMut::new(),
            });
            self.injecting = Some((group.bytes, 0));
        }
        if self.flush_pending {
            ready!(Pin::new(&mut self.inner).poll_flush(cx))?;
            self.flush_pending = false;
        }
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for WireStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.shared.task.register(cx.waker());
        // A group that cannot be written yet waits for the socket; reading goes on.
        if let Poll::Ready(Err(e)) = this.poll_inject(cx) {
            return Poll::Ready(Err(e));
        }
        loop {
            let before = out.filled().len();
            this.route(out);
            if out.filled().len() > before || out.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
            this.compact();
            let mut buf = ReadBuf::new(&mut this.read_buf[this.read_end..]);
            match Pin::new(&mut this.inner).poll_read(cx, &mut buf) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => {
                    this.fail_batches();
                    return Poll::Ready(Err(e));
                }
                Poll::Ready(Ok(())) => {
                    let n = buf.filled().len();
                    if n == 0 {
                        this.fail_batches();
                        return Poll::Ready(Ok(()));
                    }
                    this.read_end += n;
                }
            }
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for WireStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        ready!(this.poll_inject(cx))?;
        let mut limit = buf.len();
        if this.started && this.shared.has_queued() {
            // Stop at the end of the current request so the group goes next.
            let mut probe = this.out;
            limit = probe.advance(buf, true, &mut Sent::default());
        }
        let n = ready!(Pin::new(&mut this.inner).poll_write(cx, &buf[..limit]))?;
        this.sent(&buf[..n]);
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.shared.task.register(cx.waker());
        ready!(this.poll_inject(cx))?;
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Closes the wire when dropped, failing groups that were queued but never sent.
struct Closer(Option<Arc<Shared>>);

impl Drop for Closer {
    fn drop(&mut self) {
        if let Some(shared) = &self.0 {
            shared.close();
        }
    }
}
