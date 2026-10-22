//! TCP streams and listeners of the Velt backend, on [`super::Io`].

use super::{alloc, place, release, unplace, Io, INTEREST, READABLE, WRITABLE};
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};

/// A TCP connection on the Velt backend.
pub struct TcpStream(Io<mio::net::TcpStream>);

impl TcpStream {
    /// From a connected standard socket (made non-blocking here).
    pub fn from_std(s: std::net::TcpStream) -> io::Result<TcpStream> {
        s.set_nonblocking(true)?;
        Ok(TcpStream(Io::new(mio::net::TcpStream::from_std(s), false)?))
    }

    /// Connects to `addr` (waits until the connection is established).
    pub async fn connect(addr: SocketAddr) -> io::Result<TcpStream> {
        let s = TcpStream(Io::new(mio::net::TcpStream::connect(addr)?, false)?);
        std::future::poll_fn(|cx| s.0.poll_ready(cx, WRITABLE)).await?;
        if let Some(e) = s.0.sock.take_error()? {
            return Err(e);
        }
        s.0.sock.peer_addr()?;
        Ok(s)
    }

    pub async fn readable(&self) -> io::Result<()> {
        self.follow();
        std::future::poll_fn(|cx| self.0.poll_ready(cx, READABLE))
            .await
            .map(drop)
    }

    /// Moves the socket's registration to the home worker of the task reading it when that is
    /// another one (a helper took the task over): its events then wake the task where it runs,
    /// not across threads. A task run once by a worker that borrowed it keeps its home, and its
    /// socket stays. Registering again reports the socket's current readiness (edge triggers
    /// fire for what is already there), so nothing that arrived meanwhile is missed.
    #[cfg(unix)]
    fn follow(&self) {
        use std::os::fd::AsRawFd;
        let Some(here) = super::super::worker::running_home() else {
            return;
        };
        let r = &self.0.registered;
        let was = r.place.load(Ordering::Acquire);
        let (from, old) = unplace(was);
        if from == here {
            return;
        }
        let fd = self.0.sock.as_raw_fd();
        let mut src = mio::unix::SourceFd(&fd);
        let token = alloc(here, &r.reg);
        let ws = super::super::worker::workers();
        if ws[here]
            .registry
            .register(&mut src, mio::Token(token), INTEREST)
            .is_err()
        {
            release(here, token);
            return;
        }
        let now = place(here, token);
        if r.place
            .compare_exchange(was, now, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            let _ = ws[here].registry.deregister(&mut src);
            release(here, token);
            return;
        }
        let _ = ws[from].registry.deregister(&mut src);
        release(from, old);
    }

    #[cfg(not(unix))]
    fn follow(&self) {}

    pub async fn writable(&self) -> io::Result<()> {
        std::future::poll_fn(|cx| self.0.poll_ready(cx, WRITABLE))
            .await
            .map(drop)
    }

    /// Reads what is there into `buf`'s spare capacity, without waiting.
    pub fn try_read_buf(&self, buf: &mut Vec<u8>) -> io::Result<usize> {
        self.follow();
        let want = buf.capacity() - buf.len();
        self.0.try_transfer(READABLE, want, |s| {
            let spare = buf.spare_capacity_mut();
            // SAFETY: `read` only writes into the slice (a socket read never reads it), and
            // the length is advanced by what it reports written.
            let dst = unsafe { &mut *(spare as *mut [std::mem::MaybeUninit<u8>] as *mut [u8]) };
            let n = (&*s).read(dst)?;
            super::moved(n);
            // SAFETY: `n` bytes were just written.
            unsafe { buf.set_len(buf.len() + n) };
            Ok(n)
        })
    }

    pub fn try_write(&self, data: &[u8]) -> io::Result<usize> {
        self.0.try_io(WRITABLE, |s| {
            (&*s).write(data).inspect(|&n| super::moved(n))
        })
    }

    pub fn set_nodelay(&self, on: bool) -> io::Result<()> {
        self.0.sock.set_nodelay(on)
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.0.sock.peer_addr()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.sock.local_addr()
    }

    /// The socket, for `socket2` (shutdown).
    pub fn socket(&self) -> &mio::net::TcpStream {
        &self.0.sock
    }
}

impl tokio::io::AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.follow();
        let r = self.0.poll_io(cx, READABLE, |s| {
            // SAFETY: as in `try_read_buf`; `assume_init` + `advance` below cover what was read.
            let dst = unsafe { &mut *(buf.unfilled_mut() as *mut [_] as *mut [u8]) };
            (&*s).read(dst).inspect(|&n| super::moved(n))
        });
        match r {
            Poll::Ready(Ok(n)) => {
                // SAFETY: `n` bytes of the unfilled part were written.
                unsafe { buf.assume_init(n) };
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl tokio::io::AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.0.poll_io(cx, WRITABLE, |s| {
            (&*s).write(data).inspect(|&n| super::moved(n))
        })
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.0.poll_io(cx, WRITABLE, |s| {
            (&*s).write_vectored(bufs).inspect(|&n| super::moved(n))
        })
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(self.0.sock.shutdown(std::net::Shutdown::Write))
    }
}

/// A TCP listener on the Velt backend.
pub struct TcpListener(Io<mio::net::TcpListener>);

impl TcpListener {
    /// From a bound, listening standard socket (made non-blocking here).
    pub fn from_std(l: std::net::TcpListener) -> io::Result<TcpListener> {
        l.set_nonblocking(true)?;
        Ok(TcpListener(Io::new(
            mio::net::TcpListener::from_std(l),
            true,
        )?))
    }

    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let (s, addr) =
            std::future::poll_fn(|cx| self.0.poll_io(cx, READABLE, |l| l.accept())).await?;
        Ok((TcpStream(Io::new(s, false)?), addr))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.sock.local_addr()
    }
}
