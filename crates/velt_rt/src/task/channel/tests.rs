//! Channel semantics through the C ABI, on the shared multi-thread runtime.

use super::*;
use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, FUT_RESULT_OFFSET, PENDING};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

/// Awaits a channel future and returns its first `N` result bytes.
struct Await<const N: usize>(SendPtr<VeltFut>);

impl<const N: usize> Future for Await<N> {
    type Output = [u8; N];

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<[u8; N]> {
        let f = self.0 .0;
        // SAFETY: an owned, live channel future; freed once READY.
        unsafe {
            if velt_rt_fut_poll(f, raw_cx(cx)) == PENDING {
                return Poll::Pending;
            }
            let mut out = [0u8; N];
            std::ptr::copy_nonoverlapping(
                (f as *const u8).add(FUT_RESULT_OFFSET),
                out.as_mut_ptr(),
                N,
            );
            velt_rt_fut_drop(f);
            Poll::Ready(out)
        }
    }
}

fn send(h: Key<Chan>, v: u64) -> Await<1> {
    // SAFETY: `h` is live in every test; `v` is an 8-byte item.
    Await(SendPtr(unsafe {
        velt_rt_chan_send(h, &v as *const u64 as *const u8, 8, None)
    }))
}

/// `u64 | null` laid out as `{ present: bool @0, item @8 }` (16 bytes).
fn receive(h: Key<Chan>) -> impl Future<Output = Option<u64>> + Send {
    // SAFETY: `h` is live in every test.
    let f = Await::<16>(SendPtr(unsafe { velt_rt_chan_receive(h, 8, 8, 16) }));
    async move {
        let b = f.await;
        (b[0] != 0).then(|| u64::from_le_bytes(b[8..16].try_into().unwrap_or_default()))
    }
}

fn run<F: Future<Output = ()> + Send + 'static>(f: F) {
    crate::task::runtime::handle().block_on(async { tokio::spawn(f).await.expect("test task") });
}

#[test]
fn items_arrive_in_order_and_close_ends_the_stream() {
    let h = velt_rt_chan_new(0);
    let hb = h.bits();
    run(async move {
        let h = Key::<Chan>::from_bits(hb);
        for v in [1, 2, 3] {
            assert_eq!(send(h, v).await, [1]);
        }
        velt_rt_chan_close(h);
        assert_eq!(send(h, 4).await, [0], "send on a closed channel fails");
        let mut got = vec![];
        while let Some(v) = receive(h).await {
            got.push(v);
        }
        assert_eq!(got, [1, 2, 3]);
        assert!(
            CHANNELS.get(h).is_none(),
            "closed and drained: out of the table"
        );
        assert!(velt_rt_chan_closed(h));
    });
}

#[test]
fn a_bounded_channel_makes_the_sender_wait() {
    let h = velt_rt_chan_new(2);
    let hb = h.bits();
    static RECEIVED: AtomicUsize = AtomicUsize::new(0);
    run(async move {
        let h = Key::<Chan>::from_bits(hb);
        let consumer = tokio::spawn(async move {
            let h = Key::<Chan>::from_bits(hb);
            tokio::time::sleep(Duration::from_millis(30)).await;
            while let Some(v) = receive(h).await {
                assert_eq!(v as usize, RECEIVED.fetch_add(1, Ordering::SeqCst));
            }
        });
        for v in 0..10u64 {
            send(h, v).await;
            assert!(velt_rt_chan_len(h) <= 2, "never more than the capacity");
        }
        velt_rt_chan_close(h);
        consumer.await.expect("consumer");
        assert_eq!(RECEIVED.load(Ordering::SeqCst), 10);
    });
}

#[test]
fn try_receive_writes_null_when_empty() {
    let h = velt_rt_chan_new(0);
    let mut slot = [0xffu8; 16];
    // SAFETY: a 16-byte `u64 | null` slot.
    unsafe { velt_rt_chan_try_receive(h, slot.as_mut_ptr(), 8, 8) };
    assert_eq!(slot[0], 0);
    velt_rt_chan_close(h);
    assert!(CHANNELS.get(h).is_none());
}

static TRY_DROPPED: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn count_try_drop(item: *mut u8) {
    TRY_DROPPED.fetch_add(*(item as *const u64) as usize, Ordering::SeqCst);
}

fn try_send(h: Key<Chan>, v: u64) -> bool {
    // SAFETY: an 8-byte item.
    unsafe { velt_rt_chan_try_send(h, &v as *const u64 as *const u8, 8, Some(count_try_drop)) }
}

#[test]
fn try_send_queues_only_with_room_and_drops_what_it_refuses() {
    let h = velt_rt_chan_new(1);
    assert!(try_send(h, 1));
    assert!(!try_send(h, 10), "full");
    assert_eq!(velt_rt_chan_len(h), 1);
    velt_rt_chan_close(h);
    assert!(!try_send(h, 100), "closed");
    assert_eq!(TRY_DROPPED.load(Ordering::SeqCst), 110);
    let mut slot = [0u8; 16];
    // SAFETY: a 16-byte `u64 | null` slot.
    unsafe { velt_rt_chan_try_receive(h, slot.as_mut_ptr(), 8, 8) };
    assert_eq!((slot[0], slot[8]), (1, 1));
}
