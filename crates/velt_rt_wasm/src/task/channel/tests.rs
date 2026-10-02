//! Unit tests of the channel operations on the host.

use super::*;
use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, FUT_RESULT_OFFSET, PENDING, READY};
use std::cell::Cell;

thread_local! {
    static DROPPED: Cell<u64> = const { Cell::new(0) };
}

unsafe extern "C" fn count_drop(item: *mut u8) {
    DROPPED.with(|d| d.set(d.get() + *(item as *const u64)));
}

fn poll(f: *mut VeltFut) -> u32 {
    let mut cx = Context::from_waker(Waker::noop());
    // SAFETY: a live channel future.
    unsafe { velt_rt_fut_poll(f, raw_cx(&mut cx)) }
}

/// The `bool` result of a finished send (freed).
fn sent(f: *mut VeltFut) -> bool {
    assert_eq!(poll(f), READY);
    // SAFETY: a finished send future with its result at +16.
    unsafe {
        let ok = *(f as *const u8).add(FUT_RESULT_OFFSET) != 0;
        velt_rt_fut_drop(f);
        ok
    }
}

fn send(h: u64, v: u64) -> *mut VeltFut {
    // SAFETY: an 8-byte item.
    unsafe { velt_rt_chan_send(h, &v as *const u64 as *const u8, 8, Some(count_drop)) }
}

/// `u64 | null` laid out as `{ present: bool @0, item @8 }`.
fn receive(h: u64) -> *mut VeltFut {
    // SAFETY: payload and slot size describe the layout above.
    unsafe { velt_rt_chan_receive(h, 8, 8, 16) }
}

fn received(f: *mut VeltFut) -> Option<u64> {
    assert_eq!(poll(f), READY);
    // SAFETY: a finished receive future with a 16-byte result at +16.
    unsafe {
        let slot = (f as *const u8).add(FUT_RESULT_OFFSET);
        let v = (*slot != 0).then(|| *(slot.add(8) as *const u64));
        velt_rt_fut_drop(f);
        v
    }
}

#[test]
fn items_arrive_in_order_and_close_drains_then_ends() {
    let h = velt_rt_chan_new(0);
    for v in [1, 2, 3] {
        assert!(sent(send(h, v)));
    }
    velt_rt_chan_close(h);
    assert!(!sent(send(h, 4)), "send on a closed channel fails");
    let got: Vec<u64> = std::iter::from_fn(|| received(receive(h))).collect();
    assert_eq!(got, [1, 2, 3]);
    assert!(get(h).is_none(), "closed and drained: out of the table");
}

#[test]
fn a_full_bounded_channel_makes_send_wait() {
    let h = velt_rt_chan_new(1);
    assert!(sent(send(h, 1)));
    let waiting = send(h, 2);
    assert_eq!(poll(waiting), PENDING);
    assert_eq!(received(receive(h)), Some(1));
    assert!(sent(waiting));
    assert_eq!(received(receive(h)), Some(2));
    let empty = receive(h);
    assert_eq!(poll(empty), PENDING);
    velt_rt_chan_close(h);
    assert_eq!(received(empty), None);
}

#[test]
fn a_refused_or_cancelled_send_drops_its_item() {
    DROPPED.with(|d| d.set(0));
    let h = velt_rt_chan_new(1);
    assert!(sent(send(h, 1)));
    let cancelled = send(h, 10);
    assert_eq!(poll(cancelled), PENDING);
    // SAFETY: an owned, unfinished send future.
    unsafe { velt_rt_fut_drop(cancelled) };
    velt_rt_chan_close(h);
    assert!(!sent(send(h, 100)));
    assert_eq!(
        DROPPED.with(Cell::get),
        110,
        "the cancelled and the refused item"
    );
    let mut slot = [0xffu8; 16];
    // SAFETY: a 16-byte `u64 | null` slot.
    unsafe { velt_rt_chan_try_receive(h, slot.as_mut_ptr(), 8, 8) };
    assert_eq!(
        (
            slot[0],
            u64::from_le_bytes(slot[8..].try_into().unwrap_or_default())
        ),
        (1, 1)
    );
}

#[test]
fn try_send_queues_only_with_room_and_drops_what_it_refuses() {
    DROPPED.with(|d| d.set(0));
    let h = velt_rt_chan_new(1);
    let try_send = |v: u64| {
        // SAFETY: an 8-byte item.
        unsafe { velt_rt_chan_try_send(h, &v as *const u64 as *const u8, 8, Some(count_drop)) }
    };
    assert!(try_send(1));
    assert!(!try_send(10), "full");
    velt_rt_chan_close(h);
    assert!(!try_send(100), "closed");
    assert_eq!(DROPPED.with(Cell::get), 110);
    assert_eq!(received(receive(h)), Some(1));
}
