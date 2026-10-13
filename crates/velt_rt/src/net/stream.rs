//! The object behind a `TcpStream` handle: a raw TCP socket, or a byte stream that is still being
//! set up or is not a plain socket (TLS over TCP, an HTTP connection handed over by an upgrade).
//!
//! - **Raw TCP** ([`Io::Tcp`]) goes through `&TcpStream` readiness APIs: no lock between a read
//!   and a write, and a read allocates its buffer once there is something to read, so a
//!   connection waiting for data holds none.
//! - **Everything else** ([`Io::Lazy`]) starts as a future that produces the connection (a TLS
//!   handshake, an HTTP upgrade that completes once the `101` response is sent); the first
//!   operation runs it. It resolves to a raw socket again (a plain-TCP upgrade, with the bytes
//!   the HTTP server read past the request delivered by the first read) or to the two halves of
//!   any other transport, each behind its own lock so one task can read while another writes.
//!
//! `abort` makes operations in flight fail: a write to a peer that stopped reading would
//! otherwise keep the socket alive for ever.

use super::utf8::Utf8Decoder;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{Mutex as AsyncMutex, Notify};

/// Any byte stream besides a raw TCP socket (TLS over TCP, an upgraded HTTP connection).
pub trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}

/// How long a TLS stream's `shutdown` (its close_notify) may take.
const SHUTDOWN: std::time::Duration = std::time::Duration::from_secs(5);

/// The future that sets a lazy stream up.
pub(crate) type Start = Pin<Box<dyn Future<Output = io::Result<Ready>> + Send>>;

/// A lazy stream once it is set up.
pub(crate) enum Ready {
    /// A raw socket, and bytes already read from it that the first reads deliver.
    Tcp {
        stream: TcpStream,
        prefix: Mutex<Vec<u8>>,
        has_prefix: AtomicBool,
    },
    /// Any other transport, split.
    Split {
        r: AsyncMutex<ReadHalf<Box<dyn Transport>>>,
        w: AsyncMutex<WriteHalf<Box<dyn Transport>>>,
        peer: Option<SocketAddr>,
    },
}

impl Ready {
    pub(crate) fn tcp(stream: TcpStream, prefix: Vec<u8>) -> Ready {
        Ready::Tcp {
            has_prefix: AtomicBool::new(!prefix.is_empty()),
            prefix: Mutex::new(prefix),
            stream,
        }
    }

    pub(crate) fn other(io: Box<dyn Transport>, peer: Option<SocketAddr>) -> Ready {
        let (r, w) = tokio::io::split(io);
        Ready::Split {
            r: AsyncMutex::new(r),
            w: AsyncMutex::new(w),
            peer,
        }
    }
}

struct Lazy {
    /// The setup future until it has completed; polled in place, so an operation that is dropped
    /// while it waits for the setup does not lose it.
    start: AsyncMutex<Option<Start>>,
    ready: OnceLock<Result<Ready, (io::ErrorKind, String)>>,
}

enum Io {
    Tcp(TcpStream),
    Lazy(Lazy),
}

/// A stream plus the undecoded tail of a UTF-8 sequence split across `readString`s.
pub struct StreamObj {
    io: Io,
    pub(crate) utf8: Mutex<Utf8Decoder>,
    aborted: AtomicBool,
    abort: Notify,
}

fn aborted_error() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "the stream was aborted")
}

fn buffer(max: u64) -> Vec<u8> {
    Vec::with_capacity(max.clamp(1, 16 << 20) as usize)
}

/// Read what is available (at least 1 byte, at most `max`; empty only at end of stream).
pub(crate) async fn read_chunk(stream: &TcpStream, max: u64) -> io::Result<Vec<u8>> {
    loop {
        // The buffer is allocated once there is something to read, so a connection waiting
        // for data holds none (a server's idle connections would otherwise each keep `max`).
        stream.readable().await?;
        let mut buf = buffer(max);
        match stream.try_read_buf(&mut buf) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
            Ok(_) => return Ok(buf),
        }
    }
}

fn shutdown_socket(stream: &TcpStream, how: std::net::Shutdown) -> io::Result<()> {
    // tokio only offers shutdown through `AsyncWrite` (needs `&mut`); go to the socket directly.
    socket2::SockRef::from(stream).shutdown(how)
}

impl StreamObj {
    fn new(io: Io) -> StreamObj {
        StreamObj {
            io,
            utf8: Mutex::new(Utf8Decoder::default()),
            aborted: AtomicBool::new(false),
            abort: Notify::new(),
        }
    }

    pub(crate) fn tcp(stream: TcpStream) -> StreamObj {
        StreamObj::new(Io::Tcp(stream))
    }

    /// A stream that is set up already.
    pub(crate) fn ready(ready: Ready) -> StreamObj {
        StreamObj::new(Io::Lazy(Lazy {
            start: AsyncMutex::new(None),
            ready: OnceLock::from(Ok(ready)),
        }))
    }

    /// A stream that `start` sets up when its first operation runs.
    pub(crate) fn lazy(start: Start) -> StreamObj {
        StreamObj::new(Io::Lazy(Lazy {
            start: AsyncMutex::new(Some(start)),
            ready: OnceLock::new(),
        }))
    }

    /// `f`, or an error as soon as the stream is aborted.
    async fn until_abort<T>(&self, f: impl Future<Output = io::Result<T>>) -> io::Result<T> {
        let mut abort = std::pin::pin!(self.abort.notified());
        abort.as_mut().enable();
        if self.aborted.load(Ordering::Acquire) {
            return Err(aborted_error());
        }
        match futures_util::future::select(std::pin::pin!(f), abort).await {
            futures_util::future::Either::Left((r, _)) => r,
            futures_util::future::Either::Right(_) => Err(aborted_error()),
        }
    }

    /// The set-up stream, running the setup if this is the first operation.
    async fn set_up<'a>(&self, lazy: &'a Lazy) -> io::Result<&'a Ready> {
        if lazy.ready.get().is_none() {
            let mut start = lazy.start.lock().await;
            if let Some(f) = start.as_mut() {
                let r = f.await.map_err(|e| (e.kind(), e.to_string()));
                *start = None;
                let _ = lazy.ready.set(r);
            }
        }
        match lazy.ready.get() {
            Some(Ok(r)) => Ok(r),
            Some(Err((kind, message))) => Err(io::Error::new(*kind, message.clone())),
            None => Err(aborted_error()),
        }
    }

    /// Reads what is available (at least 1 byte, at most `max`; empty only at end of stream).
    pub(crate) async fn read(&self, max: u64) -> io::Result<Vec<u8>> {
        match &self.io {
            Io::Tcp(stream) => read_chunk(stream, max).await,
            Io::Lazy(lazy) => self.read_lazy(lazy, max).await,
        }
    }

    /// Writes all of `data`.
    pub(crate) async fn write(&self, data: &[u8]) -> io::Result<()> {
        match &self.io {
            Io::Tcp(stream) => self.write_tcp(stream, data).await,
            Io::Lazy(lazy) => self.write_lazy(lazy, data).await,
        }
    }

    /// `read` on a lazy stream.
    async fn read_lazy(&self, lazy: &Lazy, max: u64) -> io::Result<Vec<u8>> {
        match self.until_abort(self.set_up(lazy)).await? {
            Ready::Tcp {
                stream,
                prefix,
                has_prefix,
            } => {
                if has_prefix.load(Ordering::Acquire) {
                    let mut p = prefix.lock().unwrap_or_else(|e| e.into_inner());
                    let n = p.len().min(max.clamp(1, 16 << 20) as usize);
                    let head: Vec<u8> = p.drain(..n).collect();
                    if p.is_empty() {
                        *p = Vec::new();
                        has_prefix.store(false, Ordering::Release);
                    }
                    if !head.is_empty() {
                        return Ok(head);
                    }
                }
                read_chunk(stream, max).await
            }
            Ready::Split { r, .. } => {
                self.until_abort(async {
                    let mut r = r.lock().await;
                    let mut buf = buffer(max);
                    r.read_buf(&mut buf).await?;
                    Ok(buf)
                })
                .await
            }
        }
    }

    /// Writes all of `data` to a raw socket.
    async fn write_tcp(&self, stream: &TcpStream, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            match stream.try_write(data) {
                Ok(n) => data = &data[n..],
                // Only a write that has to wait watches for `abort`.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.until_abort(stream.writable()).await?
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// `write` on a lazy stream.
    async fn write_lazy(&self, lazy: &Lazy, data: &[u8]) -> io::Result<()> {
        match self.until_abort(self.set_up(lazy)).await? {
            Ready::Tcp { stream, .. } => self.write_tcp(stream, data).await,
            Ready::Split { w, .. } => {
                self.until_abort(async {
                    let mut w = w.lock().await;
                    w.write_all(data).await?;
                    w.flush().await
                })
                .await
            }
        }
    }

    /// The raw socket when operations can go straight to it: a plain TCP stream, or a lazy one
    /// that turned out to be a raw socket, once the bytes read ahead of it are delivered. (A
    /// stream that was aborted has its socket shut down: a read sees the end of the stream.)
    pub(crate) fn plain(&self) -> Option<&TcpStream> {
        match &self.io {
            Io::Tcp(s) => Some(s),
            Io::Lazy(l) => match l.ready.get() {
                Some(Ok(Ready::Tcp {
                    stream, has_prefix, ..
                })) if !has_prefix.load(Ordering::Acquire) => Some(stream),
                _ => None,
            },
        }
    }

    /// The raw socket, if there is one yet.
    fn socket(&self) -> Option<&TcpStream> {
        match &self.io {
            Io::Tcp(s) => Some(s),
            Io::Lazy(l) => match l.ready.get() {
                Some(Ok(Ready::Tcp { stream, .. })) => Some(stream),
                _ => None,
            },
        }
    }

    pub(crate) fn peer_addr(&self) -> Option<SocketAddr> {
        if let Some(s) = self.socket() {
            return s.peer_addr().ok();
        }
        match &self.io {
            Io::Lazy(l) => match l.ready.get() {
                Some(Ok(Ready::Split { peer, .. })) => *peer,
                _ => None,
            },
            Io::Tcp(_) => None,
        }
    }

    /// Half-close: the peer reads end of stream. A raw socket is shut down at once; any other
    /// stream in the background, once it is set up (TLS sends its close_notify).
    pub(crate) fn shutdown(self: &std::sync::Arc<Self>) -> io::Result<()> {
        if let Some(s) = self.socket() {
            return shutdown_socket(s, std::net::Shutdown::Write);
        }
        let obj = self.clone();
        crate::task::runtime::handle().spawn(async move {
            let Io::Lazy(lazy) = &obj.io else { return };
            let _ = tokio::time::timeout(SHUTDOWN, async {
                match obj.until_abort(obj.set_up(lazy)).await {
                    Ok(Ready::Tcp { stream, .. }) => {
                        let _ = shutdown_socket(stream, std::net::Shutdown::Write);
                    }
                    Ok(Ready::Split { w, .. }) => {
                        let _ = w.lock().await.shutdown().await;
                    }
                    Err(_) => {}
                }
            })
            .await;
        });
        Ok(())
    }

    /// Fails the operations in flight and every later one.
    pub(crate) fn abort(&self) {
        self.aborted.store(true, Ordering::Release);
        self.abort.notify_waiters();
        if let Some(s) = self.socket() {
            let _ = shutdown_socket(s, std::net::Shutdown::Both);
        }
    }
}
