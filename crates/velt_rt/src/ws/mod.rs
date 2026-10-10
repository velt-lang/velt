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
//! - **Writing**: `send` encodes the frame into the connection's queue and returns once it is
//!   queued (as in Bun and Node); a writer task per connection sends everything queued in one
//!   write. So the replies to messages that arrived together, or a broadcast to many sockets,
//!   cost one syscall per batch and socket. Past [`HIGH_WATER`] queued bytes, `send` waits until
//!   the writer has caught up (backpressure). A failed write is reported by the next `send`.
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

/// Queued bytes past which `send` waits for the writer.
const HIGH_WATER: usize = 1 << 20;
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
    /// Opcode and payload of a fragmented message so far.
    partial: Option<(Op, Vec<u8>)>,
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
    q: SyncMutex<Queue>,
    /// Bytes were queued.
    ready: Notify,
    /// The writer drained the queue (or stopped).
    drained: Notify,
}

impl Writer {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.q.lock().unwrap_or_else(|e| e.into_inner())
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
        if !out.is_empty() {
            if let Err(e) = half.write_all(&out).await {
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
    role: Role,
    rx: Mutex<Reader>,
    tx: Arc<Writer>,
}

impl Conn {
    /// Starts a connection over a raw TCP stream or another transport; `prefix` holds bytes
    /// already read past the handshake.
    fn start(
        role: Role,
        tcp: Option<TcpStream>,
        other: Option<Box<dyn Transport>>,
        prefix: Vec<u8>,
    ) -> Conn {
        let (r, w) = split(tcp, other);
        let tx = Arc::new(Writer {
            q: SyncMutex::new(Queue {
                buf: Vec::new(),
                closing: false,
                stopped: None,
                writer_running: true,
                dropped: false,
            }),
            ready: Notify::new(),
            drained: Notify::new(),
        });
        crate::task::runtime::handle().spawn(write_loop(tx.clone(), w));
        Conn {
            role,
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

    /// Queues one frame now if there is room: `Some(result)`, or `None` when more than
    /// [`HIGH_WATER`] bytes are queued (the caller then waits in [`Conn::send`]).
    fn try_send_now(&self, op: Op, payload: &[u8]) -> Option<io::Result<()>> {
        {
            let mut q = self.tx.lock();
            if let Some(kind) = q.stopped {
                return Some(Err(stopped_error(kind)));
            }
            if q.closing {
                return Some(Err(closed_error()));
            }
            if q.buf.len() > HIGH_WATER {
                return None;
            }
            frame::encode(&mut q.buf, op, payload, self.role == Role::Client);
            q.writer_running = true;
        }
        self.tx.ready.notify_one();
        Some(Ok(()))
    }

    /// Queues one frame; waits while more than [`HIGH_WATER`] bytes are queued.
    async fn send(&self, op: Op, payload: &[u8]) -> io::Result<()> {
        loop {
            let wait = {
                let mut q = self.tx.lock();
                if let Some(kind) = q.stopped {
                    return Err(stopped_error(kind));
                }
                if q.closing {
                    return Err(closed_error());
                }
                if q.buf.len() > HIGH_WATER {
                    // Register before unlocking, so the writer's notify cannot be missed.
                    Some(self.tx.drained.notified())
                } else {
                    frame::encode(&mut q.buf, op, payload, self.role == Role::Client);
                    if op == Op::Close {
                        q.closing = true;
                    }
                    q.writer_running = true;
                    None
                }
            };
            match wait {
                Some(drained) => drained.await,
                None => {
                    self.tx.ready.notify_one();
                    return Ok(());
                }
            }
        }
    }

    /// Queues the close frame (unless one was queued) and waits until it has been written.
    async fn close(&self, code: u16, reason: &str) -> io::Result<()> {
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(&reason.as_bytes()[..reason.len().min(123)]);
        match self.send(Op::Close, &payload).await {
            Ok(()) => {}
            // Already closing or closed (by us or after the peer's close): fine.
            Err(_) => return Ok(()),
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

    /// The next text or binary message, or kind 0 once the peer has closed the connection.
    async fn receive(&self) -> io::Result<VeltWsMessage> {
        let mut r = self.rx.lock().await;
        loop {
            if r.done {
                return Ok(message(0, vec![], vec![]));
            }
            let parsed = {
                let Reader { buf, pos, .. } = &mut *r;
                frame::parse(&mut buf[*pos..], self.role == Role::Server, MAX_MESSAGE)
            };
            let f = match parsed {
                Err(e) => return Err(protocol_error(e)),
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
                    let _ = self.send(Op::Pong, &payload).await;
                }
                Op::Pong => r.release_if_drained(),
                Op::Close => {
                    let code = if len >= 2 {
                        [r.buf[at], r.buf[at + 1]]
                    } else {
                        [0x03, 0xe8]
                    };
                    r.done = true;
                    r.release_if_drained();
                    // Answer with the same code (RFC 6455 §5.5.1); fails if we closed first.
                    let _ = self.send(Op::Close, &code).await;
                    return Ok(message(0, vec![], vec![]));
                }
                Op::Text | Op::Binary | Op::Continuation => {
                    let data = r.buf[at..at + len].to_vec();
                    r.release_if_drained();
                    let (op, data) = match (op, r.partial.take()) {
                        (Op::Continuation, Some((first, mut so_far))) => {
                            if so_far.len() + data.len() > MAX_MESSAGE {
                                return Err(protocol_error("message too long"));
                            }
                            so_far.extend_from_slice(&data);
                            (first, so_far)
                        }
                        (Op::Continuation, None) => {
                            return Err(protocol_error("continuation without a message"))
                        }
                        (_, Some(_)) => {
                            return Err(protocol_error("new message inside a fragmented one"))
                        }
                        (op, None) => (op, data),
                    };
                    if !fin {
                        r.partial = Some((op, data));
                        continue;
                    }
                    if op == Op::Text {
                        if std::str::from_utf8(&data).is_err() {
                            let _ = self.send(Op::Close, &1007u16.to_be_bytes()).await;
                            r.done = true;
                            return Err(protocol_error("invalid UTF-8 in a text message"));
                        }
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
            conn: OnceCell::new_with(Some(conn)),
        }
    }

    fn upgrading(on: OnUpgrade) -> WsObj {
        WsObj {
            pending: SyncMutex::new(Some(on)),
            conn: OnceCell::new(),
        }
    }

    async fn conn(&self) -> io::Result<&Conn> {
        self.conn
            .get_or_try_init(|| accept::finish(&self.pending))
            .await
    }
}

/// A send on an open connection with room in its queue happens right away, encoded straight
/// from the caller's bytes (no copy kept, the leaf is already complete); otherwise (the upgrade
/// is still pending, or the queue is full) the bytes are copied and the send waits.
fn send_leaf(ws: WsHandle, op: Op, payload: &[u8]) -> *mut VeltFut {
    SOCKETS.op::<()>(ws, |obj| {
        if let Some(conn) = obj.conn.get() {
            if let Some(r) = conn.try_send_now(op, payload) {
                return new_leaf(std::future::ready(IoResult::from_io(r, |()| ())));
            }
        }
        let payload = payload.to_vec();
        new_leaf(async move {
            let r = match obj.conn().await {
                Ok(c) => c.send(op, &payload).await,
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
        let server = Conn::start(Role::Server, Some(accept.await.unwrap()), None, Vec::new());
        let client = Conn::start(Role::Client, Some(client), None, Vec::new());
        (server, client)
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
                server
                    .send(Op::Text, format!("m{i}").as_bytes())
                    .await
                    .unwrap();
            }
            for i in 0..1000 {
                let m = client.receive().await.unwrap();
                assert_eq!((m.kind, text(&m)), (1, format!("m{i}")));
            }
            // Client to server (masked), then a binary message larger than one read.
            client.send(Op::Text, "héllo ✓".as_bytes()).await.unwrap();
            let big: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
            client.send(Op::Binary, &big).await.unwrap();
            let m = server.receive().await.unwrap();
            assert_eq!((m.kind, text(&m).as_str()), (1, "héllo ✓"));
            let m = server.receive().await.unwrap();
            // SAFETY: the message owns its data.
            assert_eq!((m.kind, unsafe { m.data.as_bytes() }), (2, &big[..]));
            server.close(1000, "bye").await.unwrap();
            assert_eq!(client.receive().await.unwrap().kind, 0);
            assert_eq!(client.receive().await.unwrap().kind, 0);
            assert!(server.send(Op::Text, b"late").await.is_err());
        });
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
