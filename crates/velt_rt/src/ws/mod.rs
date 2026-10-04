//! `std/websocket`: WebSocket connections (RFC 6455) over tokio-tungstenite, both accepted by
//! the HTTP server (an HTTP/1.1 upgrade, `accept.rs`) and opened as a client (`ws://`, `wss://`,
//! `connect.rs`).
//!
//! A connection handle is a key into a handle table (`crate::registry`): in-flight operations
//! hold their own `Arc`, and once the handle is freed every copy of it fails with `EBADF`. The
//! connection's two halves are locked separately, so one task can wait in `receive` while
//! others `send`. A server-side connection starts before its upgrade has
//! completed (the handler still has to return the `101` response); its first operation waits
//! for the upgrade. Pings are answered automatically. Nothing here stores callbacks: messages
//! are pulled with `receive` (§13.5).

mod accept;
mod connect;

use crate::bytes::VeltBytes;
use crate::registry::{Key, Registry};
use crate::result::{code, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use hyper::upgrade::OnUpgrade;
use std::io;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, OnceCell};
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;

/// Any byte stream a WebSocket can run on (upgraded HTTP connection, TCP, TLS over TCP).
pub trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}

type Ws = WebSocketStream<Box<dyn Transport>>;

struct Halves {
    tx: Mutex<SplitSink<Ws, Message>>,
    rx: Mutex<SplitStream<Ws>>,
}

impl Halves {
    fn new(ws: Ws) -> Halves {
        let (tx, rx) = ws.split();
        Halves {
            tx: Mutex::new(tx),
            rx: Mutex::new(rx),
        }
    }
}

/// An open (or, server side, about to open) WebSocket connection.
pub struct WsObj {
    /// Server side: the pending HTTP upgrade, taken by the first operation.
    pending: std::sync::Mutex<Option<OnUpgrade>>,
    conn: OnceCell<Halves>,
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
    fn open(ws: Ws) -> WsObj {
        WsObj {
            pending: std::sync::Mutex::new(None),
            conn: OnceCell::new_with(Some(Halves::new(ws))),
        }
    }

    fn upgrading(on: OnUpgrade) -> WsObj {
        WsObj {
            pending: std::sync::Mutex::new(Some(on)),
            conn: OnceCell::new(),
        }
    }

    async fn halves(&self) -> io::Result<&Halves> {
        self.conn
            .get_or_try_init(|| accept::finish(&self.pending))
            .await
    }
}

fn ws_error(e: WsError) -> io::Error {
    match e {
        WsError::Io(e) => e,
        WsError::ConnectionClosed
        | WsError::AlreadyClosed
        | WsError::Protocol(ProtocolError::SendAfterClosing) => {
            io::Error::new(io::ErrorKind::BrokenPipe, "the WebSocket is closed")
        }
        other => io::Error::other(other.to_string()),
    }
}

async fn send(obj: &WsObj, msg: Message) -> io::Result<()> {
    let halves = obj.halves().await?;
    halves.tx.lock().await.send(msg).await.map_err(ws_error)
}

/// `send(text)` → `IoResult<()>` (text frame; copied).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_ws_send_text(ws: WsHandle, text: *const VeltStr) -> *mut VeltFut {
    let text = (*text).text_lossy().into_owned();
    SOCKETS.op::<()>(ws, |obj| {
        new_leaf(async move { IoResult::from_io(send(&obj, Message::text(text)).await, |()| ()) })
    })
}

/// `sendBytes(data)` → `IoResult<()>` (binary frame; copied).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_ws_send_binary(
    ws: WsHandle,
    data: *const VeltBytes,
) -> *mut VeltFut {
    let data = (*data).as_bytes().to_vec();
    SOCKETS.op::<()>(ws, |obj| {
        new_leaf(async move { IoResult::from_io(send(&obj, Message::binary(data)).await, |()| ()) })
    })
}

async fn receive(obj: &WsObj) -> io::Result<VeltWsMessage> {
    let halves = obj.halves().await?;
    let mut rx = halves.rx.lock().await;
    let message = |kind, text: Vec<u8>, data: Vec<u8>| VeltWsMessage {
        kind,
        pad: 0,
        text: VeltStr::from_vec(text),
        data: VeltBytes::from_vec(data),
    };
    loop {
        match rx.next().await {
            None | Some(Ok(Message::Close(_))) => return Ok(message(0, vec![], vec![])),
            Some(Err(WsError::ConnectionClosed | WsError::AlreadyClosed)) => {
                return Ok(message(0, vec![], vec![]))
            }
            Some(Err(e)) => return Err(ws_error(e)),
            Some(Ok(Message::Text(t))) => return Ok(message(1, t.as_bytes().to_vec(), vec![])),
            Some(Ok(Message::Binary(b))) => return Ok(message(2, vec![], b.to_vec())),
            Some(Ok(_)) => {} // ping/pong/raw frames: answered by tungstenite
        }
    }
}

/// `receive()` → `IoResult<VeltWsMessage>`: the next text or binary message, or kind 0 once the
/// peer has closed the connection.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_ws_receive(ws: WsHandle) -> *mut VeltFut {
    SOCKETS.op::<VeltWsMessage>(ws, |obj| {
        new_leaf(async move { IoResult::from_io(receive(&obj).await, |m| m) })
    })
}

/// `close(code, reason)` → `IoResult<()>`: sends a close frame and closes our side; closing an
/// already closed connection (or a freed handle) succeeds.
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
        let frame = CloseFrame {
            code: CloseCode::from(code as u16),
            reason: reason.into(),
        };
        let r = match send(&obj, Message::Close(Some(frame))).await {
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
            other => other,
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
