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
mod writer;

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
use tokio::sync::{Mutex, OnceCell};
use writer::{write_loop, Writer};

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
mod tests;
