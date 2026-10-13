//! TCP listeners and streams over tokio.
//!
//! Handles are keys into handle tables (`crate::registry`): an in-flight `accept`/`read`/`write`
//! future holds its own `Arc`, so generated code may close a handle at any time without
//! dangling futures (the socket is released once the last in-flight operation finishes or is
//! dropped), and using a closed handle through any copy of the Velt struct fails with `EBADF`.
//! One task can read while another writes. The object behind a stream handle is in `stream.rs`:
//! a raw socket, or a stream set up by its first operation (TLS, an upgraded HTTP connection).

use super::stream::{read_chunk, Ready, StreamObj};
use crate::bytes::VeltBytes;
use crate::registry::{closed_error, Key, Registry};
use crate::result::{IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use std::io;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// Default chunk size for `read()` / `readString()` without an explicit maximum.
pub const DEFAULT_READ: u64 = 64 * 1024;

/// How long a TLS handshake may take.
const TLS_HANDSHAKE: Duration = Duration::from_secs(10);

/// A listening socket; with `tls`, accepted connections are TLS streams.
pub struct ListenerObj {
    listener: TcpListener,
    tls: Option<TlsAcceptor>,
}

/// An accepted connection on its way to the task that serves it. With the per-core scheduler a
/// socket is registered with the I/O driver of the worker that accepted it; when its task runs on
/// another worker, the socket is detached here and registered again (`attach`) by that task, with
/// its own worker's driver, so its wakes go there. One registration of a listening socket serves
/// every accept: per-worker duplicates woke every worker for each connection (3–5× slower
/// connection churn on a loaded machine) and, on Windows, left losers of an accept race blocked
/// in `accept` (#993).
pub(crate) enum Handoff {
    Here(TcpStream),
    Detached(std::net::TcpStream),
}

impl Handoff {
    /// `moves`: the stream will be used on another worker (detach it from this one's driver).
    pub(crate) fn new(stream: TcpStream, moves: bool) -> io::Result<Handoff> {
        if !moves {
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

/// Opaque listener handle.
pub type ListenerHandle = Key<ListenerObj>;
/// Opaque stream handle.
pub type StreamHandle = Key<StreamObj>;

static LISTENERS: Registry<ListenerObj> = Registry::new();
pub(crate) static STREAMS: Registry<StreamObj> = Registry::new();

/// Owned text of a string argument (copied: async operations outlive the call).
pub(crate) unsafe fn text_arg(s: *const VeltStr) -> String {
    (*s).text_lossy().into_owned()
}

fn stream_result(r: io::Result<TcpStream>) -> IoResult<StreamHandle> {
    let r = r.and_then(|s| s.set_nodelay(true).map(|()| s));
    IoResult::from_io(r, |stream| STREAMS.insert(StreamObj::tcp(stream)))
}

/// `listen("host:port")` → result slot `IoResult<ListenerHandle>`. Port 0 picks a free port.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_listen(addr: *const VeltStr) -> *mut VeltFut {
    let addr = text_arg(addr);
    new_leaf(async move {
        let r = crate::dev::bind(addr.as_str()).await;
        IoResult::from_io(r, |listener| {
            LISTENERS.insert(ListenerObj {
                listener,
                tls: None,
            })
        })
    })
}

fn invalid<T: Send + 'static>(message: &str) -> *mut VeltFut {
    let e = VeltErr::new(crate::result::code::INVALID_INPUT, message);
    new_leaf(async move { IoResult::<T>::err(e) })
}

/// The ALPN protocols in a comma-separated list ("" = none).
unsafe fn alpn_arg(alpn: *const VeltStr) -> Vec<Vec<u8>> {
    (*alpn)
        .text_lossy()
        .split(',')
        .filter(|p| !p.is_empty())
        .map(|p| p.as_bytes().to_vec())
        .collect()
}

/// `listenTls("host:port", { cert, key, alpn })` → `IoResult<ListenerHandle>`: a listener whose
/// accepted streams are TLS (PEM certificate chain and private key; `alpn` = the protocols to
/// offer, comma-separated, "" for none). Bad PEM fails with `EINVAL`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_listen_tls(
    addr: *const VeltStr,
    cert_pem: *const VeltStr,
    key_pem: *const VeltStr,
    alpn: *const VeltStr,
) -> *mut VeltFut {
    let config = crate::tls::server_config_alpn(
        (*cert_pem).text_lossy().as_bytes(),
        (*key_pem).text_lossy().as_bytes(),
        alpn_arg(alpn),
    );
    let acceptor = match config {
        Ok(c) => TlsAcceptor::from(c),
        Err(msg) => return invalid::<ListenerHandle>(&msg),
    };
    let addr = text_arg(addr);
    new_leaf(async move {
        let r = crate::dev::bind(addr.as_str()).await;
        IoResult::from_io(r, |listener| {
            LISTENERS.insert(ListenerObj {
                listener,
                tls: Some(acceptor),
            })
        })
    })
}

/// `listener.port`: the bound local port (the real one after listening on port 0); 0 on error
/// or once closed.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_listener_port(l: ListenerHandle) -> u32 {
    LISTENERS
        .get(l)
        .and_then(|o| o.listener.local_addr().ok())
        .map_or(0, |a| a.port() as u32)
}

/// Release a listener handle (closing it again is a no-op).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_listener_close(l: ListenerHandle) {
    LISTENERS.remove(l);
}

/// `accept()` → `IoResult<StreamHandle>` (TCP_NODELAY enabled). On a TLS listener the handshake
/// is the stream's setup (its first operation runs it, and reports its failure), so a slow or
/// silent client does not hold up the next `accept`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_accept(l: ListenerHandle) -> *mut VeltFut {
    LISTENERS.op::<StreamHandle>(l, |obj| {
        new_leaf(async move {
            let accepted = obj.listener.accept().await;
            let Some(acceptor) = obj.tls.clone() else {
                return stream_result(accepted.map(|(s, _)| s));
            };
            let r = accepted.and_then(|(s, peer)| s.set_nodelay(true).map(|()| (s, peer)));
            IoResult::from_io(r, |(stream, peer)| {
                STREAMS.insert(StreamObj::lazy(Box::pin(async move {
                    let tls = tokio::time::timeout(TLS_HANDSHAKE, acceptor.accept(stream))
                        .await
                        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake"))??;
                    Ok(Ready::other(Box::new(tls), Some(peer)))
                })))
            })
        })
    })
}

/// `connect("host:port")` → `IoResult<StreamHandle>` (TCP_NODELAY enabled).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_connect(addr: *const VeltStr) -> *mut VeltFut {
    let addr = text_arg(addr);
    new_leaf(async move { stream_result(TcpStream::connect(addr.as_str()).await) })
}

/// The host of a "host:port" address (without the brackets of an IPv6 literal).
fn host_of(addr: &str) -> &str {
    let host = addr.rsplit_once(':').map_or(addr, |(h, _)| h);
    host.trim_start_matches('[').trim_end_matches(']')
}

/// `connectTls("host:port", { serverName, ca, alpn })` → `IoResult<StreamHandle>` once the TLS
/// handshake is done. `server_name` "" = the host of `addr`; `ca_pem` = extra trusted PEM CA
/// certificates ("" = the built-in roots only); `alpn` as for `velt_rt_tcp_listen_tls`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_connect_tls(
    addr: *const VeltStr,
    server_name: *const VeltStr,
    ca_pem: *const VeltStr,
    alpn: *const VeltStr,
) -> *mut VeltFut {
    let addr = text_arg(addr);
    let name = match text_arg(server_name) {
        n if n.is_empty() => host_of(&addr).to_string(),
        n => n,
    };
    let config = crate::tls::client_config_alpn((*ca_pem).text_lossy().as_bytes(), alpn_arg(alpn));
    let config = match config {
        Ok(c) => c,
        Err(msg) => return invalid::<StreamHandle>(&msg),
    };
    let Ok(name) = rustls::pki_types::ServerName::try_from(name) else {
        return invalid::<StreamHandle>("invalid TLS server name");
    };
    new_leaf(async move {
        let r = async {
            let tcp = TcpStream::connect(addr.as_str()).await?;
            tcp.set_nodelay(true)?;
            let peer = tcp.peer_addr().ok();
            let connect = TlsConnector::from(config).connect(name, tcp);
            let tls = tokio::time::timeout(TLS_HANDSHAKE, connect)
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake"))??;
            Ok(Ready::other(Box::new(tls), peer))
        }
        .await;
        IoResult::from_io(r, |ready| STREAMS.insert(StreamObj::ready(ready)))
    })
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
                    Some(stream) => read_chunk(stream, max).await,
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

async fn write_any(obj: &StreamObj, data: &[u8]) -> io::Result<()> {
    obj.write(data).await
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

fn write_copy(s: StreamHandle, data: &[u8]) -> *mut VeltFut {
    STREAMS.op::<()>(s, |obj| {
        // A raw socket usually takes everything at once: written here, from the caller's
        // buffer, there is nothing to copy and nothing to wait for.
        let mut rest = data;
        if let Some(stream) = obj.plain() {
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
        }
        let data = rest.to_vec();
        new_leaf(async move { IoResult::from_io(write_any(&obj, &data).await, |()| ()) })
    })
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

/// `close()`: release a stream handle (the socket closes when no operation is in flight);
/// closing it again, through any copy, is a no-op.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_tcp_close(s: StreamHandle) {
    STREAMS.remove(s);
}
