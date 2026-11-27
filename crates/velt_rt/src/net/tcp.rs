//! TCP streams over tokio, which only an OS without the executor's reactor reaches, and the
//! listening socket `velt dev`'s supervisor hands over, which std registers with the reactor.
//!
//! Handles are keys into handle tables (`crate::registry`): an in-flight `read`/`write` future
//! holds its own `Arc`, so generated code may close a handle at any time without dangling futures
//! (the socket is released once the last in-flight operation finishes or is dropped), and using a
//! closed handle through any copy of the Velt struct fails with `EBADF`. One task can read while
//! another writes. The object behind a stream handle is in `stream.rs`.

use super::stream::{StreamObj, Writing};
use super::sys::TcpStream;
use crate::bytes::VeltBytes;
use crate::registry::{closed_error, Key, Registry};
use crate::result::{IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use std::io;

/// Default chunk size for `read()` / `readString()` without an explicit maximum.
pub const DEFAULT_READ: u64 = 64 * 1024;

/// Opaque stream handle.
pub type StreamHandle = Key<StreamObj>;

pub(crate) static STREAMS: Registry<StreamObj> = Registry::new();

/// Owned text of a string argument (copied: async operations outlive the call).
pub(crate) unsafe fn text_arg(s: *const VeltStr) -> String {
    (*s).text_lossy().into_owned()
}

fn stream_result(r: io::Result<TcpStream>) -> IoResult<StreamHandle> {
    let r = r.and_then(|s| s.set_nodelay(true).map(|()| s));
    IoResult::from_io(r, |stream| STREAMS.insert(StreamObj::tcp(stream)))
}

/// `listen("host:port")` under `velt dev` → result slot `IoResult<u64>`: the listening socket the
/// supervisor hands over (a non-blocking descriptor, or a `SOCKET` on Windows), which the caller
/// owns and registers with the executor's reactor (std/net/tcp_sched.velt). Port 0 picks a free
/// port.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_listen(addr: *const VeltStr) -> *mut VeltFut {
    let addr = text_arg(addr);
    new_leaf(async move {
        IoResult::from_io(crate::dev::bind(addr.as_str()).await, |l| {
            #[cfg(unix)]
            let raw = std::os::fd::IntoRawFd::into_raw_fd(l) as u64;
            #[cfg(windows)]
            let raw = std::os::windows::io::IntoRawSocket::into_raw_socket(l);
            raw
        })
    })
}

/// `connect("host:port")` → `IoResult<StreamHandle>` (TCP_NODELAY enabled).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_connect(addr: *const VeltStr) -> *mut VeltFut {
    let addr = text_arg(addr);
    new_leaf(async move { stream_result(TcpStream::connect(addr.as_str()).await) })
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
    // A leaf per kind of stream: a raw socket's pending read stays as small as it was.
    STREAMS.op::<VeltBytes>(s, |obj| {
        if obj.plain().is_some() {
            return new_leaf(async move {
                let r = match obj.plain() {
                    Some(stream) => obj.read_raw(stream, max).await,
                    None => read_any(&obj, max).await,
                };
                IoResult::from_io(r, VeltBytes::from_vec)
            });
        }
        new_leaf(async move { IoResult::from_io(read_any(&obj, max).await, VeltBytes::from_vec) })
    })
}

async fn read_any(obj: &StreamObj, max: u64) -> io::Result<Vec<u8>> {
    obj.read(max).await
}

async fn read_text(obj: &StreamObj, max: u64) -> io::Result<String> {
    loop {
        let chunk = read_any(obj, max).await?;
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

/// `write(data)` (a string, copied; one U+FFFD per lone surrogate) → `IoResult<()>` once
/// everything is written.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_write(s: StreamHandle, data: *const VeltStr) -> *mut VeltFut {
    write_copy(s, (*data).text_lossy().as_bytes())
}

/// `writeBytes(data)` (a `u8[]`, copied) → `IoResult<()>` once everything is written.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_write_bytes(
    s: StreamHandle,
    data: *const VeltBytes,
) -> *mut VeltFut {
    write_copy(s, (*data).as_bytes())
}

pub(super) fn write_copy(s: StreamHandle, data: &[u8]) -> *mut VeltFut {
    STREAMS.op::<()>(s, |obj| {
        // Bytes deferred with `writeLater` go first: these join their queue.
        if obj.later.active() {
            let data = data.to_vec();
            return new_leaf(
                async move { IoResult::from_io(obj.write_queued(&data).await, |()| ()) },
            );
        }
        // Counted from the call, so a write made after this one waits for it.
        let (w, first) = Writing::begin(obj);
        // A raw socket usually takes everything at once: written here, from the caller's
        // buffer, there is nothing to copy and nothing to wait for. Not while an earlier write
        // is still going on: these bytes go after its. In the socket's turn, which the rest
        // keeps, so no other thread's write gets between the two parts.
        let mut rest = data;
        let mut turn = None;
        if let Some((stream, t)) = w.0.plain().filter(|_| first).zip(w.0.try_turn()) {
            while !rest.is_empty() {
                match stream.try_write(rest) {
                    Ok(n) => rest = &rest[n..],
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        return new_leaf(std::future::ready(IoResult::<()>::from_io(
                            Err(e),
                            |()| (),
                        )))
                    }
                }
            }
            if rest.is_empty() {
                return new_leaf(std::future::ready(IoResult::ok(())));
            }
            turn = Some(t);
        }
        let data = rest.to_vec();
        new_leaf(async move { IoResult::from_io(w.0.write_in_turn(&data, turn).await, |()| ()) })
    })
}

/// `tryWrite(data)`: writes what a raw socket takes right now, without waiting, and returns the
/// number of bytes written. 0 when nothing can be written now, also for a closed handle, a
/// stream that is not a raw socket (TLS) or not set up yet, and after an error (the next
/// `write` reports it).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_try_write(s: StreamHandle, data: *const VeltBytes) -> u64 {
    try_write_now(s, (*data).as_bytes())
}

/// What `velt_rt_tcp_try_write` writes of `data`.
pub(super) fn try_write_now(s: StreamHandle, data: &[u8]) -> u64 {
    let Some(obj) = STREAMS.get(s) else { return 0 };
    if obj.later.active() {
        return 0;
    }
    // Nothing while another write goes on (these bytes go after its), in the socket's turn.
    let (w, first) = Writing::begin(obj);
    let Some((stream, _turn)) = w.0.plain().filter(|_| first).zip(w.0.try_turn()) else {
        return 0;
    };
    let mut done = 0;
    while done < data.len() {
        match stream.try_write(&data[done..]) {
            Ok(n) => done += n,
            Err(_) => break,
        }
    }
    done as u64
}

/// Shut down the write half (the peer reads end of stream). `out` receives the status.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_shutdown(s: StreamHandle, out: *mut VeltErr) {
    let Some(obj) = STREAMS.get(s) else {
        return out.write(closed_error());
    };
    let e = match obj.shutdown() {
        Ok(()) => VeltErr::ok(),
        Err(e) => VeltErr::from_io(&e),
    };
    out.write(e);
}

/// `abort()`: fails the operations in flight on the stream, and every later one (a raw socket
/// is shut down in both directions). The handle stays open until `velt_rt_tcp_close`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_abort(s: StreamHandle) {
    if let Some(obj) = STREAMS.get(s) {
        obj.abort();
    }
}

/// Remote address as `ip:port` (empty string if unavailable or closed).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_peer_addr(s: StreamHandle, out: *mut VeltStr) {
    let a = STREAMS
        .get(s)
        .and_then(|o| o.peer_addr())
        .map(|a| a.to_string())
        .unwrap_or_default();
    out.write(VeltStr::from_bytes(a.as_bytes()));
}

/// `close()`: release a stream handle (the socket closes when no operation is in flight; one
/// that waits for input then ends with `EBADF`, a write in flight goes on); closing it again,
/// through any copy, is a no-op.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_close(s: StreamHandle) {
    if let Some(obj) = STREAMS.remove(s) {
        // An operation still in flight holds the stream too: its waits for input end now.
        if std::sync::Arc::strong_count(&obj) > 1 {
            obj.close_reads();
        }
    }
}
