//! The lines of waiting operations, polled by hand with counting wakers: who is woken, in what
//! order, and what an operation dropped while it waits leaves behind.

use super::*;
use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, FUT_RESULT_OFFSET, READY};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Wake, Waker};

struct Count(AtomicUsize);

impl Wake for Count {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// A channel future polled by hand, with a waker that counts its wakes.
struct Hand {
    f: *mut VeltFut,
    count: Arc<Count>,
    waker: Waker,
}

impl Hand {
    fn new(f: *mut VeltFut) -> Hand {
        let count = Arc::new(Count(AtomicUsize::new(0)));
        let waker = Waker::from(count.clone());
        Hand { f, count, waker }
    }

    /// `receive` of a `u64 | null` laid out as `{ present: bool @0, item @8 }`.
    fn receive(h: Key<Chan>) -> Hand {
        // SAFETY: an 8-byte item in a 16-byte slot.
        Hand::new(unsafe { velt_rt_chan_receive(h, 8, 8, 16) })
    }

    fn send(h: Key<Chan>, v: u64) -> Hand {
        // SAFETY: an 8-byte item, no drop glue.
        Hand::new(unsafe { velt_rt_chan_send(h, &v as *const u64 as *const u8, 8, None) })
    }

    /// Polls once: whether it finished.
    fn poll(&self) -> bool {
        let mut cx = Context::from_waker(&self.waker);
        // SAFETY: a live channel future.
        unsafe { velt_rt_fut_poll(self.f, raw_cx(&mut cx).ptr()) == READY }
    }

    fn wakes(&self) -> usize {
        self.count.0.load(Ordering::SeqCst)
    }

    /// The received item (after `poll` returned true).
    fn item(&self) -> Option<u64> {
        // SAFETY: a finished receive's 16-byte result slot: the flag at 0, and the item at 8
        // when it is set (the runtime writes nothing else).
        unsafe {
            let at = (self.f as *const u8).add(FUT_RESULT_OFFSET);
            (*at != 0).then(|| std::ptr::read_unaligned(at.add(8) as *const u64))
        }
    }

    /// The `bool` a finished send wrote.
    fn sent(&self) -> bool {
        // SAFETY: a finished send's result slot.
        unsafe { *(self.f as *const u8).add(FUT_RESULT_OFFSET) != 0 }
    }
}

impl Drop for Hand {
    fn drop(&mut self) {
        // SAFETY: owned; dropped once.
        unsafe { velt_rt_fut_drop(self.f) };
    }
}

fn try_send(h: Key<Chan>, v: u64) -> bool {
    // SAFETY: an 8-byte item, no drop glue.
    unsafe { velt_rt_chan_try_send(h, &v as *const u64 as *const u8, 8, None) }
}

#[test]
fn an_item_wakes_the_first_receiver_in_line_once() {
    let h = velt_rt_chan_new(0);
    let (a, b) = (Hand::receive(h), Hand::receive(h));
    assert!(!a.poll() && !b.poll(), "nothing queued: both wait");
    assert!(try_send(h, 7));
    assert_eq!((a.wakes(), b.wakes()), (1, 0), "the first in line, alone");
    assert!(a.poll());
    assert_eq!(a.item(), Some(7));
    assert!(try_send(h, 8));
    assert_eq!((a.wakes(), b.wakes()), (1, 1), "then the next");
    assert!(b.poll());
    assert_eq!(b.item(), Some(8));
    velt_rt_chan_close(h);
}

#[test]
fn a_receiver_polled_again_before_its_wake_keeps_its_place() {
    let h = velt_rt_chan_new(0);
    let (a, b) = (Hand::receive(h), Hand::receive(h));
    assert!(!a.poll() && !b.poll());
    // Polled again with nothing queued (woken by something else): still first in line.
    assert!(!a.poll());
    assert!(try_send(h, 1));
    assert_eq!((a.wakes(), b.wakes()), (1, 0));
    assert!(a.poll());
    assert_eq!(a.item(), Some(1));
    drop(a);
    // In line once: nothing of `a` is left behind `b` for a later item to wake.
    assert!(try_send(h, 2));
    assert_eq!(b.wakes(), 1);
    assert!(b.poll());
    assert_eq!(b.item(), Some(2));
    let c = Hand::receive(h);
    assert!(!c.poll());
    assert!(try_send(h, 3));
    assert_eq!(c.wakes(), 1, "the next item wakes the receiver waiting now");
    assert!(c.poll());
    assert_eq!(c.item(), Some(3));
    velt_rt_chan_close(h);
}

#[test]
fn close_wakes_every_receiver_with_null() {
    let h = velt_rt_chan_new(0);
    let (a, b) = (Hand::receive(h), Hand::receive(h));
    assert!(!a.poll() && !b.poll());
    velt_rt_chan_close(h);
    assert_eq!((a.wakes(), b.wakes()), (1, 1));
    assert!(a.poll() && b.poll());
    assert_eq!((a.item(), b.item()), (None, None));
}

#[test]
fn a_receiver_dropped_while_waiting_leaves_the_line() {
    let h = velt_rt_chan_new(0);
    let (a, b) = (Hand::receive(h), Hand::receive(h));
    assert!(!a.poll() && !b.poll());
    drop(a);
    assert!(try_send(h, 3));
    assert_eq!(b.wakes(), 1, "the wake goes to the receiver still waiting");
    assert!(b.poll());
    assert_eq!(b.item(), Some(3));
    velt_rt_chan_close(h);
}

#[test]
fn a_woken_receiver_dropped_unpolled_hands_its_wake_on() {
    let h = velt_rt_chan_new(0);
    let (a, b) = (Hand::receive(h), Hand::receive(h));
    assert!(!a.poll() && !b.poll());
    assert!(try_send(h, 5));
    assert_eq!((a.wakes(), b.wakes()), (1, 0));
    // Cancelled after its wake, before it took the item: the item must not wait unseen.
    drop(a);
    assert_eq!(b.wakes(), 1, "the next receiver gets the wake");
    assert!(b.poll());
    assert_eq!(b.item(), Some(5));
    velt_rt_chan_close(h);
}

#[test]
fn a_woken_receiver_whose_item_was_taken_waits_again() {
    let h = velt_rt_chan_new(0);
    let a = Hand::receive(h);
    assert!(!a.poll());
    assert!(try_send(h, 9));
    assert_eq!(a.wakes(), 1);
    assert_eq!(
        velt_rt_chan_len(h),
        1,
        "queued until the woken receive takes it"
    );
    let mut slot = [0u8; 16];
    // SAFETY: a 16-byte `u64 | null` slot. Takes the item before the woken receiver runs.
    unsafe { velt_rt_chan_try_receive(h, slot.as_mut_ptr(), 8, 8) };
    assert_eq!(slot[0], 1);
    assert!(!a.poll(), "nothing left: waits again");
    assert!(try_send(h, 10));
    assert_eq!(a.wakes(), 2, "and is woken by the next item");
    assert!(a.poll());
    assert_eq!(a.item(), Some(10));
    velt_rt_chan_close(h);
}

#[test]
fn room_wakes_the_first_sender_in_line_and_a_dropped_one_hands_it_on() {
    let h = velt_rt_chan_new(1);
    assert!(try_send(h, 0));
    let (a, b, c) = (Hand::send(h, 1), Hand::send(h, 2), Hand::send(h, 3));
    assert!(!a.poll() && !b.poll() && !c.poll(), "full: all wait");
    let r = Hand::receive(h);
    assert!(r.poll());
    assert_eq!(r.item(), Some(0));
    assert_eq!(
        (a.wakes(), b.wakes(), c.wakes()),
        (1, 0, 0),
        "room: the first sender"
    );
    // Cancelled after its wake: the room goes to the next sender.
    drop(a);
    assert_eq!((b.wakes(), c.wakes()), (1, 0));
    assert!(b.poll() && b.sent());
    // Another dropped while still in line: the next wake skips it.
    drop(c);
    let r = Hand::receive(h);
    assert!(r.poll());
    assert_eq!(r.item(), Some(2), "the cancelled sends queued nothing");
    assert_eq!(velt_rt_chan_len(h), 0);
    velt_rt_chan_close(h);
}

#[test]
fn close_ends_waiting_sends_with_false() {
    let h = velt_rt_chan_new(1);
    assert!(try_send(h, 0));
    let a = Hand::send(h, 1);
    assert!(!a.poll());
    velt_rt_chan_close(h);
    assert_eq!(a.wakes(), 1);
    assert!(a.poll());
    assert!(!a.sent());
}

#[test]
fn a_receive_takes_a_queued_item_when_it_is_made() {
    let h = velt_rt_chan_new(0);
    assert!(try_send(h, 1));
    let a = Hand::receive(h);
    let mut slot = [0u8; 16];
    // SAFETY: a 16-byte `u64 | null` slot. The receive was made but not polled yet.
    unsafe { velt_rt_chan_try_receive(h, slot.as_mut_ptr(), 8, 8) };
    assert_eq!(slot[0], 0, "the item went to the receive made first");
    assert!(try_send(h, 2));
    assert!(a.poll());
    assert_eq!(a.item(), Some(1));
    assert_eq!(velt_rt_chan_len(h), 1);
    velt_rt_chan_close(h);
}

/// Drop glue that records the ids it drops (each test uses ids of its own).
static GLUE_DROPPED: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

unsafe extern "C-unwind" fn record_glue(item: *mut u8) {
    GLUE_DROPPED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(*(item as *const u64));
}

fn glue_dropped(id: u64) -> usize {
    let d = GLUE_DROPPED.lock().unwrap_or_else(|e| e.into_inner());
    d.iter().filter(|&&x| x == id).count()
}

#[test]
fn a_waiting_send_cancelled_drops_its_item_once() {
    let h = velt_rt_chan_new(1);
    assert!(try_send(h, 0));
    let id = 7001u64;
    // SAFETY: an 8-byte item with drop glue.
    let a = Hand::new(unsafe {
        velt_rt_chan_send(h, &id as *const u64 as *const u8, 8, Some(record_glue))
    });
    assert!(!a.poll(), "full: waits");
    drop(a);
    assert_eq!(glue_dropped(id), 1, "the item it held is dropped, once");
    assert_eq!(velt_rt_chan_len(h), 1, "and was never queued");
    velt_rt_chan_close(h);
    assert_eq!(glue_dropped(id), 1);
}

#[test]
fn a_receive_dropped_unpolled_leaves_the_others_waiting() {
    let h = velt_rt_chan_new(0);
    let a = Hand::receive(h);
    assert!(!a.poll());
    // Made but never polled: never in line.
    drop(Hand::receive(h));
    let c = Hand::receive(h);
    assert!(!c.poll());
    drop(Hand::receive(h));
    assert!(try_send(h, 1));
    assert!(try_send(h, 2));
    assert_eq!(
        (a.wakes(), c.wakes()),
        (1, 1),
        "both waiters still in line, in order"
    );
    assert!(a.poll() && c.poll());
    assert_eq!((a.item(), c.item()), (Some(1), Some(2)));
    velt_rt_chan_close(h);
}

/// A waker whose last reference locks the channel when it goes (as a task's state owning an
/// operation on the same channel does).
struct Reenter(Key<Chan>);

// Its wake does nothing: what matters is its drop.
#[allow(clippy::manual_noop_waker)]
impl Wake for Reenter {
    fn wake(self: Arc<Self>) {}
}

impl Drop for Reenter {
    fn drop(&mut self) {
        velt_rt_chan_len(self.0);
    }
}

#[test]
fn a_replaced_waker_is_dropped_after_the_lock_is_released() {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let h = velt_rt_chan_new(0);
        let a = Hand::receive(h);
        // Polled with a waker only the line keeps, then with another: the first one goes.
        let first = Waker::from(Arc::new(Reenter(h)));
        let mut cx = Context::from_waker(&first);
        // SAFETY: a live channel future.
        assert_eq!(unsafe { velt_rt_fut_poll(a.f, raw_cx(&mut cx).ptr()) }, 0);
        drop(first);
        assert!(!a.poll(), "replaces the waker: no deadlock");
        // The same for a send waiting for room.
        let b = velt_rt_chan_new(1);
        assert!(try_send(b, 0));
        let s = Hand::send(b, 1);
        let first = Waker::from(Arc::new(Reenter(b)));
        let mut cx = Context::from_waker(&first);
        // SAFETY: as above.
        assert_eq!(unsafe { velt_rt_fut_poll(s.f, raw_cx(&mut cx).ptr()) }, 0);
        drop(first);
        assert!(!s.poll());
        velt_rt_chan_close(h);
        velt_rt_chan_close(b);
        let _ = done_tx.send(());
    });
    assert!(
        done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok(),
        "deadlocked: a waker dropped under the channel's lock"
    );
}
