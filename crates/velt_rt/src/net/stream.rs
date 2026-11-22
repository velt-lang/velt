//! The object behind a `TcpStream` handle: a TCP socket, used through `&TcpStream` readiness
//! APIs: no lock between a read and a write, and a read allocates its buffer once there is
//! something to read, so a connection waiting for data holds none. (TLS is Velt's, over these
//! streams or the reactor's sockets: std/net/tls/.)
//!
//! `abort` makes operations in flight fail: a write to a peer that stopped reading would
//! otherwise keep the socket alive for ever. A raw socket's read does not watch for it (that
//! would cost every read); `abort` shuts the socket down, which ends the read, and the read then
//! reports the abort instead of the end of the stream.

use super::sys::TcpStream;
use super::utf8::Utf8Decoder;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{fence, AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use tokio::sync::{Mutex as AsyncMutex, Notify};

/// A stream plus the undecoded tail of a UTF-8 sequence split across `readString`s.
pub struct StreamObj {
    io: TcpStream,
    pub(crate) utf8: Mutex<Utf8Decoder>,
    aborted: AtomicBool,
    /// The handle was closed while an operation was in flight: waits for input end with
    /// `EBADF` (a write in flight goes on). See [`StreamObj::close_reads`].
    reads_closed: AtomicBool,
    abort: Notify,
    /// Writes that are still going on (not done at the call); while there are any, a new write
    /// is not written at the call, which would put its bytes before theirs.
    pub(crate) writing: AtomicUsize,
    /// Taken by a raw socket's write for as long as it goes on: writes go out one after another,
    /// in the order they started (a `write` first polls at its call; the lock is FIFO).
    turn: std::sync::Arc<AsyncMutex<()>>,
    /// Bytes written with `writeLater`, sent when the poll ends (net/later.rs).
    pub(crate) later: super::later::Later,
}

/// A raw socket's turn to write ([`StreamObj::turn`]), held across the write.
pub(crate) type Turn = tokio::sync::OwnedMutexGuard<()>;

/// A write counted in [`StreamObj::writing`] from its call until it is done or dropped.
pub(crate) struct Writing(pub(crate) std::sync::Arc<StreamObj>);

impl Writing {
    /// Counts a write from its call; also whether no other write was going on then.
    pub(crate) fn begin(obj: std::sync::Arc<StreamObj>) -> (Writing, bool) {
        let earlier = obj.writing.fetch_add(1, Ordering::AcqRel) != 0;
        (Writing(obj), !earlier)
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        self.0.writing.fetch_sub(1, Ordering::AcqRel);
    }
}

fn aborted_error() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "the stream was aborted")
}

/// Reads up to this size go through [`READ_SCRATCH`].
const SCRATCH: usize = 256 << 10;

thread_local! {
    static READ_SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn buffer(max: u64) -> Vec<u8> {
    Vec::with_capacity(max.clamp(1, 16 << 20) as usize)
}

/// Read what is available (at least 1 byte, at most `max`; empty only at end of stream).
pub(crate) async fn read_chunk(stream: &TcpStream, max: u64) -> io::Result<Vec<u8>> {
    let limit = usize::try_from(max).unwrap_or(usize::MAX);
    loop {
        // The buffer is allocated once there is something to read, so a connection waiting
        // for data holds none (a server's idle connections would otherwise each keep `max`).
        stream.readable().await?;
        if max > SCRATCH as u64 {
            let mut buf = buffer(max);
            match stream.try_read_buf(&mut buf, limit) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e),
                Ok(_) => return Ok(buf),
            }
        }
        // Read into this thread's scratch buffer and keep only what came: a message is usually
        // far smaller than `max`, and a buffer of `max` for each read costs more than the copy.
        let r = READ_SCRATCH.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            scratch.clear();
            scratch.reserve(max as usize);
            stream
                .try_read_buf(&mut scratch, limit)
                .map(|_| scratch.to_vec())
        });
        match r {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            r => return r,
        }
    }
}

fn shutdown_socket(stream: &TcpStream, how: std::net::Shutdown) -> io::Result<()> {
    // tokio only offers shutdown through `AsyncWrite` (needs `&mut`); go to the socket directly.
    stream.sock_ref().shutdown(how)
}

impl StreamObj {
    pub(crate) fn tcp(io: TcpStream) -> StreamObj {
        StreamObj {
            io,
            utf8: Mutex::new(Utf8Decoder::default()),
            aborted: AtomicBool::new(false),
            reads_closed: AtomicBool::new(false),
            abort: Notify::new(),
            writing: AtomicUsize::new(0),
            turn: std::sync::Arc::new(AsyncMutex::new(())),
            later: Default::default(),
        }
    }

    /// `f`, or an error as soon as the stream is aborted.
    async fn until_abort<T>(&self, f: impl Future<Output = io::Result<T>>) -> io::Result<T> {
        self.until_ended(f, false).await
    }

    /// [`until_abort`](Self::until_abort) for a read: it also ends once the handle was closed
    /// ([`close_reads`](Self::close_reads)).
    async fn until_read_ends<T>(&self, f: impl Future<Output = io::Result<T>>) -> io::Result<T> {
        self.until_ended(f, true).await
    }

    async fn until_ended<T>(
        &self,
        f: impl Future<Output = io::Result<T>>,
        reading: bool,
    ) -> io::Result<T> {
        let mut f = std::pin::pin!(f);
        loop {
            let mut woken = std::pin::pin!(self.abort.notified());
            woken.as_mut().enable();
            if self.aborted.load(Ordering::Acquire) {
                return Err(aborted_error());
            }
            if reading && self.reads_closed.load(Ordering::Acquire) {
                return Err(crate::registry::closed_io_error());
            }
            match futures_util::future::select(f.as_mut(), woken).await {
                // A read that ended because the handle was closed (the socket's read side shut
                // down) reports the close, not the end of the stream.
                futures_util::future::Either::Left(_)
                    if reading && self.reads_closed.load(Ordering::Acquire) =>
                {
                    return Err(crate::registry::closed_io_error());
                }
                futures_util::future::Either::Left((r, _)) => return r,
                // Woken: an abort ends every operation, a close only reads.
                futures_util::future::Either::Right(_) => {}
            }
        }
    }

    /// The handle was closed while an operation was in flight (something still holds the
    /// stream): no code can read what a wait for input would bring, so those waits end (the read
    /// side of the socket is shut down, and the waits are woken); a write in flight goes on.
    pub(crate) fn close_reads(&self) {
        self.reads_closed.store(true, Ordering::Release);
        self.abort.notify_waiters();
        let _ = shutdown_socket(&self.io, std::net::Shutdown::Read);
    }

    /// `read_chunk` on this stream's raw socket: after `abort` (which shut the socket down, ending
    /// the read) it fails as every other operation does, instead of reading the end of the stream,
    /// and so after a close of the handle while it waited (`EBADF`).
    pub(crate) async fn read_raw(&self, stream: &TcpStream, max: u64) -> io::Result<Vec<u8>> {
        let r = self.until_read_ends(read_chunk(stream, max)).await;
        if self.aborted.load(Ordering::Acquire) {
            return Err(aborted_error());
        }
        r
    }

    /// Reads what is available (at least 1 byte, at most `max`; empty only at end of stream).
    pub(crate) async fn read(&self, max: u64) -> io::Result<Vec<u8>> {
        self.read_raw(&self.io, max).await
    }

    /// Reads what is there into `dst` without waiting (`dst` not empty): the bytes read, 0 at the
    /// end of the stream, `WouldBlock` when there is nothing yet ([`readable`](Self::readable)
    /// waits for more). It reads straight into `dst`.
    pub(crate) fn try_read_into(&self, dst: &mut [u8]) -> io::Result<usize> {
        if self.aborted.load(Ordering::Acquire) {
            return Err(aborted_error());
        }
        self.io.try_read(dst)
    }

    /// Whether an idle stream (one parked between requests) can carry another request: nothing
    /// has arrived on it, not even its end (asked with a system call).
    pub(crate) fn idle_open(&self) -> bool {
        !self.aborted.load(Ordering::Acquire) && self.io.idle_open()
    }

    /// Waits until [`try_read_into`](Self::try_read_into) may find something (data, the end of
    /// the stream or an error; it may also wake without data).
    pub(crate) async fn readable(&self) -> io::Result<()> {
        // `abort` shuts the socket down, which makes it readable (the next `try_read_into`
        // reports the abort); a close while this waits wakes it (shutting down only the read side
        // does not on every OS).
        self.until_read_ends(self.io.readable()).await
    }

    /// An error once the stream was aborted.
    pub(crate) fn check_open(&self) -> io::Result<()> {
        match self.aborted.load(Ordering::Acquire) {
            true => Err(aborted_error()),
            false => Ok(()),
        }
    }

    /// The raw socket's turn now, if no write has it (for a write made at the call).
    pub(crate) fn try_turn(&self) -> Option<Turn> {
        self.turn.clone().try_lock_owned().ok()
    }

    /// Writes all of `data`, in `turn` if the caller already has it (it wrote the first bytes at
    /// the call).
    pub(crate) async fn write_in_turn(&self, data: &[u8], turn: Option<Turn>) -> io::Result<()> {
        self.write_tcp(&self.io, data, turn).await
    }

    /// Writes all of `data` to a raw socket.
    async fn write_tcp(
        &self,
        stream: &TcpStream,
        mut data: &[u8],
        turn: Option<Turn>,
    ) -> io::Result<()> {
        let _turn = match turn {
            Some(t) => t,
            None => self.turn.clone().lock_owned().await,
        };
        while !data.is_empty() {
            // Not what the socket says after `abort` shut it down (a broken pipe, as if the
            // peer had closed): the stream was aborted.
            if self.aborted.load(Ordering::Acquire) {
                return Err(aborted_error());
            }
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

    /// The socket when operations can go straight to it: not after an abort, which shut it
    /// down (a read would see the end of the stream).
    pub(crate) fn plain(&self) -> Option<&TcpStream> {
        (!self.aborted.load(Ordering::Relaxed)).then_some(&self.io)
    }

    pub(crate) fn peer_addr(&self) -> Option<SocketAddr> {
        self.io.peer_addr().ok()
    }

    /// Half-close: the peer reads end of stream, after every byte written or queued before
    /// (`writeLater`). With nothing pending the socket is shut down at once; otherwise in the
    /// background, once those bytes are written.
    pub(crate) fn shutdown(self: &std::sync::Arc<Self>) -> io::Result<()> {
        let pending = self.later.active() || self.writing.load(Ordering::Acquire) > 0;
        if !pending {
            return shutdown_socket(&self.io, std::net::Shutdown::Write);
        }
        let obj = self.clone();
        crate::task::runtime::spawn(async move {
            // What was queued goes first, then what is being written (the socket's turn).
            let _ = obj.clone().drained(0).await;
            let _turn = obj.turn.clone().lock_owned().await;
            let _ = shutdown_socket(&obj.io, std::net::Shutdown::Write);
        });
        Ok(())
    }

    /// Fails the operations in flight and every later one.
    pub(crate) fn abort(&self) {
        self.aborted.store(true, Ordering::Release);
        self.abort.notify_waiters();
        fence(Ordering::SeqCst);
        let _ = shutdown_socket(&self.io, std::net::Shutdown::Both);
    }
}
