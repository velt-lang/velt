//! `std/websocket`: WebSocket connections (RFC 6455), both accepted by the HTTP server (an
//! HTTP/1.1 upgrade, `accept.rs`) and opened as a client (`ws://`, `wss://`, `connect.rs`).
//!
//! A connection handle is a key into a handle table (`crate::registry`): in-flight operations
//! hold their own `Arc`, and once the handle is freed every copy of it fails with `EBADF`. A
//! server-side connection starts before its upgrade has completed (the handler still has to
//! return the `101` response); its first operation waits for the upgrade. Nothing here stores
//! callbacks: messages are pulled with `receive` (§13.5).
//!
//! The codec is our own (`frame.rs`), built for many mostly idle connections:
//! - **Reading** (one `receive` at a time, behind a lock): the read buffer is allocated once data
//!   has arrived and dropped once every byte in it is used, so an idle connection holds none.
//!   Frames are unmasked eight bytes at a time; text is checked as UTF-8.
//! - **Writing**: `send` encodes the frame into the connection's queue when it is called, so
//!   frames go out in call order, also before a server-side upgrade has completed, and returns
//!   once it is queued (as in Bun and Node); a writer task per connection sends everything queued
//!   in one write. So the replies to messages that arrived together, or a broadcast to many
//!   sockets, cost one syscall per batch and socket. Past [`HIGH_WATER`] queued bytes a `send`
//!   completes only once the writer has caught up (backpressure). A failed write is reported by
//!   the next `send`. Once the connection is closing or was dropped, the writer gives the peer
//!   [`CLOSE_FLUSH`] to take what is queued, then shuts the socket down.
//! - Pings are answered automatically; a close frame from the peer is answered and ends the
//!   connection (`receive` → kind 0).

mod accept;
mod connect;
mod frame;

use crate::bytes::VeltBytes;
use crate::registry::{Key, Registry};
use crate::result::{code, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use frame::{Frame, Op};
use hyper::upgrade::OnUpgrade;
use std::io;
use std::sync::{Arc, Mutex as SyncMutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify, OnceCell};

/// Any byte stream a WebSocket can run on (upgraded HTTP connection, TCP, TLS over TCP).
pub trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}

/// Queued bytes past which a `send` completes only once the writer has caught up.
const HIGH_WATER: usize = 1 << 20;
/// How long the writer keeps flushing once the connection is closing or was dropped (a peer that
/// stopped reading must not keep the socket and its queue alive): 30 s, as the `ws` package.
#[cfg(not(test))]
const CLOSE_FLUSH: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(test)]
const CLOSE_FLUSH: std::time::Duration = std::time::Duration::from_millis(300);
/// Largest message accepted (tungstenite's default).
const MAX_MESSAGE: usize = 64 << 20;
/// Bytes read at a time.
const READ_CHUNK: usize = 16 * 1024;

/// Which side of the connection we are: a client masks what it sends, a server requires masks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Server,
    Client,
}

enum ReadHalf {
    Tcp(OwnedReadHalf),
    Other(tokio::io::ReadHalf<Box<dyn Transport>>),
}

enum WriteHalf {
    Tcp(OwnedWriteHalf),
    Other(tokio::io::WriteHalf<Box<dyn Transport>>),
}

impl WriteHalf {
    async fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        match self {
            WriteHalf::Tcp(w) => w.write_all(data).await,
            WriteHalf::Other(w) => {
                w.write_all(data).await?;
                w.flush().await
            }
        }
    }

    async fn shutdown(&mut self) {
        let _ = match self {
            WriteHalf::Tcp(w) => w.shutdown().await,
            WriteHalf::Other(w) => w.shutdown().await,
        };
    }
}

/// Splits a raw TCP connection (no lock between the halves) or any other transport.
fn split(tcp: Option<TcpStream>, other: Option<Box<dyn Transport>>) -> (ReadHalf, WriteHalf) {
    if let Some(tcp) = tcp {
        let (r, w) = tcp.into_split();
        return (ReadHalf::Tcp(r), WriteHalf::Tcp(w));
    }
    let (r, w) = tokio::io::split(other.expect("ICE: a WebSocket needs a transport"));
    (ReadHalf::Other(r), WriteHalf::Other(w))
}

/// The receiving side: buffered bytes, a message being reassembled from fragments.
struct Reader {
    half: ReadHalf,
    buf: Vec<u8>,
    pos: usize,
    /// Opcode and payload of a fragmented message so far, and how much of a text payload is
    /// known to be valid UTF-8 (checked fragment by fragment, so bad text fails early).
    partial: Option<(Op, Vec<u8>, usize)>,
    /// The peer's close frame was seen (or the stream ended): every later `receive` is kind 0.
    done: bool,
}

impl Reader {
    /// Reads more bytes; false at end of stream.
    async fn fill(&mut self) -> io::Result<bool> {
        if self.pos > 0 && self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
        if self.buf.capacity() - self.buf.len() < READ_CHUNK / 2 {
            if self.pos > 0 {
                self.buf.drain(..self.pos);
                self.pos = 0;
            }
            if let ReadHalf::Tcp(r) = &self.half {
                // Allocate only once there is something to read: idle connections hold nothing.
                r.readable().await?;
            }
            self.buf.reserve(READ_CHUNK);
        }
        loop {
            let n = match &mut self.half {
                ReadHalf::Tcp(r) => match r.try_read_buf(&mut self.buf) {
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        r.readable().await?;
                        continue;
                    }
                    other => other?,
                },
                ReadHalf::Other(r) => r.read_buf(&mut self.buf).await?,
            };
            return Ok(n > 0);
        }
    }

    /// Drops the buffer once every byte in it is used.
    fn release_if_drained(&mut self) {
        if self.pos == self.buf.len() {
            self.buf = Vec::new();
            self.pos = 0;
        }
    }
}

/// The sending side's queue, shared by `send` and the writer task.
struct Queue {
    buf: Vec<u8>,
    /// A close frame is queued: nothing may follow it, and the writer shuts down after it.
    closing: bool,
    /// The writer stopped (write error, or done after closing).
    stopped: Option<io::ErrorKind>,
    writer_running: bool,
    /// The connection was dropped: the writer sends what is queued, shuts down and ends.
    dropped: bool,
}

struct Writer {
    role: Role,
    q: SyncMutex<Queue>,
    /// Bytes were queued.
    ready: Notify,
    /// The writer took the queue (it is empty now) or stopped.
    drained: Notify,
    /// The connection started closing or was dropped: a write in progress gets [`CLOSE_FLUSH`].
    ending: Notify,
}

impl Writer {
    /// The queue of a connection that isn't running yet: frames sent before a server-side
    /// upgrade completes wait here, in order, until [`Conn::start`] starts the writer.
    fn new(role: Role) -> Arc<Writer> {
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

    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.q.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Encodes one frame into the queue now (call order is wire order); true if more than
    /// [`HIGH_WATER`] bytes are queued after it (the send then completes in [`Writer::room`]).
    fn enqueue(&self, op: Op, payload: &[u8]) -> io::Result<bool> {
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
    async fn room(&self) -> io::Result<()> {
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
    fn abort(&self, kind: io::ErrorKind) {
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
async fn write_loop(writer: Arc<Writer>, mut half: WriteHalf) {
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

/// An open connection.
struct Conn {
    rx: Mutex<Reader>,
    tx: Arc<Writer>,
}

impl Conn {
    /// Starts a connection over a raw TCP stream or another transport, with the queue `tx` (which
    /// may already hold frames sent before the upgrade completed); `prefix` holds bytes already
    /// read past the handshake.
    fn start(
        tx: Arc<Writer>,
        tcp: Option<TcpStream>,
        other: Option<Box<dyn Transport>>,
        prefix: Vec<u8>,
    ) -> Conn {
        let (r, w) = split(tcp, other);
        crate::task::runtime::handle().spawn(write_loop(tx.clone(), w));
        Conn {
            rx: Mutex::new(Reader {
                half: r,
                buf: prefix,
                pos: 0,
                partial: None,
                done: false,
            }),
            tx,
        }
    }

    /// Queues the close frame (unless one was queued) and waits until it has been written.
    async fn close(&self, code: u16, reason: &str) -> io::Result<()> {
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(close_reason(reason).as_bytes());
        if self.tx.enqueue(Op::Close, &payload).is_err() {
            // Already closing or closed (by us or after the peer's close): fine.
            return Ok(());
        }
        loop {
            let drained = self.tx.drained.notified();
            {
                let q = self.tx.lock();
                if !q.writer_running || q.stopped.is_some() {
                    return Ok(());
                }
            }
            drained.await;
        }
    }

    /// Fails the connection (RFC 6455 §7.1.7): sends a close frame with `code`, after which
    /// every `receive` reports the connection closed; returns the error for this `receive`.
    async fn fail(&self, r: &mut Reader, code: u16, what: &str) -> io::Error {
        r.done = true;
        let _ = self.tx.enqueue(Op::Close, &code.to_be_bytes());
        protocol_error(what)
    }

    /// The next text or binary message, or kind 0 once the peer has closed the connection.
    async fn receive(&self) -> io::Result<VeltWsMessage> {
        let mut r = self.rx.lock().await;
        loop {
            if r.done {
                return Ok(message(0, vec![], vec![]));
            }
            let parsed = {
                let Reader { buf, pos, .. } = &mut *r;
                frame::parse(&mut buf[*pos..], self.tx.role == Role::Server, MAX_MESSAGE)
            };
            let f = match parsed {
                Err(e) => {
                    let code = if e == frame::TOO_LONG { 1009 } else { 1002 };
                    return Err(self.fail(&mut r, code, e).await);
                }
                Ok(None) => {
                    if !r.fill().await? {
                        r.done = true;
                        return Ok(message(0, vec![], vec![]));
                    }
                    continue;
                }
                Ok(Some(f)) => f,
            };
            let Frame {
                fin,
                op,
                start,
                len,
                consumed,
            } = f;
            let at = r.pos + start;
            r.pos += consumed;
            match op {
                Op::Ping => {
                    let payload = r.buf[at..at + len].to_vec();
                    r.release_if_drained();
                    // A pong for a connection that is closing is not needed.
                    let _ = self.tx.enqueue(Op::Pong, &payload);
                }
                Op::Pong => r.release_if_drained(),
                Op::Close => {
                    let payload = r.buf[at..at + len].to_vec();
                    r.done = true;
                    r.release_if_drained();
                    // Answer with the peer's code (RFC 6455 §5.5.1); a malformed close frame is a
                    // protocol error (1002), a reason that isn't UTF-8 is 1007. Fails harmlessly
                    // if we closed first.
                    let reply = match payload.len() {
                        0 => 1000,
                        1 => 1002,
                        _ => {
                            let code = u16::from_be_bytes([payload[0], payload[1]]);
                            if !valid_close_code(code) {
                                1002
                            } else if std::str::from_utf8(&payload[2..]).is_err() {
                                1007
                            } else {
                                code
                            }
                        }
                    };
                    let _ = self.tx.enqueue(Op::Close, &reply.to_be_bytes());
                    return Ok(message(0, vec![], vec![]));
                }
                Op::Text | Op::Binary | Op::Continuation => {
                    let data = r.buf[at..at + len].to_vec();
                    r.release_if_drained();
                    let (op, data, checked) = match (op, r.partial.take()) {
                        (Op::Continuation, Some((first, mut so_far, checked))) => {
                            if so_far.len() + data.len() > MAX_MESSAGE {
                                return Err(self.fail(&mut r, 1009, frame::TOO_LONG).await);
                            }
                            so_far.extend_from_slice(&data);
                            (first, so_far, checked)
                        }
                        (Op::Continuation, None) => {
                            let e = "continuation without a message";
                            return Err(self.fail(&mut r, 1002, e).await);
                        }
                        (_, Some(_)) => {
                            let e = "new message inside a fragmented one";
                            return Err(self.fail(&mut r, 1002, e).await);
                        }
                        (op, None) => (op, data, 0),
                    };
                    let mut checked = checked;
                    if op == Op::Text {
                        // Invalid UTF-8 fails at the fragment that has it (§8.1); a character
                        // split across fragments is merely incomplete until the last one.
                        match std::str::from_utf8(&data[checked..]) {
                            Ok(_) => checked = data.len(),
                            Err(e) if e.error_len().is_none() && !fin => {
                                checked += e.valid_up_to();
                            }
                            Err(_) => {
                                let e = "invalid UTF-8 in a text message";
                                return Err(self.fail(&mut r, 1007, e).await);
                            }
                        }
                    }
                    if !fin {
                        r.partial = Some((op, data, checked));
                        continue;
                    }
                    if op == Op::Text {
                        return Ok(message(1, data, vec![]));
                    }
                    return Ok(message(2, vec![], data));
                }
            }
        }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.tx.lock().dropped = true;
        self.tx.ready.notify_one();
        self.tx.ending.notify_waiters();
    }
}

fn message(kind: u32, text: Vec<u8>, data: Vec<u8>) -> VeltWsMessage {
    VeltWsMessage {
        kind,
        pad: 0,
        text: VeltStr::from_vec(text),
        data: VeltBytes::from_vec(data),
    }
}

/// `reason` cut to the 123 bytes a close frame has room for (125 minus the code), at a
/// character boundary: the reason must stay valid UTF-8 (RFC 6455 §5.5.1).
fn close_reason(reason: &str) -> &str {
    let mut end = reason.len().min(123);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    &reason[..end]
}

/// Close codes a peer may send (RFC 6455 §7.4): the defined ones, and 3000–4999 for libraries
/// and applications. 1005, 1006 and 1015 never go on the wire; the rest is reserved.
fn valid_close_code(code: u16) -> bool {
    matches!(code, 1000..=1003 | 1007..=1014 | 3000..=4999)
}

fn closed_error() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the WebSocket is closed")
}

fn stopped_error(kind: io::ErrorKind) -> io::Error {
    match kind {
        io::ErrorKind::BrokenPipe => closed_error(),
        other => io::Error::new(other, "the WebSocket connection failed"),
    }
}

fn protocol_error(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("WebSocket protocol error: {what}"),
    )
}

/// An open (or, server side, about to open) WebSocket connection.
pub struct WsObj {
    /// Server side: the pending HTTP upgrade, taken by the first operation.
    pending: SyncMutex<Option<OnUpgrade>>,
    conn: OnceCell<Conn>,
    /// The send queue, there from the start so that sends keep call order across the upgrade.
    tx: Arc<Writer>,
}

/// Opaque connection handle.
pub type WsHandle = Key<WsObj>;

pub(crate) static SOCKETS: Registry<WsObj> = Registry::new();

/// `{ u32 kind; u32 pad; VeltStr text; VeltBytes data; }` (56 bytes): kind 0 = closed (no more
/// messages), 1 = text (in `text`), 2 = binary (in `data`); the other field is empty.
#[repr(C)]
pub struct VeltWsMessage {
    /// 0 closed, 1 text, 2 binary.
    pub kind: u32,
    /// Always 0.
    pub pad: u32,
    /// A text message's payload.
    pub text: VeltStr,
    /// A binary message's payload.
    pub data: VeltBytes,
}

impl WsObj {
    fn open(conn: Conn) -> WsObj {
        WsObj {
            pending: SyncMutex::new(None),
            tx: conn.tx.clone(),
            conn: OnceCell::new_with(Some(conn)),
        }
    }

    fn upgrading(on: OnUpgrade) -> WsObj {
        WsObj {
            pending: SyncMutex::new(Some(on)),
            conn: OnceCell::new(),
            tx: Writer::new(Role::Server),
        }
    }

    async fn conn(&self) -> io::Result<&Conn> {
        let r = self
            .conn
            .get_or_try_init(|| accept::finish(&self.pending, self.tx.clone()))
            .await;
        if let Err(e) = &r {
            // Frames queued before the upgrade will never go out.
            self.tx.abort(e.kind());
        }
        r
    }
}

/// A send is encoded into the queue at the call, so frames go out in call order. On an open
/// connection with room in its queue the leaf is already complete; otherwise it completes once
/// the upgrade is done (server side) and the writer has made room.
fn send_leaf(ws: WsHandle, op: Op, payload: &[u8]) -> *mut VeltFut {
    SOCKETS.op::<()>(ws, |obj| {
        let over = match obj.tx.enqueue(op, payload) {
            Ok(over) => over,
            Err(e) => return new_leaf(std::future::ready(IoResult::from_io(Err(e), |()| ()))),
        };
        if !over && obj.conn.get().is_some() {
            return new_leaf(std::future::ready(IoResult::ok(())));
        }
        new_leaf(async move {
            let r = match obj.conn().await {
                Ok(_) if over => obj.tx.room().await,
                Ok(_) => Ok(()),
                Err(e) => Err(e),
            };
            IoResult::from_io(r, |()| ())
        })
    })
}

/// `send(text)` → `IoResult<()>` (text frame; queued, see the module docs).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_ws_send_text(ws: WsHandle, text: *const VeltStr) -> *mut VeltFut {
    send_leaf(ws, Op::Text, (*text).text_lossy().as_bytes())
}

/// `sendBytes(data)` → `IoResult<()>` (binary frame; queued).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_ws_send_binary(
    ws: WsHandle,
    data: *const VeltBytes,
) -> *mut VeltFut {
    send_leaf(ws, Op::Binary, (*data).as_bytes())
}

/// `receive()` → `IoResult<VeltWsMessage>`: the next text or binary message, or kind 0 once the
/// peer has closed the connection.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_ws_receive(ws: WsHandle) -> *mut VeltFut {
    SOCKETS.op::<VeltWsMessage>(ws, |obj| {
        new_leaf(async move {
            let r = match obj.conn().await {
                Ok(c) => c.receive().await,
                Err(e) => Err(e),
            };
            IoResult::from_io(r, |m| m)
        })
    })
}

/// `close(code, reason)` → `IoResult<()>`: sends a close frame and closes our side once it is
/// written; closing an already closed connection (or a freed handle) succeeds.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_ws_close(
    ws: WsHandle,
    code: u32,
    reason: *const VeltStr,
) -> *mut VeltFut {
    // Codes a peer may receive (RFC 6455 §7.4); 1005, 1006, 1015 and reserved ones never go on the
    // wire.
    if code > u16::MAX as u32 || !valid_close_code(code as u16) {
        let e = invalid(format!(
            "invalid WebSocket close code {code} (1000-1003, 1007-1014 or 3000-4999)"
        ));
        return new_leaf(std::future::ready(IoResult::<()>::err(e)));
    }
    let Some(obj) = SOCKETS.get(ws) else {
        return new_leaf(async { IoResult::ok(()) });
    };
    let reason = (*reason).text_lossy().into_owned();
    new_leaf(async move {
        let r = match obj.conn().await {
            Ok(c) => c.close(code as u16, &reason).await,
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
            Err(e) => Err(e),
        };
        IoResult::from_io(r, |()| ())
    })
}

/// Release a connection handle (the connection closes when nothing else uses it); releasing
/// it again, through any copy, is a no-op.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_ws_free(ws: WsHandle) {
    SOCKETS.remove(ws);
}

fn invalid(message: String) -> VeltErr {
    VeltErr::new(code::INVALID_INPUT, &message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connected server/client pair over loopback TCP (no HTTP handshake).
    async fn pair() -> (Conn, Conn) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let accept = tokio::spawn(async move { l.accept().await.unwrap().0 });
        let client = TcpStream::connect(addr).await.unwrap();
        let server = Conn::start(
            Writer::new(Role::Server),
            Some(accept.await.unwrap()),
            None,
            vec![],
        );
        let client = Conn::start(Writer::new(Role::Client), Some(client), None, Vec::new());
        (server, client)
    }

    /// A server connection whose peer is a raw socket that never reads, and that peer.
    async fn unread_pair() -> (Conn, TcpStream) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let accept = tokio::spawn(async move { l.accept().await.unwrap().0 });
        let peer = TcpStream::connect(addr).await.unwrap();
        let server = Conn::start(
            Writer::new(Role::Server),
            Some(accept.await.unwrap()),
            None,
            vec![],
        );
        (server, peer)
    }

    /// What a `send` does: queue now, then wait for room if more than HIGH_WATER is queued.
    async fn send(c: &Conn, op: Op, payload: &[u8]) -> io::Result<()> {
        if c.tx.enqueue(op, payload)? {
            c.tx.room().await
        } else {
            Ok(())
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn sends_keep_call_order_past_the_high_water_mark() {
        rt().block_on(async {
            let (server, client) = pair().await;
            // The first send leaves more than HIGH_WATER queued (it would have waited before
            // queueing); the second must still go out after it.
            let big = vec![b'x'; HIGH_WATER + 1];
            assert!(server.tx.enqueue(Op::Binary, &big).unwrap());
            assert!(server.tx.enqueue(Op::Text, b"after").unwrap());
            let first = client.receive().await.unwrap();
            assert_eq!(
                (first.kind, unsafe { first.data.as_bytes() }.len()),
                (2, big.len())
            );
            assert_eq!(text(&client.receive().await.unwrap()), "after");
            server.tx.room().await.unwrap();
        });
    }

    #[test]
    fn frames_queued_before_the_writer_starts_go_out_in_order() {
        rt().block_on(async {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = l.local_addr().unwrap();
            let accept = tokio::spawn(async move { l.accept().await.unwrap().0 });
            let peer = TcpStream::connect(addr).await.unwrap();
            // A server-side queue before its upgrade completed.
            let tx = Writer::new(Role::Server);
            tx.enqueue(Op::Text, b"a").unwrap();
            tx.enqueue(Op::Text, b"b").unwrap();
            let _server = Conn::start(tx, Some(accept.await.unwrap()), None, vec![]);
            let client = Conn::start(Writer::new(Role::Client), Some(peer), None, vec![]);
            assert_eq!(text(&client.receive().await.unwrap()), "a");
            assert_eq!(text(&client.receive().await.unwrap()), "b");
        });
    }

    #[test]
    fn close_and_drop_are_bounded_when_the_peer_does_not_read() {
        rt().block_on(async {
            let (server, _peer) = unread_pair().await;
            // Far more than the socket buffers take: the writer blocks in a write.
            for _ in 0..64 {
                let _ = server.tx.enqueue(Op::Binary, &vec![0u8; 256 * 1024]);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let t = std::time::Instant::now();
            server.close(1000, "").await.unwrap();
            assert!(
                t.elapsed() < CLOSE_FLUSH * 4,
                "close took {:?}",
                t.elapsed()
            );
            // Dropped with data still queued: the writer gives up and lets the socket go.
            let (server, _peer) = unread_pair().await;
            for _ in 0..64 {
                let _ = server.tx.enqueue(Op::Binary, &vec![0u8; 256 * 1024]);
            }
            let tx = server.tx.clone();
            drop(server);
            let t = std::time::Instant::now();
            while Arc::strong_count(&tx) > 1 {
                assert!(
                    t.elapsed() < CLOSE_FLUSH * 4,
                    "the writer outlived its connection"
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        });
    }

    #[test]
    fn invalid_utf8_fails_at_its_fragment() {
        rt().block_on(async {
            let (server, client) = pair().await;
            // A first fragment with an invalid byte; no final fragment ever follows.
            let mut f = Vec::new();
            frame::encode(&mut f, Op::Text, b"ok \xff", true);
            f[0] = 0x01; // FIN clear, text
            client.tx.lock().buf.extend_from_slice(&f);
            client.tx.ready.notify_one();
            match server.receive().await {
                Err(e) => assert!(e.to_string().contains("UTF-8"), "{e}"),
                Ok(m) => panic!("expected an error, got kind {}", m.kind),
            }
            // A character split across fragments is fine.
            let (server, client) = pair().await;
            let euro = "€".as_bytes(); // 3 bytes
            for (fin, op, part) in [(false, 1u8, &euro[..1]), (true, 0, &euro[1..])] {
                let mut f = Vec::new();
                frame::encode(&mut f, Op::Text, part, true);
                f[0] = if fin { 0x80 } else { 0 } | op;
                client.tx.lock().buf.extend_from_slice(&f);
            }
            client.tx.ready.notify_one();
            assert_eq!(text(&server.receive().await.unwrap()), "€");
        });
    }

    #[test]
    fn invalid_close_codes_are_rejected() {
        for code in [0u32, 999, 1004, 1005, 1006, 1015, 2000, 2999, 5000, 70000] {
            // SAFETY: a null handle and a valid string; the leaf is complete and freed below.
            unsafe {
                let reason = VeltStr::from_bytes(b"");
                let f = velt_rt_ws_close(WsHandle::NULL, code, &reason);
                let out =
                    &*((f as *const u8).add(crate::task::FUT_RESULT_OFFSET) as *const IoResult<()>);
                let ready = ((*f).poll)(f, std::ptr::null_mut());
                assert_eq!(ready, crate::task::READY);
                assert_eq!(out.err.code, code::INVALID_INPUT, "close code {code}");
                ((*f).drop)(f);
            }
        }
        for code in [1000, 1001, 1003, 1007, 1014, 3000, 4999] {
            assert!(valid_close_code(code));
        }
    }

    fn text(m: &VeltWsMessage) -> String {
        // SAFETY: the message owns its text.
        String::from_utf8(unsafe { m.text.as_bytes() }.to_vec()).unwrap()
    }

    #[test]
    fn queued_sends_arrive_in_order_and_close_ends_the_stream() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (server, client) = pair().await;
            for i in 0..1000 {
                send(&server, Op::Text, format!("m{i}").as_bytes())
                    .await
                    .unwrap();
            }
            for i in 0..1000 {
                let m = client.receive().await.unwrap();
                assert_eq!((m.kind, text(&m)), (1, format!("m{i}")));
            }
            // Client to server (masked), then a binary message larger than one read.
            send(&client, Op::Text, "héllo ✓".as_bytes()).await.unwrap();
            let big: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
            send(&client, Op::Binary, &big).await.unwrap();
            let m = server.receive().await.unwrap();
            assert_eq!((m.kind, text(&m).as_str()), (1, "héllo ✓"));
            let m = server.receive().await.unwrap();
            // SAFETY: the message owns its data.
            assert_eq!((m.kind, unsafe { m.data.as_bytes() }), (2, &big[..]));
            server.close(1000, "bye").await.unwrap();
            assert_eq!(client.receive().await.unwrap().kind, 0);
            assert_eq!(client.receive().await.unwrap().kind, 0);
            assert!(send(&server, Op::Text, b"late").await.is_err());
        });
    }

    #[test]
    fn close_reasons_are_cut_at_a_character_boundary() {
        assert_eq!(close_reason("bye"), "bye");
        let long = "é".repeat(100); // 200 bytes: byte 123 is inside a character
        let cut = close_reason(&long);
        assert_eq!(cut.len(), 122);
        assert!(cut.chars().all(|c| c == 'é'));
        assert_eq!(close_reason(&"x".repeat(200)).len(), 123);
    }

    #[test]
    fn pings_are_answered_and_fragments_joined() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (server, client) = pair().await;
            // Raw frames from the client: a fragmented text with a ping in between.
            let mut raw = Vec::new();
            for (fin, op, payload) in [
                (false, 1u8, &b"frag"[..]),
                (true, 9, b"pp"),
                (false, 0, b"men"),
                (true, 0, b"ted"),
            ] {
                let mut f = Vec::new();
                frame::encode(&mut f, Op::Binary, payload, true);
                f[0] = if fin { 0x80 } else { 0 } | op;
                raw.extend_from_slice(&f);
            }
            {
                let mut q = client.tx.lock();
                q.buf.extend_from_slice(&raw);
            }
            client.tx.ready.notify_one();
            let m = server.receive().await.unwrap();
            assert_eq!((m.kind, text(&m).as_str()), (1, "fragmented"));
            // The server answered the ping with a pong the client skips; then it closes.
            server.close(1000, "").await.unwrap();
            assert_eq!(client.receive().await.unwrap().kind, 0);
        });
    }
}
