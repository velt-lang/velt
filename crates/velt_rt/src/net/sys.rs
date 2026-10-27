//! TCP sockets for whichever runtime backend runs: tokio's, or the Velt backend's own
//! ([`crate::task::runtime::velt_io`]). Only what `std/net`, the HTTP server and the upgrade
//! hand-over use; one type each, so a stream keeps its fast paths (`TokioIo<TcpStream>` is the
//! same type on both backends).

use crate::task::runtime::{self, velt_io};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A TCP connection.
pub enum TcpStream {
    Tokio(tokio::net::TcpStream),
    Velt(velt_io::TcpStream),
}

impl TcpStream {
    /// Connects to "host:port" (the first address that answers).
    pub async fn connect(addr: &str) -> io::Result<TcpStream> {
        if !runtime::is_velt() {
            return tokio::net::TcpStream::connect(addr)
                .await
                .map(TcpStream::Tokio);
        }
        let addrs: Vec<SocketAddr> = match addr.parse() {
            Ok(a) => vec![a],
            Err(_) => {
                let owned = addr.to_string();
                let found = runtime::spawn_blocking(move || {
                    std::net::ToSocketAddrs::to_socket_addrs(&owned).map(Iterator::collect)
                });
                found.await.map_err(|e| io::Error::other(e.to_string()))??
            }
        };
        let mut last = io::Error::new(io::ErrorKind::InvalidInput, "no address to connect to");
        for a in addrs {
            match TcpStream::connect_addr(a).await {
                Ok(s) => return Ok(s),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Connects to one address.
    pub async fn connect_addr(addr: SocketAddr) -> io::Result<TcpStream> {
        if runtime::is_velt() {
            velt_io::TcpStream::connect(addr).await.map(TcpStream::Velt)
        } else {
            tokio::net::TcpStream::connect(addr)
                .await
                .map(TcpStream::Tokio)
        }
    }

    /// From a connected standard socket, registered with this thread's reactor.
    pub fn from_std(s: std::net::TcpStream) -> io::Result<TcpStream> {
        if runtime::is_velt() {
            velt_io::TcpStream::from_std(s).map(TcpStream::Velt)
        } else {
            tokio::net::TcpStream::from_std(s).map(TcpStream::Tokio)
        }
    }

    /// Detaches the socket from its reactor (to move it to another per-core worker).
    pub fn into_std(self) -> io::Result<std::net::TcpStream> {
        match self {
            TcpStream::Tokio(s) => s.into_std(),
            TcpStream::Velt(_) => Err(io::Error::other("ICE: hand-off of a Velt-backend socket")),
        }
    }

    pub async fn readable(&self) -> io::Result<()> {
        match self {
            TcpStream::Tokio(s) => s.readable().await,
            TcpStream::Velt(s) => s.readable().await,
        }
    }

    pub async fn writable(&self) -> io::Result<()> {
        match self {
            TcpStream::Tokio(s) => s.writable().await,
            TcpStream::Velt(s) => s.writable().await,
        }
    }

    /// Reads what is there into `buf`'s spare capacity (at most `max` bytes), without waiting.
    pub fn try_read_buf(&self, buf: &mut Vec<u8>, max: usize) -> io::Result<usize> {
        match self {
            TcpStream::Tokio(s) => s.try_read_buf(&mut bytes::BufMut::limit(buf, max)),
            TcpStream::Velt(s) => s.try_read_buf(buf, max),
        }
    }

    pub fn try_read(&self, dst: &mut [u8]) -> io::Result<usize> {
        match self {
            TcpStream::Tokio(s) => s.try_read(dst),
            TcpStream::Velt(s) => s.try_read(dst),
        }
    }

    pub fn try_write(&self, data: &[u8]) -> io::Result<usize> {
        match self {
            TcpStream::Tokio(s) => s.try_write(data),
            TcpStream::Velt(s) => s.try_write(data),
        }
    }

    pub fn set_nodelay(&self, on: bool) -> io::Result<()> {
        match self {
            TcpStream::Tokio(s) => s.set_nodelay(on),
            TcpStream::Velt(s) => s.set_nodelay(on),
        }
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        match self {
            TcpStream::Tokio(s) => s.peer_addr(),
            TcpStream::Velt(s) => s.peer_addr(),
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        match self {
            TcpStream::Tokio(s) => s.local_addr(),
            TcpStream::Velt(s) => s.local_addr(),
        }
    }

    /// The socket, for `socket2` (shutdown).
    pub fn sock_ref(&self) -> socket2::SockRef<'_> {
        match self {
            TcpStream::Tokio(s) => socket2::SockRef::from(s),
            TcpStream::Velt(s) => socket2::SockRef::from(s.socket()),
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
            TcpStream::Velt(s) => Pin::new(s).poll_read(cx, buf),
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
            TcpStream::Velt(s) => Pin::new(s).poll_write(cx, data),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            TcpStream::Tokio(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            TcpStream::Velt(s) => Pin::new(s).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            TcpStream::Tokio(s) => Pin::new(s).poll_flush(cx),
            TcpStream::Velt(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            TcpStream::Tokio(s) => Pin::new(s).poll_shutdown(cx),
            TcpStream::Velt(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// A listening TCP socket.
pub enum TcpListener {
    Tokio(tokio::net::TcpListener),
    Velt(velt_io::TcpListener),
}

impl TcpListener {
    /// From a bound, listening standard socket.
    pub fn from_std(l: std::net::TcpListener) -> io::Result<TcpListener> {
        if runtime::is_velt() {
            velt_io::TcpListener::from_std(l).map(TcpListener::Velt)
        } else {
            tokio::net::TcpListener::from_std(l).map(TcpListener::Tokio)
        }
    }

    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        match self {
            TcpListener::Tokio(l) => l.accept().await.map(|(s, a)| (TcpStream::Tokio(s), a)),
            TcpListener::Velt(l) => l.accept().await.map(|(s, a)| (TcpStream::Velt(s), a)),
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        match self {
            TcpListener::Tokio(l) => l.local_addr(),
            TcpListener::Velt(l) => l.local_addr(),
        }
    }
}
