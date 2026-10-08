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

#[test]
fn hand_offs_publish_this_threads_output_first() {
    use crate::io::handoff_probe::{buffer_output, published};
    let bounded = queue::Chan::new(1);
    let v = 7u64;
    let mut out = 0u64;
    // SAFETY: 8-byte items, read from and written to live locals.
    unsafe {
        buffer_output();
        assert!(matches!(
            bounded.try_push(&v as *const u64 as *const u8, 8, None),
            queue::Push::Sent
        ));
        assert!(published(), "send");
        buffer_output();
        assert!(matches!(
            bounded.try_pop(&mut out as *mut u64 as *mut u8, 8),
            queue::Pop::Item
        ));
        assert!(published(), "a receive that frees room");
    }
    buffer_output();
    bounded.close();
    assert!(published(), "close");

    // A receive from an unbounded channel hands nothing over.
    let unbounded = queue::Chan::new(0);
    // SAFETY: as above.
    unsafe {
        unbounded.try_push(&v as *const u64 as *const u8, 8, None);
        buffer_output();
        unbounded.try_pop(&mut out as *mut u64 as *mut u8, 8);
    }
    assert!(!published());
    crate::io::publish_before_handoff();
}

/// A 37-byte item (odd size) carrying `n` in every byte.
fn big_item(n: u8) -> [u8; 37] {
    [n; 37]
}

#[test]
fn odd_sized_items_cross_a_bounded_channel_whole_and_in_order() {
    let h = velt_rt_chan_new(3);
    let hb = h.bits();
    run(async move {
        let consumer = tokio::spawn(async move {
            let h = Key::<Chan>::from_bits(hb);
            let mut got = vec![];
            loop {
                // `[u8; 37] | null`: present flag @0, item @1.
                // SAFETY: `h` is live until closed and drained.
                let f = Await::<38>(SendPtr(unsafe { velt_rt_chan_receive(h, 37, 1, 38) }));
                let b = f.await;
                if b[0] == 0 {
                    break got;
                }
                assert_eq!(b[1..], big_item(b[1]), "a whole item");
                got.push(b[1]);
            }
        });
        let h = Key::<Chan>::from_bits(hb);
        for n in 0..200u8 {
            let v = big_item(n);
            // SAFETY: a 37-byte item; no drop glue.
            let f = Await::<1>(SendPtr(unsafe {
                velt_rt_chan_send(h, v.as_ptr(), 37, None)
            }));
            assert_eq!(f.await, [1]);
            assert!(velt_rt_chan_len(h) <= 3, "never more than the capacity");
        }
        velt_rt_chan_close(h);
        let got = consumer.await.expect("consumer");
        assert_eq!(got, (0..200u8).collect::<Vec<_>>());
        assert!(CHANNELS.get(h).is_none(), "closed and drained");
    });
}

#[test]
fn items_queued_before_close_are_still_received_whole() {
    let h = velt_rt_chan_new(0);
    for n in 1..=9u8 {
        let v = big_item(n);
        // SAFETY: a 37-byte item; no drop glue.
        assert!(unsafe { velt_rt_chan_try_send(h, v.as_ptr(), 37, None) });
    }
    velt_rt_chan_close(h);
    for n in 1..=9u8 {
        let mut slot = [0u8; 38];
        // SAFETY: a 38-byte `[u8; 37] | null` slot, item @1.
        unsafe { velt_rt_chan_try_receive(h, slot.as_mut_ptr(), 37, 1) };
        assert_eq!((slot[0], &slot[1..]), (1, &big_item(n)[..]));
    }
    let mut slot = [0xffu8; 38];
    // SAFETY: as above.
    unsafe { velt_rt_chan_try_receive(h, slot.as_mut_ptr(), 37, 1) };
    assert_eq!(slot[0], 0, "drained: null");
    assert!(CHANNELS.get(h).is_none());
}

/// The ids of the items `record_drop` dropped (the leftover tests use ids of their own).
static LEFTOVERS_DROPPED: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

unsafe extern "C" fn record_drop(item: *mut u8) {
    assert_eq!(item as usize % 16, 0, "drop glue gets an aligned item");
    let id = *(item as *const u64);
    LEFTOVERS_DROPPED.lock().unwrap().push(id);
}

fn dropped_in(range: std::ops::Range<u64>) -> Vec<u64> {
    let mut ids: Vec<u64> = LEFTOVERS_DROPPED
        .lock()
        .unwrap()
        .iter()
        .copied()
        .filter(|id| range.contains(id))
        .collect();
    ids.sort_unstable();
    ids
}

/// A 24-byte item (not a multiple of 16, so ring slots are unaligned) whose first word is `id`,
/// 16-aligned like the values generated code sends.
#[repr(C, align(16))]
struct IdItem([u64; 3]);

impl IdItem {
    fn as_ptr(&self) -> *const u64 {
        self.0.as_ptr()
    }
}

fn item_with_id(id: u64) -> IdItem {
    IdItem([id, !id, id ^ 0x5555])
}

#[test]
fn leftover_items_are_dropped_once_with_the_kept_drop_glue() {
    let h = velt_rt_chan_new(0);
    let hb = h.bits();
    run(async move {
        let h = Key::<Chan>::from_bits(hb);
        for id in 1000..1010u64 {
            let v = item_with_id(id);
            let sent = if id % 2 == 0 {
                // SAFETY: a 24-byte item with drop glue.
                unsafe { velt_rt_chan_try_send(h, v.as_ptr() as *const u8, 24, Some(record_drop)) }
            } else {
                // SAFETY: as above, through the `send` future.
                let f =
                    unsafe { velt_rt_chan_send(h, v.as_ptr() as *const u8, 24, Some(record_drop)) };
                Await::<1>(SendPtr(f)).await == [1]
            };
            assert!(sent);
        }
        // Receive three; the receiver owns them now (the runtime must not drop them).
        for id in 1000..1003u64 {
            let mut slot = [0u64; 4];
            // SAFETY: a 32-byte `[u64; 3] | null` slot, item @8.
            unsafe { velt_rt_chan_try_receive(h, slot.as_mut_ptr() as *mut u8, 24, 8) };
            assert_eq!((slot[0] & 0xff, slot[1]), (1, id));
        }
        velt_rt_chan_close(h);
        let v = item_with_id(1010);
        // SAFETY: as above; refused (closed), so dropped at once.
        assert!(!unsafe {
            velt_rt_chan_try_send(h, v.as_ptr() as *const u8, 24, Some(record_drop))
        });
        assert_eq!(
            dropped_in(1000..1100),
            [1010],
            "only the refused item so far"
        );
        let chan = CHANNELS.get(h).expect("closed but not drained: still open");
        chan.drop_items();
        assert_eq!(
            dropped_in(1000..1100),
            (1003..=1010).collect::<Vec<_>>(),
            "each leftover item dropped exactly once"
        );
        assert_eq!(velt_rt_chan_len(h), 0);
        chan.drop_items();
        assert_eq!(dropped_in(1000..1100).len(), 8, "nothing dropped twice");
        let mut slot = [0xffu64; 4];
        // SAFETY: as above.
        unsafe { velt_rt_chan_try_receive(h, slot.as_mut_ptr() as *mut u8, 24, 8) };
        assert_eq!(slot[0] & 0xff, 0, "drained: null");
    });
}

#[test]
fn leftover_items_without_drop_glue_are_just_discarded() {
    let h = velt_rt_chan_new(0);
    for id in 2000..2005u64 {
        let v = item_with_id(id);
        // SAFETY: a 24-byte Copy item: no drop glue.
        assert!(unsafe { velt_rt_chan_try_send(h, v.as_ptr() as *const u8, 24, None) });
    }
    let chan = CHANNELS.get(h).expect("open");
    chan.drop_items();
    assert_eq!(velt_rt_chan_len(h), 0);
    assert!(dropped_in(2000..2100).is_empty(), "no drop glue was called");
    // Still usable afterwards, with items of the same size.
    let v = item_with_id(2005);
    // SAFETY: as above.
    assert!(unsafe { velt_rt_chan_try_send(h, v.as_ptr() as *const u8, 24, None) });
    assert_eq!(velt_rt_chan_len(h), 1);
    velt_rt_chan_close(h);
}

#[test]
fn every_open_channel_is_listed_for_the_exit_drain() {
    let a = velt_rt_chan_new(0);
    let b = velt_rt_chan_new(4);
    let all = CHANNELS.all();
    for h in [a, b] {
        let c = CHANNELS.get(h).expect("open");
        assert!(all.iter().any(|x| std::sync::Arc::ptr_eq(x, &c)));
    }
    velt_rt_chan_close(a);
    velt_rt_chan_close(b);
}
