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
//! otherwise keep the socket alive for ever. A raw socket's read does not watch for it (that
//! would cost every read); `abort` shuts the socket down, which ends the read, and the read then
//! reports the abort instead of the end of the stream.

use super::sys::TcpStream;
use super::utf8::Utf8Decoder;
use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{fence, AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{Mutex as AsyncMutex, Notify};

/// Any byte stream besides a raw TCP socket (TLS over TCP, an upgraded HTTP connection).
pub trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}

/// How long a TLS stream's `shutdown` (its close_notify) may take.
const SHUTDOWN: std::time::Duration = std::time::Duration::from_secs(5);

/// The future that sets a lazy stream up.
pub(crate) type Start = Pin<Box<dyn Future<Output = io::Result<Ready>> + Send>>;

/// A lazy stream once it is set up: a transport that is not a raw socket (TLS), split into its
/// two halves.
pub(crate) struct Ready {
    r: AsyncMutex<ReadHalf<Box<dyn Transport>>>,
    w: AsyncMutex<WriteHalf<Box<dyn Transport>>>,
    peer: Option<SocketAddr>,
}

impl Ready {
    pub(crate) fn other(io: Box<dyn Transport>, peer: Option<SocketAddr>) -> Ready {
        let (r, w) = tokio::io::split(io);
        Ready {
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
    /// What [`readable`](Self::readable) read from a transport that cannot say whether it has
    /// data without reading it (TLS, an upgraded connection), for the next
    /// [`try_read_into`](Self::try_read_into); `held_end` once that read found the end.
    held: Mutex<VecDeque<u8>>,
    held_end: AtomicBool,
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

/// The most a TLS or upgraded stream's [`StreamObj::readable`] reads ahead.
const DEFAULT_HELD: u64 = 64 << 10;

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
    fn new(io: Io) -> StreamObj {
        StreamObj {
            io,
            utf8: Mutex::new(Utf8Decoder::default()),
            aborted: AtomicBool::new(false),
            reads_closed: AtomicBool::new(false),
            abort: Notify::new(),
            writing: AtomicUsize::new(0),
            turn: std::sync::Arc::new(AsyncMutex::new(())),
            held: Mutex::new(VecDeque::new()),
            held_end: AtomicBool::new(false),
            later: Default::default(),
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
    /// side of a socket is shut down, a TLS or upgraded stream's waits are woken); a write in
    /// flight goes on.
    pub(crate) fn close_reads(&self) {
        self.reads_closed.store(true, Ordering::Release);
        self.abort.notify_waiters();
        if let Some(s) = self.socket() {
            let _ = shutdown_socket(s, std::net::Shutdown::Read);
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
        match &self.io {
            Io::Tcp(stream) => self.read_raw(stream, max).await,
            Io::Lazy(lazy) => self.read_lazy(lazy, max).await,
        }
    }

    /// Reads what is there into `dst` without waiting (`dst` not empty): the bytes read, 0 at the
    /// end of the stream, `WouldBlock` when there is nothing yet ([`readable`](Self::readable)
    /// waits for more). A raw socket reads straight into `dst`.
    pub(crate) fn try_read_into(&self, dst: &mut [u8]) -> io::Result<usize> {
        if self.aborted.load(Ordering::Acquire) {
            return Err(aborted_error());
        }
        match &self.io {
            Io::Tcp(stream) => return stream.try_read(dst),
            Io::Lazy(lazy) => match lazy.ready.get() {
                None => return Err(io::ErrorKind::WouldBlock.into()),
                Some(Err((kind, message))) => return Err(io::Error::new(*kind, message.clone())),
                Some(Ok(_)) => {}
            },
        }
        // A set-up transport: what `readable` read ahead of this call.
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if held.is_empty() {
            return if self.held_end.load(Ordering::Acquire) {
                Ok(0)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            };
        }
        // From the front of a deque: a small buffer reads a large chunk in linear time.
        let n = held.len().min(dst.len());
        let (a, b) = held.as_slices();
        let k = a.len().min(n);
        dst[..k].copy_from_slice(&a[..k]);
        dst[k..n].copy_from_slice(&b[..n - k]);
        held.drain(..n);
        Ok(n)
    }

    /// Whether an idle stream (one parked between requests) can carry another request: nothing
    /// has arrived on it, not even its end. A raw socket is asked with a system call; another
    /// transport (TLS) is polled for input once without waiting, which reads its records (a
    /// session ticket is not input) and finds a close the reactor has not reported yet.
    pub(crate) fn idle_open(&self) -> bool {
        if self.aborted.load(Ordering::Acquire) {
            return false;
        }
        let ready = match &self.io {
            Io::Tcp(stream) => return stream.idle_open(),
            Io::Lazy(lazy) => match lazy.ready.get() {
                Some(Ok(ready)) => ready,
                _ => return false,
            },
        };
        if self.held_end.load(Ordering::Acquire)
            || !self
                .held
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
        {
            return false;
        }
        let Ok(mut r) = ready.r.try_lock() else {
            return false;
        };
        let mut probe = [0u8; 1];
        let mut buf = tokio::io::ReadBuf::new(&mut probe);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        // Pending: nothing yet (cancel-safe: nothing was taken). Anything else (bytes, the end,
        // an error) means the connection is not idle and open.
        Pin::new(&mut *r).poll_read(&mut cx, &mut buf).is_pending()
    }

    /// Waits until [`try_read_into`](Self::try_read_into) may find something (data, the end of
    /// the stream or an error; a raw socket may also wake without data). A transport other than
    /// a raw socket reads its next chunk here, which the next `try_read_into` hands over.
    pub(crate) async fn readable(&self) -> io::Result<()> {
        // `abort` shuts a raw socket down, which makes it readable (the next `try_read_into`
        // reports the abort); a close while this waits wakes it (shutting down only the read side
        // does not on every OS).
        match &self.io {
            Io::Tcp(stream) => return self.until_read_ends(stream.readable()).await,
            Io::Lazy(lazy) => {
                self.until_read_ends(self.set_up(lazy)).await?;
            }
        }
        let has = !self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty();
        if has || self.held_end.load(Ordering::Acquire) {
            return Ok(());
        }
        let chunk = self.read(DEFAULT_HELD).await?;
        if chunk.is_empty() {
            self.held_end.store(true, Ordering::Release);
        } else {
            let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
            match held.is_empty() {
                true => *held = chunk.into(),
                false => held.extend(&chunk),
            }
        }
        Ok(())
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
        match &self.io {
            Io::Tcp(stream) => self.write_tcp(stream, data, turn).await,
            Io::Lazy(lazy) => self.write_lazy(lazy, data, turn).await,
        }
    }

    /// `read` on a lazy stream.
    async fn read_lazy(&self, lazy: &Lazy, max: u64) -> io::Result<Vec<u8>> {
        let ready = self.until_read_ends(self.set_up(lazy)).await?;
        {
            // What `readable` read ahead comes first.
            let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
            if !held.is_empty() {
                let n = held.len().min(max.clamp(1, 16 << 20) as usize);
                return Ok(held.drain(..n).collect());
            }
            if self.held_end.load(Ordering::Acquire) {
                return Ok(Vec::new());
            }
        }
        self.until_read_ends(async {
            let mut r = ready.r.lock().await;
            let mut buf = buffer(max);
            r.read_buf(&mut buf).await?;
            Ok(buf)
        })
        .await
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

    /// `write` on a lazy stream.
    async fn write_lazy(&self, lazy: &Lazy, data: &[u8], turn: Option<Turn>) -> io::Result<()> {
        let ready = self.until_abort(self.set_up(lazy)).await?;
        let _turn = turn;
        self.until_abort(async {
            let mut w = ready.w.lock().await;
            w.write_all(data).await?;
            w.flush().await
        })
        .await
    }

    /// The raw socket when operations can go straight to it: a plain TCP stream. (A stream that
    /// was aborted has its socket shut down: a read sees the end of the stream.)
    pub(crate) fn plain(&self) -> Option<&TcpStream> {
        match &self.io {
            Io::Tcp(s) => (!self.aborted.load(Ordering::Relaxed)).then_some(s),
            Io::Lazy(_) => None,
        }
    }

    /// The raw socket: a plain TCP stream's (a lazy one is another transport).
    fn socket(&self) -> Option<&TcpStream> {
        match &self.io {
            Io::Tcp(s) => Some(s),
            Io::Lazy(_) => None,
        }
    }

    pub(crate) fn peer_addr(&self) -> Option<SocketAddr> {
        match &self.io {
            Io::Tcp(s) => s.peer_addr().ok(),
            Io::Lazy(l) => match l.ready.get() {
                Some(Ok(ready)) => ready.peer,
                _ => None,
            },
        }
    }

    /// Half-close: the peer reads end of stream, after every byte written or queued before
    /// (`writeLater`). A raw socket with nothing pending is shut down at once; otherwise in the
    /// background, once those bytes are written and (any other stream) it is set up (TLS sends
    /// its close_notify).
    pub(crate) fn shutdown(self: &std::sync::Arc<Self>) -> io::Result<()> {
        let pending = self.later.active() || self.writing.load(Ordering::Acquire) > 0;
        if let (Some(s), false) = (self.socket(), pending) {
            return shutdown_socket(s, std::net::Shutdown::Write);
        }
        let obj = self.clone();
        crate::task::runtime::spawn(async move {
            // What was queued goes first, then what is being written (the socket's turn).
            let _ = obj.clone().drained(0).await;
            let _turn = obj.turn.clone().lock_owned().await;
            let lazy = match &obj.io {
                Io::Tcp(s) => {
                    let _ = shutdown_socket(s, std::net::Shutdown::Write);
                    return;
                }
                Io::Lazy(lazy) => lazy,
            };
            let _ = tokio::time::timeout(SHUTDOWN, async {
                if let Ok(ready) = obj.until_abort(obj.set_up(lazy)).await {
                    let _ = ready.w.lock().await.shutdown().await;
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
        fence(Ordering::SeqCst);
        if let Some(s) = self.socket() {
            let _ = shutdown_socket(s, std::net::Shutdown::Both);
        }
    }
}
