//! Deferred writes (`TcpStream.writeLater`, `drained`): bytes a task writes during a poll are
//! collected per stream and sent when the poll ends, in one system call for all of them. A
//! server that writes a response in pieces (an HTTP head, then the chunks a render produced
//! without waiting) sends one packet where it would send several, without holding back a piece
//! that is followed by a wait: the poll ends there.
//!
//! - [`StreamObj::write_later`] appends to the stream's [`Later`] buffer and puts the stream on
//!   this thread's list of streams to send ([`DUE`]) once per poll.
//! - [`flush_due`] runs after every poll of a task (next to `publish_thread_output`): it writes
//!   each stream's bytes at once when the socket takes them, and leaves what it cannot take to a
//!   background task that holds the socket's turn while it writes.
//! - While deferred bytes are waiting or being written, a `write` joins the same queue
//!   ([`StreamObj::write_queued`]): bytes go out in the order they were written.
//! - [`StreamObj::drained`] waits until at most `max` bytes are still to be sent: backpressure
//!   for a writer that produces faster than the peer reads.
//!
//! A failure to send is kept: the next `writeLater`, `write` or `drained` reports it.

use super::stream::{StreamObj, Turn, Writing};
use std::cell::RefCell;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;

/// A stream's deferred bytes.
#[derive(Default)]
pub(crate) struct Later {
    state: Mutex<State>,
    /// Notified when bytes were sent, or sending failed.
    progress: Notify,
}

#[derive(Default)]
struct State {
    /// Bytes not handed to a writer yet.
    buf: Vec<u8>,
    /// Bytes appended in all (also those written by now), and bytes written.
    queued: u64,
    written: u64,
    /// The stream is on a thread's [`DUE`] list.
    due: bool,
    /// A background task is sending: it takes what is appended meanwhile.
    flushing: bool,
    /// Why sending failed (the stream sends nothing more).
    failed: Option<(io::ErrorKind, String)>,
}

impl State {
    fn error(&self) -> Option<io::Error> {
        self.failed
            .as_ref()
            .map(|(kind, msg)| io::Error::new(*kind, msg.clone()))
    }
}

/// The most of a sent buffer's capacity kept for the next poll's bytes.
const KEEP: usize = 64 << 10;

thread_local! {
    /// The streams this thread appended to during the current poll.
    static DUE: RefCell<Vec<Arc<StreamObj>>> = const { RefCell::new(Vec::new()) };
}

impl Later {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether bytes are waiting or being written: a `write` then goes after them.
    pub(crate) fn active(&self) -> bool {
        let st = self.lock();
        st.flushing || !st.buf.is_empty()
    }
}

/// Sends the bytes deferred during the poll that just ended on this thread.
pub fn flush_due() {
    let due = DUE.with(|d| std::mem::take(&mut *d.borrow_mut()));
    for obj in due {
        obj.send_later();
    }
}

impl StreamObj {
    /// `writeLater`: appends `data`, sent when this poll ends. The bytes still to be sent.
    pub(crate) fn write_later(self: &Arc<Self>, data: &[u8]) -> io::Result<u64> {
        let mut st = self.later.lock();
        if let Some(e) = st.error() {
            return Err(e);
        }
        self.check_open()?;
        st.buf.extend_from_slice(data);
        st.queued += data.len() as u64;
        if !st.due && !st.flushing {
            st.due = true;
            DUE.with(|d| d.borrow_mut().push(self.clone()));
        }
        Ok(st.queued - st.written)
    }

    /// `write` while deferred bytes go first: appends `data` behind them, sends now, and resolves
    /// once `data` is written.
    pub(crate) async fn write_queued(self: Arc<Self>, data: &[u8]) -> io::Result<()> {
        let mark = {
            let mut st = self.later.lock();
            if let Some(e) = st.error() {
                return Err(e);
            }
            st.buf.extend_from_slice(data);
            st.queued += data.len() as u64;
            st.queued
        };
        self.clone().send_later();
        self.until_written(mark).await
    }

    /// `drained(max)`: resolves once at most `max` bytes are still to be sent (those this poll
    /// deferred are sent when it ends: wait for that only beyond `max`).
    pub(crate) async fn drained(self: Arc<Self>, max: u64) -> io::Result<()> {
        let mark = {
            let st = self.later.lock();
            st.queued.saturating_sub(max)
        };
        self.until_written(mark).await
    }

    /// Waits until `mark` bytes in all were written.
    async fn until_written(&self, mark: u64) -> io::Result<()> {
        loop {
            let progress = self.later.progress.notified();
            let mut progress = std::pin::pin!(progress);
            progress.as_mut().enable();
            {
                let st = self.later.lock();
                if let Some(e) = st.error() {
                    return Err(e);
                }
                if st.written >= mark {
                    return Ok(());
                }
            }
            progress.await;
        }
    }

    /// Sends what is deferred: at once what the socket takes, the rest in the background.
    fn send_later(self: Arc<Self>) {
        let mut st = self.later.lock();
        st.due = false;
        if st.flushing || st.buf.is_empty() || st.failed.is_some() {
            return;
        }
        let (w, first) = Writing::begin(self.clone());
        // Written here, without a task, when no other write goes on and the socket takes it.
        let mut turn = None;
        let mut sent = 0;
        if let Some((stream, t)) = self.plain().filter(|_| first).zip(self.try_turn()) {
            while sent < st.buf.len() {
                match stream.try_write(&st.buf[sent..]) {
                    Ok(n) => sent += n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        drop(t);
                        return self.fail(st, e);
                    }
                }
            }
            st.written += sent as u64;
            if sent == st.buf.len() {
                st.buf.clear();
                if st.buf.capacity() > KEEP {
                    st.buf = Vec::new();
                }
                drop(st);
                self.later.progress.notify_waiters();
                return;
            }
            turn = Some(t);
        }
        let rest = st.buf[sent..].to_vec();
        st.buf.clear();
        st.flushing = true;
        drop(st);
        if sent > 0 {
            self.later.progress.notify_waiters();
        }
        crate::task::runtime::spawn(flush(w, rest, turn));
    }

    /// Sending failed: nothing more is sent, and the waiters learn why.
    fn fail(&self, mut st: MutexGuard<'_, State>, e: io::Error) {
        st.failed = Some((e.kind(), e.to_string()));
        st.buf = Vec::new();
        st.flushing = false;
        drop(st);
        self.later.progress.notify_waiters();
    }
}

/// Writes `rest`, then what was appended meanwhile, each in a turn of its own: a `write` that
/// waits for the socket's turn goes between them, after the bytes deferred before it.
async fn flush(w: Writing, mut rest: Vec<u8>, mut turn: Option<Turn>) {
    let obj = w.0.clone();
    loop {
        if let Err(e) = obj.write_in_turn(&rest, turn.take()).await {
            return obj.fail(obj.later.lock(), e);
        }
        let mut st = obj.later.lock();
        st.written += rest.len() as u64;
        if st.buf.is_empty() {
            st.flushing = false;
            drop(st);
            obj.later.progress.notify_waiters();
            return;
        }
        rest.clear();
        std::mem::swap(&mut rest, &mut st.buf);
        drop(st);
        obj.later.progress.notify_waiters();
    }
}

#[cfg(test)]
pub(crate) fn due_len() -> usize {
    DUE.with(|d| d.borrow().len())
}

#[cfg(test)]
mod tests {
    use super::super::stream::StreamObj;
    use super::super::sys::TcpStream;
    use std::io::Read;
    use std::sync::Arc;

    #[test]
    fn queued_bytes_leave_together_when_the_poll_ends() {
        crate::task::runtime::runtime().block_on(async {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let mut peer = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (accepted, _) = listener.accept().unwrap();
            accepted.set_nonblocking(true).unwrap();
            let obj = Arc::new(StreamObj::tcp(TcpStream::from_std(accepted).unwrap()));

            assert_eq!(obj.write_later(b"ab").unwrap(), 2);
            assert_eq!(obj.write_later(b"cd").unwrap(), 4);
            assert_eq!(super::due_len(), 1, "a stream is listed once per poll");
            peer.set_nonblocking(true).unwrap();
            let mut buf = [0u8; 16];
            let early = peer.read(&mut buf);
            assert!(
                matches!(&early, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
                "nothing is sent before the poll ends: {early:?}"
            );

            super::flush_due();
            assert_eq!(super::due_len(), 0);
            obj.clone().drained(0).await.unwrap();
            peer.set_nonblocking(false).unwrap();
            peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut got = Vec::new();
            while got.len() < 4 {
                let n = peer.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
            }
            assert_eq!(got, b"abcd");
        });
    }
}
