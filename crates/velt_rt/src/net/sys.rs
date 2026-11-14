//! TCP sockets of the runtime's host (tokio's), for its Rust glue: only what the HTTP server, TLS
//! and the upgrade hand-over use.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A TCP connection, registered with the host's reactor.
pub struct TcpStream(tokio::net::TcpStream);

impl TcpStream {
    /// Connects to "host:port" (the first address that answers).
    pub async fn connect(addr: &str) -> io::Result<TcpStream> {
        tokio::net::TcpStream::connect(addr).await.map(TcpStream)
    }

    /// From a connected standard socket, registered with the host's reactor (the caller is
    /// inside the host's context).
    pub fn from_std(s: std::net::TcpStream) -> io::Result<TcpStream> {
        tokio::net::TcpStream::from_std(s).map(TcpStream)
    }

    pub async fn readable(&self) -> io::Result<()> {
        self.0.readable().await
    }

    pub async fn writable(&self) -> io::Result<()> {
        self.0.writable().await
    }

    /// Reads what is there into `buf`'s spare capacity (at most `max` bytes), without waiting.
    pub fn try_read_buf(&self, buf: &mut Vec<u8>, max: usize) -> io::Result<usize> {
        self.0.try_read_buf(&mut bytes::BufMut::limit(buf, max))
    }

    pub fn try_read(&self, dst: &mut [u8]) -> io::Result<usize> {
        self.0.try_read(dst)
    }

    pub fn try_write(&self, data: &[u8]) -> io::Result<usize> {
        self.0.try_write(data)
    }

    pub fn set_nodelay(&self, on: bool) -> io::Result<()> {
        self.0.set_nodelay(on)
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.0.peer_addr()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }

    /// Whether an idle socket is still open: a look at what has arrived, without taking it and
    /// without waiting (a system call, not the reactor's possibly stale readiness), finds
    /// nothing yet. The end of the stream, unexpected bytes or an error mean it is not.
    pub fn idle_open(&self) -> bool {
        let mut probe = [std::mem::MaybeUninit::<u8>::uninit()];
        matches!(self.sock_ref().peek(&mut probe), Err(e) if e.kind() == io::ErrorKind::WouldBlock)
    }

    /// The socket, for `socket2` (shutdown).
    pub fn sock_ref(&self) -> socket2::SockRef<'_> {
        socket2::SockRef::from(&self.0)
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().0).poll_write(cx, data)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().0).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
    }
}

/// A listening TCP socket, registered with the host's reactor.
pub struct TcpListener(tokio::net::TcpListener);

impl TcpListener {
    /// From a bound, listening standard socket.
    pub fn from_std(l: std::net::TcpListener) -> io::Result<TcpListener> {
        tokio::net::TcpListener::from_std(l).map(TcpListener)
    }

    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        self.0.accept().await.map(|(s, a)| (TcpStream(s), a))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }
}
