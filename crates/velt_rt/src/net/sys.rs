//! TCP sockets of the runtime's host (tokio's), for its Rust glue: the streams of native
//! libraries (native/aio.rs) and the listener `velt dev`'s supervisor hands over (dev/mod.rs).

use std::io;
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

    /// The socket, for `socket2` (its descriptor).
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

    /// The socket, out of the host's reactor (still non-blocking), for the caller to own: a
    /// descriptor, or a `SOCKET` on Windows.
    pub fn into_raw(self) -> io::Result<u64> {
        let l = self.0.into_std()?;
        #[cfg(unix)]
        let raw = std::os::fd::IntoRawFd::into_raw_fd(l) as u64;
        #[cfg(windows)]
        let raw = std::os::windows::io::IntoRawSocket::into_raw_socket(l);
        Ok(raw)
    }
}
