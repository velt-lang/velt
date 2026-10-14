//! Sockets of the Velt backend: `mio` TCP streams and listeners, registered with the reactor of
//! the worker that makes them (worker 0 off the workers). Work stays on the worker that made it
//! (see [`super::worker`]), so a connection's events reach the worker that runs its task.
//!
//! Readiness is edge-triggered and kept per registration as `tick << 8 | bits`: an event sets
//! bits and bumps the tick; an operation that hit `WouldBlock` clears its bit only if no event
//! came in since it looked (the tick is unchanged), so no wake-up is lost. The TCP types are in
//! [`tcp`].

use futures_util::task::AtomicWaker;
use std::io;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};

mod tcp;
pub use tcp::{TcpListener, TcpStream};

pub(super) const READABLE: usize = 1;
pub(super) const WRITABLE: usize = 2;
const READ_CLOSED: usize = 4;
const WRITE_CLOSED: usize = 8;
const ERROR: usize = 16;
const BITS: usize = 0xff;

/// Readiness of one socket and the tasks waiting for it: one reader and one writer (a stream),
/// or any number of readers (a listener several tasks accept on).
pub(super) struct Registration {
    readiness: AtomicUsize,
    reader: AtomicWaker,
    writer: AtomicWaker,
    readers: Mutex<Vec<std::task::Waker>>,
}

/// Each worker's registrations, by token (slot index; a freed slot is reused).
#[derive(Default)]
struct Table {
    slots: Vec<Option<Arc<Registration>>>,
    free: Vec<usize>,
}

fn tables() -> &'static [Mutex<Table>] {
    static TABLES: OnceLock<Vec<Mutex<Table>>> = OnceLock::new();
    TABLES.get_or_init(|| {
        (0..super::worker::workers().len())
            .map(|_| Mutex::new(Table::default()))
            .collect()
    })
}

/// Sets the readiness an event reports and wakes who waits for it (on the worker whose reactor
/// produced it).
pub(super) fn dispatch(worker: usize, ev: &mio::event::Event) {
    let reg = {
        let t = tables()[worker].lock().unwrap_or_else(|e| e.into_inner());
        match t.slots.get(ev.token().0) {
            Some(Some(r)) => r.clone(),
            _ => return,
        }
    };
    let mut bits = 0;
    if ev.is_readable() {
        bits |= READABLE;
    }
    if ev.is_writable() {
        bits |= WRITABLE;
    }
    if ev.is_read_closed() {
        bits |= READ_CLOSED;
    }
    if ev.is_write_closed() {
        bits |= WRITE_CLOSED;
    }
    if ev.is_error() {
        bits |= ERROR;
    }
    let mut cur = reg.readiness.load(Ordering::Acquire);
    loop {
        let next = ((cur >> 8).wrapping_add(1) << 8) | (cur & BITS) | bits;
        match reg
            .readiness
            .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => break,
            Err(now) => cur = now,
        }
    }
    if bits & (READABLE | READ_CLOSED | ERROR) != 0 {
        reg.reader.wake();
        let waiting = std::mem::take(&mut *reg.readers.lock().unwrap_or_else(|e| e.into_inner()));
        waiting.into_iter().for_each(std::task::Waker::wake);
    }
    if bits & (WRITABLE | WRITE_CLOSED | ERROR) != 0 {
        reg.writer.wake();
    }
}

/// A socket's place in a reactor: `worker << 32 | token` (a connection whose task moved to
/// another worker follows it there, see `TcpStream::follow`), and its readiness.
pub(super) struct Registered {
    pub(super) place: AtomicU64,
    pub(super) reg: Arc<Registration>,
}

pub(super) fn place(worker: usize, token: usize) -> u64 {
    (worker as u64) << 32 | token as u64
}

pub(super) fn unplace(p: u64) -> (usize, usize) {
    ((p >> 32) as usize, (p & 0xffff_ffff) as usize)
}

/// A slot for `reg` in `worker`'s table; its token.
pub(super) fn alloc(worker: usize, reg: &Arc<Registration>) -> usize {
    let mut t = tables()[worker].lock().unwrap_or_else(|e| e.into_inner());
    let i = t.free.pop().unwrap_or(t.slots.len());
    if i == t.slots.len() {
        t.slots.push(None);
    }
    t.slots[i] = Some(reg.clone());
    i
}

/// A `mio` socket registered with a worker's reactor.
pub(super) struct Io<S: mio::event::Source> {
    pub(super) sock: S,
    pub(super) registered: Registered,
    /// Several tasks may wait to read at once (a listener).
    shared_reads: bool,
}

impl<S: mio::event::Source> Io<S> {
    /// Registers `sock` with this thread's worker (worker 0 off the workers).
    pub(super) fn new(mut sock: S, shared_reads: bool) -> io::Result<Io<S>> {
        super::start();
        let worker = super::worker::current().unwrap_or(0);
        let reg = Arc::new(Registration {
            readiness: AtomicUsize::new(0),
            reader: AtomicWaker::new(),
            writer: AtomicWaker::new(),
            readers: Mutex::new(Vec::new()),
        });
        let token = alloc(worker, &reg);
        let registry = &super::worker::workers()[worker].registry;
        if let Err(e) = registry.register(&mut sock, mio::Token(token), INTEREST) {
            release(worker, token);
            return Err(e);
        }
        Ok(Io {
            sock,
            registered: Registered {
                place: AtomicU64::new(place(worker, token)),
                reg,
            },
            shared_reads,
        })
    }

    /// Ready for `interest` (READABLE or WRITABLE); the tick it saw.
    pub(super) fn poll_ready(
        &self,
        cx: &mut Context<'_>,
        interest: usize,
    ) -> Poll<io::Result<usize>> {
        let r = &self.registered;
        let (waker, done) = if interest == READABLE {
            (&r.reg.reader, READABLE | READ_CLOSED | ERROR)
        } else {
            (&r.reg.writer, WRITABLE | WRITE_CLOSED | ERROR)
        };
        let cur = r.reg.readiness.load(Ordering::Acquire);
        if cur & done != 0 {
            return Poll::Ready(Ok(cur >> 8));
        }
        if interest == READABLE && self.shared_reads {
            let mut list = r.reg.readers.lock().unwrap_or_else(|e| e.into_inner());
            if !list.iter().any(|w| w.will_wake(cx.waker())) {
                list.push(cx.waker().clone());
            }
        } else {
            waker.register(cx.waker());
        }
        let cur = r.reg.readiness.load(Ordering::Acquire);
        if cur & done != 0 {
            return Poll::Ready(Ok(cur >> 8));
        }
        Poll::Pending
    }

    /// Clears `interest` after a `WouldBlock`, unless an event came in since `tick`.
    fn clear(&self, interest: usize, tick: usize) {
        let r = &self.registered;
        let mut cur = r.reg.readiness.load(Ordering::Acquire);
        while cur >> 8 == tick && cur & interest != 0 {
            match r.reg.readiness.compare_exchange_weak(
                cur,
                cur & !interest,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(now) => cur = now,
            }
        }
    }

    /// `op` once the socket looks ready for `interest`; `WouldBlock` clears the readiness and
    /// waits again.
    pub(super) fn poll_io<R>(
        &self,
        cx: &mut Context<'_>,
        interest: usize,
        mut op: impl FnMut(&S) -> io::Result<R>,
    ) -> Poll<io::Result<R>> {
        loop {
            let tick = match self.poll_ready(cx, interest) {
                Poll::Ready(Ok(t)) => t,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            };
            match op(&self.sock) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.clear(interest, tick),
                r => return Poll::Ready(r),
            }
        }
    }

    /// `op` now, without waiting (`WouldBlock` clears the readiness).
    pub(super) fn try_io<R>(
        &self,
        interest: usize,
        op: impl FnOnce(&S) -> io::Result<R>,
    ) -> io::Result<R> {
        let tick = self.registered.reg.readiness.load(Ordering::Acquire) >> 8;
        let r = op(&self.sock);
        if matches!(&r, Err(e) if e.kind() == io::ErrorKind::WouldBlock) {
            self.clear(interest, tick);
        }
        r
    }
}

impl<S: mio::event::Source> Drop for Io<S> {
    fn drop(&mut self) {
        let (worker, token) = unplace(self.registered.place.load(Ordering::Acquire));
        let registry = &super::worker::workers()[worker].registry;
        let _ = registry.deregister(&mut self.sock);
        release(worker, token);
    }
}

pub(super) const INTEREST: mio::Interest = mio::Interest::READABLE.add(mio::Interest::WRITABLE);

pub(super) fn release(worker: usize, token: usize) {
    let mut t = tables()[worker].lock().unwrap_or_else(|e| e.into_inner());
    t.slots[token] = None;
    t.free.push(token);
}
