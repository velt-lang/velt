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

/// Whether accepts run on every per-core worker, each on its own registration of the listening
/// socket (a duplicated descriptor in that worker's I/O driver), so new connections are accepted
/// in parallel and each stays on the worker that accepted it. Not on Windows: there a worker that
/// lost an accept race on a duplicated socket could stay blocked in `accept` on its thread (#993),
/// so one registration accepts and each connection is handed to its worker ([`Handoff`]).
pub(crate) fn accepts_per_worker() -> bool {
    cfg!(unix) && crate::task::runtime::worker_handles().is_some()
}

/// An accepted connection on its way to the task that serves it, when one registration accepts
/// for every worker: with the per-core scheduler the socket is detached from the accepting
/// worker's I/O driver and registered again (`attach`) by the task that serves it, with its own
/// worker's driver, so its wakes go there.
pub(crate) enum Handoff {
    Here(TcpStream),
    Detached(std::net::TcpStream),
}

impl Handoff {
    pub(crate) fn new(stream: TcpStream) -> io::Result<Handoff> {
        if crate::task::runtime::current_worker().is_none() {
            return Ok(Handoff::Here(stream));
        }
        stream.into_std().map(Handoff::Detached)
    }

    /// The stream, registered with this thread's driver (call it where it will be used).
    pub(crate) fn attach(self) -> io::Result<TcpStream> {
        match self {
            Handoff::Here(s) => Ok(s),
            Handoff::Detached(s) => TcpStream::from_std(s),
        }
    }
}

/// A listening socket: one registration, or ([`accepts_per_worker`]) one per worker with the
/// original registered with none.
pub enum ListenerObj {
    One(TcpListener),
    PerWorker {
        /// The unregistered original: the address, and what the registrations duplicate.
        source: std::net::TcpListener,
        regs: Vec<TcpListener>,
    },
}

/// One registration of `source` per per-core worker (in worker order), each in that worker's
/// I/O driver; empty with the stealing scheduler. All or nothing: an error registers none.
/// Only where [`accepts_per_worker`].
pub(crate) fn per_worker_listeners(source: &std::net::TcpListener) -> io::Result<Vec<TcpListener>> {
    let Some(workers) = crate::task::runtime::worker_handles() else {
        return Ok(Vec::new());
    };
    workers
        .map(|h| {
            let dup: std::net::TcpListener = socket2::SockRef::from(source).try_clone()?.into();
            dup.set_nonblocking(true)?;
            let _in = h.enter();
            TcpListener::from_std(dup)
        })
        .collect()
}

impl ListenerObj {
    fn new(listener: TcpListener) -> io::Result<ListenerObj> {
        if !accepts_per_worker() {
            return Ok(ListenerObj::One(listener));
        }
        let source = listener.into_std()?;
        let regs = per_worker_listeners(&source)?;
        Ok(ListenerObj::PerWorker { source, regs })
    }

    /// The registration to accept on from this thread (outside a worker, the first's: correct,
    /// with a wake across workers).
    fn here(&self) -> &TcpListener {
        match self {
            ListenerObj::One(l) => l,
            ListenerObj::PerWorker { regs, .. } => {
                &regs[crate::task::runtime::current_worker().map_or(0, |i| i.min(regs.len() - 1))]
            }
        }
    }

    fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        match self {
            ListenerObj::One(l) => l.local_addr(),
            ListenerObj::PerWorker { source, .. } => source.local_addr(),
        }
    }
}

/// Opaque listener handle.
pub type ListenerHandle = Key<ListenerObj>;
/// Opaque stream handle.
pub type StreamHandle = Key<StreamObj>;

static LISTENERS: Registry<ListenerObj> = Registry::new();
static STREAMS: Registry<StreamObj> = Registry::new();

/// Owned text of a string argument (copied: async operations outlive the call).
pub(crate) unsafe fn text_arg(s: *const VeltStr) -> String {
    (*s).text_lossy().into_owned()
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
        let r = crate::dev::bind(addr.as_str())
            .await
            .and_then(ListenerObj::new);
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
        new_leaf(async move {
            let accepted = listener.here().accept().await;
            stream_result(accepted.and_then(|(s, _)| match &*listener {
                ListenerObj::PerWorker { .. } => Ok(s),
                // One registration: move the connection to this (the caller's) worker.
                ListenerObj::One(_) => Handoff::new(s)?.attach(),
            }))
        })
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
    loop {
        // The buffer is allocated once there is something to read, so a connection waiting
        // for data holds none (a server's idle connections would otherwise each keep `max`).
        stream.readable().await?;
        let mut buf = Vec::with_capacity(max.clamp(1, 16 << 20) as usize);
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
