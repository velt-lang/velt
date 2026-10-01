//! TCP listeners and streams over tokio.
//!
//! Handles are keys into handle tables (`crate::registry`): an in-flight `accept`/`read`/`write`
//! future holds its own `Arc`, so generated code may close a handle at any time without
//! dangling futures (the socket is released once the last in-flight operation finishes or is
//! dropped), and using a closed handle through any copy of the Velt struct fails with `EBADF`.
//! Reads and writes go through `&TcpStream` readiness APIs, so one task can read while another
//! writes.

use super::utf8::Utf8Decoder;
use crate::bytes::VeltBytes;
use crate::registry::{closed_error, Key, Registry};
use crate::result::{IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use std::io;
use std::sync::Mutex;
use tokio::net::{TcpListener, TcpStream};

/// Default chunk size for `read()` / `readString()` without an explicit maximum.
pub const DEFAULT_READ: u64 = 64 * 1024;

/// A connected stream plus the undecoded tail of a UTF-8 sequence split across `readString`s.
pub struct StreamObj {
    stream: TcpStream,
    utf8: Mutex<Utf8Decoder>,
}

/// Opaque listener handle.
pub type ListenerHandle = Key<TcpListener>;
/// Opaque stream handle.
pub type StreamHandle = Key<StreamObj>;

static LISTENERS: Registry<TcpListener> = Registry::new();
static STREAMS: Registry<StreamObj> = Registry::new();

/// Owned text of a string argument (copied: async operations outlive the call).
pub(crate) unsafe fn text_arg(s: *const VeltStr) -> String {
    String::from_utf8_lossy((*s).as_bytes()).into_owned()
}

fn stream_result(r: io::Result<TcpStream>) -> IoResult<StreamHandle> {
    let r = r.and_then(|s| s.set_nodelay(true).map(|()| s));
    IoResult::from_io(r, |stream| {
        STREAMS.insert(StreamObj {
            stream,
            utf8: Mutex::new(Utf8Decoder::default()),
        })
    })
}

/// `listen("host:port")` → result slot `IoResult<ListenerHandle>`. Port 0 picks a free port.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_listen(addr: *const VeltStr) -> *mut VeltFut {
    let addr = text_arg(addr);
    new_leaf(async move {
        let r = crate::dev::bind(addr.as_str()).await;
        IoResult::from_io(r, |l| LISTENERS.insert(l))
    })
}

/// `listener.port`: the bound local port (the real one after listening on port 0); 0 on error
/// or once closed.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_listener_port(l: ListenerHandle) -> u32 {
    LISTENERS
        .get(l)
        .and_then(|o| o.local_addr().ok())
        .map_or(0, |a| a.port() as u32)
}

/// Release a listener handle (closing it again is a no-op).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_listener_close(l: ListenerHandle) {
    LISTENERS.remove(l);
}

/// `accept()` → `IoResult<StreamHandle>` (TCP_NODELAY enabled).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_accept(l: ListenerHandle) -> *mut VeltFut {
    LISTENERS.op::<StreamHandle>(l, |listener| {
        new_leaf(async move { stream_result(listener.accept().await.map(|(s, _)| s)) })
    })
}

/// `connect("host:port")` → `IoResult<StreamHandle>` (TCP_NODELAY enabled).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_connect(addr: *const VeltStr) -> *mut VeltFut {
    let addr = text_arg(addr);
    new_leaf(async move { stream_result(TcpStream::connect(addr.as_str()).await) })
}

/// Read what is available (at least 1 byte, at most `max`; empty only at end of stream).
async fn read_chunk(stream: &TcpStream, max: u64) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(max.clamp(1, 16 << 20) as usize);
    loop {
        stream.readable().await?;
        match stream.try_read_buf(&mut buf) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
            Ok(_) => return Ok(buf),
        }
    }
}

fn chunk_size(max: u64) -> u64 {
    if max == 0 {
        DEFAULT_READ
    } else {
        max
    }
}

/// `read(max)` → `IoResult<VeltBytes>` with 1..=max bytes; an empty buffer means end of stream.
/// `max == 0` means [`DEFAULT_READ`].
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_read(s: StreamHandle, max: u64) -> *mut VeltFut {
    let max = chunk_size(max);
    STREAMS.op::<VeltBytes>(s, |obj| {
        new_leaf(async move {
            IoResult::from_io(read_chunk(&obj.stream, max).await, VeltBytes::from_vec)
        })
    })
}

async fn read_text(obj: &StreamObj, max: u64) -> io::Result<String> {
    loop {
        let chunk = read_chunk(&obj.stream, max).await?;
        let mut dec = obj.utf8.lock().unwrap_or_else(|e| e.into_inner());
        let text = dec.decode(&chunk, chunk.is_empty());
        // A chunk holding only part of a character decodes to nothing: read on.
        if !text.is_empty() || chunk.is_empty() {
            return Ok(text);
        }
    }
}

/// `readString(max)` → `IoResult<VeltStr>`: the next chunk decoded as UTF-8. A multi-byte
/// character split across chunks is completed on the next call; invalid bytes become U+FFFD.
/// An empty string means end of stream. `max == 0` means [`DEFAULT_READ`].
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_read_string(s: StreamHandle, max: u64) -> *mut VeltFut {
    let max = chunk_size(max);
    STREAMS.op::<VeltStr>(s, |obj| {
        new_leaf(async move {
            IoResult::from_io(read_text(&obj, max).await, |t| {
                VeltStr::from_bytes(t.as_bytes())
            })
        })
    })
}

async fn write_all(stream: &TcpStream, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        stream.writable().await?;
        match stream.try_write(data) {
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// `write(data)` (a string, copied) → `IoResult<()>` once everything is written.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_write(s: StreamHandle, data: *const VeltStr) -> *mut VeltFut {
    write_copy(s, (*data).as_bytes())
}

/// `writeBytes(data)` (a `u8[]`, copied) → `IoResult<()>` once everything is written.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_write_bytes(
    s: StreamHandle,
    data: *const VeltBytes,
) -> *mut VeltFut {
    write_copy(s, (*data).as_bytes())
}

fn write_copy(s: StreamHandle, data: &[u8]) -> *mut VeltFut {
    let data = data.to_vec();
    STREAMS.op::<()>(s, |obj| {
        new_leaf(async move { IoResult::from_io(write_all(&obj.stream, &data).await, |()| ()) })
    })
}

/// Shut down the write half (the peer reads end of stream). `out` receives the status.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_shutdown(s: StreamHandle, out: *mut VeltErr) {
    let Some(obj) = STREAMS.get(s) else {
        return out.write(closed_error());
    };
    // tokio only offers shutdown through `AsyncWrite` (needs `&mut`); go to the socket directly.
    let e = match socket2::SockRef::from(&obj.stream).shutdown(std::net::Shutdown::Write) {
        Ok(()) => VeltErr::ok(),
        Err(e) => VeltErr::from_io(&e),
    };
    out.write(e);
}

/// Remote address as `ip:port` (empty string if unavailable or closed).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_peer_addr(s: StreamHandle, out: *mut VeltStr) {
    let a = STREAMS
        .get(s)
        .and_then(|o| o.stream.peer_addr().ok())
        .map(|a| a.to_string())
        .unwrap_or_default();
    out.write(VeltStr::from_bytes(a.as_bytes()));
}

/// `close()`: release a stream handle (the socket closes when no operation is in flight);
/// closing it again, through any copy, is a no-op.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_close(s: StreamHandle) {
    STREAMS.remove(s);
}
