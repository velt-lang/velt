//! TCP sockets of the runtime's host (tokio's), for its Rust glue: only what the HTTP server, TLS
//! and the upgrade hand-over use.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A TCP connection.
pub enum TcpStream {
    Tokio(tokio::net::TcpStream),
}

impl TcpStream {
    /// Connects to "host:port" (the first address that answers).
    pub async fn connect(addr: &str) -> io::Result<TcpStream> {
        tokio::net::TcpStream::connect(addr)
            .await
            .map(TcpStream::Tokio)
    }

    /// Connects to one address.
    pub async fn connect_addr(addr: SocketAddr) -> io::Result<TcpStream> {
        tokio::net::TcpStream::connect(addr)
            .await
            .map(TcpStream::Tokio)
    }

    /// From a connected standard socket, registered with this thread's reactor.
    pub fn from_std(s: std::net::TcpStream) -> io::Result<TcpStream> {
        tokio::net::TcpStream::from_std(s).map(TcpStream::Tokio)
    }

    /// Detaches the socket from its reactor (to hand it over).
    pub fn into_std(self) -> io::Result<std::net::TcpStream> {
        match self {
            TcpStream::Tokio(s) => s.into_std(),
        }
    }

    pub async fn readable(&self) -> io::Result<()> {
        match self {
            TcpStream::Tokio(s) => s.readable().await,
        }
    }

    pub async fn writable(&self) -> io::Result<()> {
        match self {
            TcpStream::Tokio(s) => s.writable().await,
        }
    }

    /// Reads what is there into `buf`'s spare capacity (at most `max` bytes), without waiting.
    pub fn try_read_buf(&self, buf: &mut Vec<u8>, max: usize) -> io::Result<usize> {
        match self {
            TcpStream::Tokio(s) => s.try_read_buf(&mut bytes::BufMut::limit(buf, max)),
        }
    }

    pub fn try_read(&self, dst: &mut [u8]) -> io::Result<usize> {
        match self {
            TcpStream::Tokio(s) => s.try_read(dst),
        }
    }

    pub fn try_write(&self, data: &[u8]) -> io::Result<usize> {
        match self {
            TcpStream::Tokio(s) => s.try_write(data),
        }
    }

    pub fn set_nodelay(&self, on: bool) -> io::Result<()> {
        match self {
            TcpStream::Tokio(s) => s.set_nodelay(on),
        }
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        match self {
            TcpStream::Tokio(s) => s.peer_addr(),
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        match self {
            TcpStream::Tokio(s) => s.local_addr(),
        }
    }

    /// The socket, for `socket2` (shutdown).
    /// Whether an idle socket is still open: a look at what has arrived, without taking it and
    /// without waiting (a system call, not the reactor's possibly stale readiness), finds
    /// nothing yet. The end of the stream, unexpected bytes or an error mean it is not.
    pub fn idle_open(&self) -> bool {
        let mut probe = [std::mem::MaybeUninit::<u8>::uninit()];
        matches!(self.sock_ref().peek(&mut probe), Err(e) if e.kind() == io::ErrorKind::WouldBlock)
    }

    pub fn sock_ref(&self) -> socket2::SockRef<'_> {
        match self {
            TcpStream::Tokio(s) => socket2::SockRef::from(s),
        }
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            TcpStream::Tokio(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            TcpStream::Tokio(s) => Pin::new(s).poll_write(cx, data),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            TcpStream::Tokio(s) => Pin::new(s).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            TcpStream::Tokio(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            TcpStream::Tokio(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// A listening TCP socket.
pub enum TcpListener {
    Tokio(tokio::net::TcpListener),
}

impl TcpListener {
    /// From a bound, listening standard socket.
    pub fn from_std(l: std::net::TcpListener) -> io::Result<TcpListener> {
        tokio::net::TcpListener::from_std(l).map(TcpListener::Tokio)
    }

    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        match self {
            TcpListener::Tokio(l) => l.accept().await.map(|(s, a)| (TcpStream::Tokio(s), a)),
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        match self {
            TcpListener::Tokio(l) => l.local_addr(),
        }
    }
}
