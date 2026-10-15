//! Timers of the Velt backend: each worker keeps a heap of deadlines and sleeps in its poll until
//! the earliest one; [`Sleep`] registers with the worker that polls it.

use futures_util::task::AtomicWaker;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

/// One registration: fired by the worker, or cancelled by its `Sleep`.
struct Entry {
    fired: AtomicBool,
    cancelled: AtomicBool,
    waker: AtomicWaker,
}

/// A worker's pending deadlines (earliest first; equal deadlines in registration order).
#[derive(Default)]
pub(super) struct Timers {
    heap: BinaryHeap<Reverse<(Instant, u64, Key)>>,
    seq: u64,
}

/// An entry ordered by address only (the heap orders by deadline and sequence first).
struct Key(Arc<Entry>);

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for Key {}
impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Key {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Arc::as_ptr(&self.0).cmp(&Arc::as_ptr(&other.0))
    }
}

impl Timers {
    fn add(&mut self, deadline: Instant, entry: Arc<Entry>) {
        self.seq += 1;
        self.heap.push(Reverse((deadline, self.seq, Key(entry))));
    }

    /// The earliest pending deadline.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.heap.peek().map(|Reverse((d, _, _))| *d)
    }

    /// Wakes every entry due at `now`.
    pub(super) fn fire(&mut self, now: Instant) {
        while let Some(Reverse((d, _, _))) = self.heap.peek() {
            if *d > now {
                break;
            }
            let Some(Reverse((_, _, Key(e)))) = self.heap.pop() else {
                break;
            };
            if !e.cancelled.load(Ordering::Acquire) {
                e.fired.store(true, Ordering::Release);
                e.waker.wake();
            }
        }
    }
}

/// Completes at `deadline`; can be moved to a new deadline with [`Sleep::reset`].
pub struct Sleep {
    deadline: Instant,
    entry: Option<Arc<Entry>>,
    /// Off the workers (a `block_on` on the main thread): tokio's timer, through the bridge.
    fallback: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl Sleep {
    pub fn until(deadline: Instant) -> Sleep {
        Sleep {
            deadline,
            entry: None,
            fallback: None,
        }
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn reset(&mut self, deadline: Instant) {
        self.cancel();
        self.deadline = deadline;
        self.fallback = None;
    }

    fn cancel(&mut self) {
        if let Some(e) = self.entry.take() {
            e.cancelled.store(true, Ordering::Release);
        }
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        if Instant::now() >= this.deadline {
            this.cancel();
            return Poll::Ready(());
        }
        if let Some(e) = &this.entry {
            e.waker.register(cx.waker());
            if e.fired.load(Ordering::Acquire) {
                return Poll::Ready(());
            }
            return Poll::Pending;
        }
        let deadline = this.deadline;
        let registered = super::worker::with_timers(|t| {
            let t = t?;
            let e = Arc::new(Entry {
                fired: AtomicBool::new(false),
                cancelled: AtomicBool::new(false),
                waker: AtomicWaker::new(),
            });
            e.waker.register(cx.waker());
            t.add(deadline, e.clone());
            Some(e)
        });
        match registered {
            Some(e) => {
                this.entry = Some(e);
                Poll::Pending
            }
            None => {
                let at = tokio::time::Instant::from_std(deadline);
                let f = this
                    .fallback
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(at)));
                f.as_mut().poll(cx)
            }
        }
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::task::{Wake, Waker};
    use std::time::Duration;

    struct Record(u32, Arc<Mutex<Vec<u32>>>);

    impl Wake for Record {
        fn wake(self: Arc<Self>) {
            self.1.lock().unwrap().push(self.0);
        }
    }

    fn entry(id: u32, log: &Arc<Mutex<Vec<u32>>>) -> Arc<Entry> {
        let e = Arc::new(Entry {
            fired: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        });
        e.waker
            .register(&Waker::from(Arc::new(Record(id, log.clone()))));
        e
    }

    #[test]
    fn due_timers_fire_in_deadline_order_then_registration_order() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let t0 = Instant::now();
        let mut timers = Timers::default();
        let ms = |n| t0 + Duration::from_millis(n);
        timers.add(ms(30), entry(30, &log));
        timers.add(ms(10), entry(10, &log));
        timers.add(ms(20), entry(21, &log));
        timers.add(ms(20), entry(22, &log));
        let cancelled = entry(99, &log);
        cancelled.cancelled.store(true, Ordering::Release);
        timers.add(ms(15), cancelled);
        assert_eq!(timers.next_deadline(), Some(ms(10)));
        timers.fire(ms(20));
        assert_eq!(*log.lock().unwrap(), vec![10, 21, 22]);
        assert_eq!(timers.next_deadline(), Some(ms(30)));
        timers.fire(ms(40));
        assert_eq!(*log.lock().unwrap(), vec![10, 21, 22, 30]);
    }
}
