//! Producers and consumers on the shared multi-thread runtime, on a channel of one, with sends
//! and receives cancelled while they wait (in line, or woken and unused) and a close at the end:
//! every item is received or dropped exactly once, and nothing hangs.
//!
//! It cannot catch a wake lost when a woken operation is cancelled: the next operation on the
//! channel takes the item or the room anyway. wait_tests.rs checks that the wake is handed on.

use super::*;
use crate::task::{
    raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, SendPtr, FUT_RESULT_OFFSET, PENDING,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

/// A channel future awaited by Rust code: its first 16 result bytes; dropped (cancelled when
/// unfinished) with the wrapper.
struct Op(SendPtr<VeltFut>);

impl Future for Op {
    type Output = [u8; 16];

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<[u8; 16]> {
        let f = self.0 .0;
        // SAFETY: an owned, live channel future.
        unsafe {
            if velt_rt_fut_poll(f, raw_cx(cx).ptr()) == PENDING {
                return Poll::Pending;
            }
            let mut out = [0u8; 16];
            std::ptr::copy_nonoverlapping(
                (f as *const u8).add(FUT_RESULT_OFFSET),
                out.as_mut_ptr(),
                16,
            );
            Poll::Ready(out)
        }
    }
}

impl Drop for Op {
    fn drop(&mut self) {
        // SAFETY: owned; dropped once (finished or not).
        unsafe { velt_rt_fut_drop(self.0 .0) };
    }
}

static DROPPED: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

unsafe extern "C-unwind" fn record(item: *mut u8) {
    DROPPED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(*(item as *const u64));
}

const PRODUCERS: u64 = 4;
const CONSUMERS: usize = 2;
/// Operations given up while they waited (so the test is known to cover them).
static SENDS_GIVEN_UP: AtomicUsize = AtomicUsize::new(0);
static RECEIVES_GIVEN_UP: AtomicUsize = AtomicUsize::new(0);
/// Of those, the ones woken before they were given up (the hand-on path).
static WOKEN_THEN_GIVEN_UP: AtomicUsize = AtomicUsize::new(0);
const PER_PRODUCER: u64 = 3000;
const FIRST_ID: u64 = 1_000_000;

fn send(h: Key<Chan>, id: u64) -> Op {
    // SAFETY: an 8-byte item with drop glue.
    Op(SendPtr(unsafe {
        velt_rt_chan_send(h, &id as *const u64 as *const u8, 8, Some(record))
    }))
}

fn receive(h: Key<Chan>) -> Op {
    // SAFETY: `u64 | null` as `{ present @0, item @8 }`.
    Op(SendPtr(unsafe { velt_rt_chan_receive(h, 8, 8, 16) }))
}

/// Polls `op` once; if it has to wait, lets the other tasks run a turn (which may wake it) and
/// then drops it unpolled: cancelled while in line, or after a wake it did not use.
async fn give_up_after_a_turn(mut op: Op) -> Option<[u8; 16]> {
    let woken = Arc::new(Woken {
        woken: AtomicBool::new(false),
        task: std::sync::Mutex::new(None),
    });
    let first = std::future::poll_fn(|cx| {
        *woken.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(cx.waker().clone());
        let waker = Waker::from(woken.clone());
        Poll::Ready(Pin::new(&mut op).poll(&mut Context::from_waker(&waker)))
    })
    .await;
    if let Poll::Ready(r) = first {
        return Some(r);
    }
    tokio::task::yield_now().await;
    if woken.woken.load(Ordering::Acquire) {
        WOKEN_THEN_GIVEN_UP.fetch_add(1, Ordering::Relaxed);
    }
    None
}

/// The waker a given-up operation waits with: notes that it was woken, and wakes the task.
struct Woken {
    woken: AtomicBool,
    task: std::sync::Mutex<Option<Waker>>,
}

impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.woken.store(true, Ordering::Release);
        if let Some(w) = self.task.lock().unwrap_or_else(|e| e.into_inner()).take() {
            w.wake();
        }
    }
}

async fn produce(h: Key<Chan>, p: u64) {
    for i in 0..PER_PRODUCER {
        let id = FIRST_ID + p * PER_PRODUCER + i;
        if i.is_multiple_of(5) {
            // Given up if it waits too long: the item is dropped, unless it was queued.
            if give_up_after_a_turn(send(h, id)).await.is_none() {
                SENDS_GIVEN_UP.fetch_add(1, Ordering::Relaxed);
            }
        } else {
            assert_eq!(send(h, id).await[0], 1, "open: queued");
        }
    }
}

async fn consume(h: Key<Chan>, c: usize) -> Vec<u64> {
    let mut got = Vec::new();
    let mut n = 0usize;
    loop {
        n += 1;
        let r = if (n + c).is_multiple_of(3) {
            // A receive given up while it waits must not lose an item.
            match give_up_after_a_turn(receive(h)).await {
                Some(r) => r,
                None => {
                    RECEIVES_GIVEN_UP.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        } else {
            receive(h).await
        };
        if r[0] == 0 {
            return got;
        }
        got.push(u64::from_le_bytes(r[8..16].try_into().unwrap_or_default()));
        // Slower than the producers: they wait for room.
        tokio::task::yield_now().await;
    }
}

#[test]
fn every_item_is_received_or_dropped_once_under_cancels_and_close() {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let h = velt_rt_chan_new(1);
        let hb = h.bits();
        let received = crate::task::runtime::runtime().block_on(async move {
            let h = Key::<Chan>::from_bits(hb);
            let consumers: Vec<_> = (0..CONSUMERS)
                .map(|c| tokio::spawn(consume(h, c)))
                .collect();
            let producers: Vec<_> = (0..PRODUCERS)
                .map(|p| tokio::spawn(produce(h, p)))
                .collect();
            for p in producers {
                p.await.expect("producer");
            }
            velt_rt_chan_close(h);
            let mut all = Vec::new();
            for c in consumers {
                all.extend(c.await.expect("consumer"));
            }
            all
        });
        let _ = done_tx.send(received);
    });
    let received = done_rx
        .recv_timeout(Duration::from_secs(120))
        .expect("hung: a wake was lost");
    let range = FIRST_ID..FIRST_ID + PRODUCERS * PER_PRODUCER;
    let dropped: Vec<u64> = DROPPED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .copied()
        .filter(|id| range.contains(id))
        .collect();
    let mut seen = vec![0u8; (PRODUCERS * PER_PRODUCER) as usize];
    for id in received.iter().chain(&dropped) {
        seen[(id - FIRST_ID) as usize] += 1;
    }
    let wrong: Vec<u64> = seen
        .iter()
        .enumerate()
        .filter(|(_, &n)| n != 1)
        .map(|(i, _)| FIRST_ID + i as u64)
        .take(5)
        .collect();
    assert!(
        wrong.is_empty(),
        "items received or dropped other than once: {wrong:?} ({} received, {} dropped)",
        received.len(),
        dropped.len()
    );
    assert!(!received.is_empty());
    assert!(
        SENDS_GIVEN_UP.load(Ordering::Relaxed) > 0 && RECEIVES_GIVEN_UP.load(Ordering::Relaxed) > 0,
        "cancels covered: {} sends, {} receives given up",
        SENDS_GIVEN_UP.load(Ordering::Relaxed),
        RECEIVES_GIVEN_UP.load(Ordering::Relaxed)
    );
    assert!(
        WOKEN_THEN_GIVEN_UP.load(Ordering::Relaxed) > 0,
        "no operation was given up after its wake"
    );
}
