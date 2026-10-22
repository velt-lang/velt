//! TCP streams on the runtime's own sockets: on Velt's scheduler a stream is registered with the
//! worker's reactor (so a driver in a package does what one in the runtime would), on tokio's
//! with tokio's. With the `tokio` feature, [`TcpStream`] implements tokio's `AsyncRead` and
//! `AsyncWrite`, so libraries written for tokio (database drivers, TLS) run on it unchanged.

use std::io;
use std::task::{Context, Poll};

use crate::rt::{RtFuture, WakerArg, PENDING};
use crate::{api, IoResultSlot};

/// A connected TCP stream owned by the runtime.
pub struct TcpStream {
    h: u64,
}

// SAFETY: the runtime's stream may be used from any thread (one operation of each kind at a
// time, which `&mut self` ensures).
unsafe impl Send for TcpStream {}
// SAFETY: `&self` methods only read the socket handle.
unsafe impl Sync for TcpStream {}

/// An I/O error from a runtime error code: an OS error number (positive) or a negated runtime
/// code (native_abi.md "Async I/O").
pub(crate) fn io_error(code: i32) -> io::Error {
    if code > 0 {
        return io::Error::from_raw_os_error(code);
    }
    io::Error::from(kind_of(-code))
}

/// The `io::ErrorKind` of a runtime error code ([`crate::code`]).
pub(crate) fn kind_of(code: i32) -> io::ErrorKind {
    use crate::code as c;
    use io::ErrorKind as K;
    match code {
        c::NOT_FOUND => K::NotFound,
        c::PERMISSION_DENIED => K::PermissionDenied,
        c::ALREADY_EXISTS => K::AlreadyExists,
        c::INVALID_INPUT => K::InvalidInput,
        c::INVALID_DATA => K::InvalidData,
        c::TIMED_OUT => K::TimedOut,
        c::CONNECTION_REFUSED => K::ConnectionRefused,
        c::CONNECTION_RESET => K::ConnectionReset,
        c::ADDR_IN_USE => K::AddrInUse,
        c::BROKEN_PIPE => K::BrokenPipe,
        c::UNEXPECTED_EOF => K::UnexpectedEof,
        c::UNSUPPORTED => K::Unsupported,
        _ => K::Other,
    }
}

impl TcpStream {
    /// Connects to `addr` (`"host:port"`; the name is resolved off the workers).
    pub async fn connect(addr: &str) -> io::Result<TcpStream> {
        // SAFETY: valid bytes; the future's result is `IoResult<u64>`.
        let raw = unsafe { (api().tcp_connect)(addr.as_ptr(), addr.len()) };
        let r: IoResultSlot<u64> = unsafe { RtFuture::new(raw) }.await;
        if r.err.code == 0 {
            // SAFETY: success: the value is set.
            return Ok(TcpStream {
                h: unsafe { r.value.assume_init() },
            });
        }
        let message = crate::take_str(r.err.message);
        Err(io::Error::new(kind_of(r.err.code), message))
    }

    /// Takes over a connected standard stream (registered with this worker's reactor).
    pub fn from_std(s: std::net::TcpStream) -> io::Result<TcpStream> {
        #[cfg(unix)]
        let socket = std::os::fd::IntoRawFd::into_raw_fd(s) as i64;
        #[cfg(windows)]
        let socket = std::os::windows::io::IntoRawSocket::into_raw_socket(s) as i64;
        let mut h = 0u64;
        // SAFETY: the stream now owns the socket.
        match unsafe { (api().stream_from_socket)(socket, &mut h) } {
            0 => Ok(TcpStream { h }),
            e => Err(io_error(e)),
        }
    }

    /// Reads into `buf`: `Ready(Ok(0))` at the end of the stream.
    pub fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        // SAFETY: initialized bytes are valid uninitialized ones.
        unsafe { self.poll_read_raw(cx, buf.as_mut_ptr(), buf.len()) }
    }

    /// # Safety
    /// `buf` is valid for writes of `cap` bytes.
    unsafe fn poll_read_raw(
        &mut self,
        cx: &mut Context<'_>,
        buf: *mut u8,
        cap: usize,
    ) -> Poll<io::Result<usize>> {
        let arg = WakerArg::of(cx.waker());
        let (mut n, mut err) = (0usize, 0i32);
        if (api().stream_poll_read)(self.h, &arg, buf, cap, &mut n, &mut err) == PENDING {
            return Poll::Pending;
        }
        Poll::Ready(if err == 0 { Ok(n) } else { Err(io_error(err)) })
    }

    /// Writes from `buf`; the bytes written.
    pub fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let arg = WakerArg::of(cx.waker());
        let (mut n, mut err) = (0usize, 0i32);
        // SAFETY: a live stream; `buf` valid for reads.
        let r = unsafe {
            (api().stream_poll_write)(self.h, &arg, buf.as_ptr(), buf.len(), &mut n, &mut err)
        };
        if r == PENDING {
            return Poll::Pending;
        }
        Poll::Ready(if err == 0 { Ok(n) } else { Err(io_error(err)) })
    }

    /// Flushes what is buffered (nothing, for a socket).
    pub fn poll_flush(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.status(cx, api().stream_poll_flush)
    }

    /// Shuts down the writing half.
    pub fn poll_shutdown(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.status(cx, api().stream_poll_shutdown)
    }

    fn status(
        &mut self,
        cx: &mut Context<'_>,
        f: unsafe extern "C" fn(u64, *const WakerArg, *mut i32) -> u32,
    ) -> Poll<io::Result<()>> {
        let arg = WakerArg::of(cx.waker());
        let mut err = 0i32;
        // SAFETY: a live stream and a waker valid for the call.
        if unsafe { f(self.h, &arg, &mut err) } == PENDING {
            return Poll::Pending;
        }
        Poll::Ready(if err == 0 { Ok(()) } else { Err(io_error(err)) })
    }

    /// The socket, for socket options (`socket2::SockRef::from(&stream.socket())`). The stream
    /// keeps owning it.
    #[cfg(unix)]
    pub fn socket(&self) -> std::os::fd::BorrowedFd<'_> {
        // SAFETY: the stream owns this descriptor for as long as `self` lives.
        unsafe { std::os::fd::BorrowedFd::borrow_raw((api().stream_socket)(self.h) as i32) }
    }

    /// The socket, for socket options. The stream keeps owning it.
    #[cfg(windows)]
    pub fn socket(&self) -> std::os::windows::io::BorrowedSocket<'_> {
        // SAFETY: the stream owns this socket for as long as `self` lives.
        unsafe {
            std::os::windows::io::BorrowedSocket::borrow_raw((api().stream_socket)(self.h) as u64)
        }
    }

    /// Sets `TCP_NODELAY`.
    pub fn set_nodelay(&self, on: bool) -> io::Result<()> {
        let fd = self.socket();
        #[cfg(unix)]
        let s = {
            use std::os::fd::{AsRawFd, FromRawFd};
            std::mem::ManuallyDrop::new(unsafe { std::net::TcpStream::from_raw_fd(fd.as_raw_fd()) })
        };
        #[cfg(windows)]
        let s = {
            use std::os::windows::io::{AsRawSocket, FromRawSocket};
            std::mem::ManuallyDrop::new(unsafe {
                std::net::TcpStream::from_raw_socket(fd.as_raw_socket())
            })
        };
        s.set_nodelay(on)
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { (api().stream_close)(self.h) }
    }
}

#[cfg(feature = "tokio")]
mod tokio_io {
    use super::TcpStream;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    impl AsyncRead for TcpStream {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            // SAFETY: the runtime only writes into the unfilled part, then reports how much.
            let unfilled = unsafe { buf.unfilled_mut() };
            let (ptr, cap) = (unfilled.as_mut_ptr() as *mut u8, unfilled.len());
            match unsafe { this.poll_read_raw(cx, ptr, cap) } {
                Poll::Ready(Ok(n)) => {
                    // SAFETY: the runtime initialized `n` bytes.
                    unsafe { buf.assume_init(n) };
                    buf.advance(n);
                    Poll::Ready(Ok(()))
                }
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Pending => Poll::Pending,
            }
        }
    }

    impl AsyncWrite for TcpStream {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.get_mut().poll_write(cx, buf)
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.get_mut().poll_flush(cx)
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.get_mut().poll_shutdown(cx)
        }
    }
}
