//! `velt_rt_chan_*` on the current-thread executor: `Channel<T>` (std/channel.vlt) with the
//! semantics of velt_rt's task/channel (see there): items are the bytes of Velt values, the
//! receive result is a `T | null`, and a closed, drained channel leaves the handle table. One
//! thread, so a `RefCell` queue whose waiters are all woken on every change and re-check.

use std::alloc::Layout;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use super::all::ResultDropFn;
use super::{context, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};

struct State {
    bytes: VecDeque<u8>,
    len: usize,
    closed: bool,
    waiters: Vec<Waker>,
}

struct Chan {
    /// 0 = unbounded.
    capacity: usize,
    state: RefCell<State>,
}

impl Chan {
    fn wake_all(&self) {
        let ws = std::mem::take(&mut self.state.borrow_mut().waiters);
        for w in ws {
            w.wake();
        }
    }

    fn wait(&self, cx: &mut Context<'_>) {
        self.state.borrow_mut().waiters.push(cx.waker().clone());
    }

    /// `Some(sent)` when done (false: closed), `None` when full.
    unsafe fn try_push(&self, src: *const u8, size: usize) -> Option<bool> {
        let mut s = self.state.borrow_mut();
        if s.closed {
            return Some(false);
        }
        if self.capacity != 0 && s.len >= self.capacity {
            return None;
        }
        s.bytes
            .extend(std::slice::from_raw_parts(src, size).iter().copied());
        s.len += 1;
        drop(s);
        self.wake_all();
        Some(true)
    }

    /// `Some(got)` when done (false: closed and empty), `None` when empty.
    unsafe fn try_pop(&self, dst: *mut u8, size: usize) -> Option<bool> {
        let mut s = self.state.borrow_mut();
        if s.len == 0 {
            return if s.closed { Some(false) } else { None };
        }
        for (i, b) in s.bytes.drain(..size).enumerate() {
            *dst.add(i) = b;
        }
        s.len -= 1;
        drop(s);
        self.wake_all();
        Some(true)
    }

    fn done(&self) -> bool {
        let s = self.state.borrow();
        s.closed && s.len == 0
    }
}

thread_local! {
    /// Handle `k` is slot `k - 1`; slots are never reused, so a stale handle finds `None`.
    static CHANNELS: RefCell<Vec<Option<Rc<Chan>>>> = const { RefCell::new(Vec::new()) };
}

fn get(h: u64) -> Option<Rc<Chan>> {
    let i = (h as usize).checked_sub(1)?;
    CHANNELS.with(|t| t.borrow().get(i).cloned().flatten())
}

fn retire_if_done(h: u64, c: &Chan) {
    if c.done() {
        CHANNELS.with(|t| {
            if let Some(slot) = t.borrow_mut().get_mut(h as usize - 1) {
                *slot = None;
            }
        });
    }
}

#[no_mangle]
pub extern "C" fn velt_rt_chan_new(capacity: u64) -> u64 {
    let c = Rc::new(Chan {
        capacity: capacity as usize,
        state: RefCell::new(State {
            bytes: VecDeque::new(),
            len: 0,
            closed: false,
            waiters: Vec::new(),
        }),
    });
    CHANNELS.with(|t| {
        let mut t = t.borrow_mut();
        t.push(Some(c));
        t.len() as u64
    })
}

#[no_mangle]
pub extern "C" fn velt_rt_chan_close(h: u64) {
    if let Some(c) = get(h) {
        c.state.borrow_mut().closed = true;
        c.wake_all();
        retire_if_done(h, &c);
    }
}

#[no_mangle]
pub extern "C" fn velt_rt_chan_closed(h: u64) -> bool {
    get(h).is_none_or(|c| c.state.borrow().closed)
}

#[no_mangle]
pub extern "C" fn velt_rt_chan_len(h: u64) -> u64 {
    get(h).map_or(0, |c| c.state.borrow().len as u64)
}

/// `await ch.send(value)` (see velt_rt): moves the `size`-byte item at `src` into the future;
/// `bool` result; an item that was not queued is dropped with `item_drop`.
///
/// # Safety
/// `src` must hold a `size`-byte item.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_chan_send(
    h: u64,
    src: *const u8,
    size: u64,
    item_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    let c = get(h);
    let size = size as usize;
    let mut item = Some(Item::take(src, size, item_drop));
    new_op(1, move |cx, slot| {
        let sent = match &c {
            Some(c) => {
                let it = item
                    .as_ref()
                    .map_or(std::ptr::null(), |i| i.ptr as *const u8);
                match c.try_push(it, size) {
                    Some(sent) => sent,
                    None => {
                        c.wait(cx);
                        return Poll::Pending;
                    }
                }
            }
            None => false,
        };
        if let (true, Some(i)) = (sent, item.as_mut()) {
            i.owned = false;
        }
        item = None;
        *slot = sent as u8;
        Poll::Ready(())
    })
}

/// `ch.trySend(value)` (see velt_rt): true if the item was queued now; otherwise it is dropped
/// with `item_drop`.
///
/// # Safety
/// `src` must hold a `size`-byte item.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_chan_try_send(
    h: u64,
    src: *const u8,
    size: u64,
    item_drop: Option<ResultDropFn>,
) -> bool {
    let sent = get(h).is_some_and(|c| c.try_push(src, size as usize) == Some(true));
    if !sent {
        if let Some(d) = item_drop {
            d(src as *mut u8);
        }
    }
    sent
}

/// `await ch.receive()` (see velt_rt): the result slot gets a `T | null`.
///
/// # Safety
/// `payload` and `slot_size` describe `T | null`, with `T` of `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_chan_receive(
    h: u64,
    size: u64,
    payload: u64,
    slot_size: u64,
) -> *mut VeltFut {
    let c = get(h);
    let (size, payload) = (size as usize, payload as usize);
    new_op(slot_size as usize, move |cx, slot| {
        let got = match &c {
            Some(c) => match c.try_pop(slot.add(payload), size) {
                Some(got) => {
                    retire_if_done(h, c);
                    got
                }
                None => {
                    c.wait(cx);
                    return Poll::Pending;
                }
            },
            None => false,
        };
        write_option(slot, payload, size, got);
        Poll::Ready(())
    })
}

/// `ch.tryReceive()` (see velt_rt).
///
/// # Safety
/// `dst` must hold a `T | null` as `payload` describes, with `T` of `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_chan_try_receive(h: u64, dst: *mut u8, size: u64, payload: u64) {
    let (size, payload) = (size as usize, payload as usize);
    let got = get(h).is_some_and(|c| {
        let got = c.try_pop(dst.add(payload), size) == Some(true);
        retire_if_done(h, &c);
        got
    });
    write_option(dst, payload, size, got);
}

/// See velt_rt's `write_option`: flag at 0 and payload at `payload`, or (`payload == 0`) a
/// pointer-like item whose null is all zero bits.
unsafe fn write_option(slot: *mut u8, payload: usize, size: usize, present: bool) {
    if payload == 0 {
        if !present {
            std::ptr::write_bytes(slot, 0, size);
        }
    } else {
        *slot = present as u8;
    }
}

/// An item's bytes (16-aligned) owned by a pending send; dropped with `item_drop` unless queued.
struct Item {
    ptr: *mut u8,
    size: usize,
    item_drop: Option<ResultDropFn>,
    owned: bool,
}

impl Item {
    unsafe fn take(src: *const u8, size: usize, item_drop: Option<ResultDropFn>) -> Item {
        let p = std::alloc::alloc(Self::layout(size));
        if p.is_null() {
            std::alloc::handle_alloc_error(Self::layout(size));
        }
        std::ptr::copy_nonoverlapping(src, p, size);
        Item {
            ptr: p,
            size,
            item_drop,
            owned: true,
        }
    }

    fn layout(size: usize) -> Layout {
        Layout::from_size_align(size.max(1), 16)
            .unwrap_or_else(|_| crate::panic::fatal("invalid channel item size"))
    }
}

impl Drop for Item {
    fn drop(&mut self) {
        if let (true, Some(d)) = (self.owned, self.item_drop) {
            // SAFETY: the buffer still owns a valid item.
            unsafe { d(self.ptr) };
        }
        // SAFETY: allocated in `take` with this layout.
        unsafe { std::alloc::dealloc(self.ptr, Self::layout(self.size)) };
    }
}

type Op = Box<dyn FnMut(&mut Context<'_>, *mut u8) -> Poll<()>>;

/// `[Tail][VeltFut hdr][result (slot_size)]`, align 16 (velt_rt's channel/fut.rs).
#[repr(C, align(16))]
struct Tail {
    op: Option<Op>,
    slot_size: usize,
}

const TAIL: usize = std::mem::size_of::<Tail>();

fn op_layout(slot_size: usize) -> Layout {
    Layout::from_size_align(TAIL + std::mem::size_of::<VeltFut>() + slot_size, 16)
        .unwrap_or_else(|_| crate::panic::fatal("invalid channel result size"))
}

unsafe fn tail<'a>(f: *mut VeltFut) -> &'a mut Tail {
    &mut *((f as *mut u8).sub(TAIL) as *mut Tail)
}

fn new_op(
    slot_size: usize,
    op: impl FnMut(&mut Context<'_>, *mut u8) -> Poll<()> + 'static,
) -> *mut VeltFut {
    let l = op_layout(slot_size);
    // SAFETY: a fresh allocation of `l`, initialized before it is handed out.
    unsafe {
        let base = std::alloc::alloc(l);
        if base.is_null() {
            std::alloc::handle_alloc_error(l);
        }
        (base as *mut Tail).write(Tail {
            op: Some(Box::new(op)),
            slot_size,
        });
        let f = base.add(TAIL) as *mut VeltFut;
        f.write(VeltFut::new(op_poll, op_drop));
        f
    }
}

unsafe extern "C" fn op_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let t = tail(f);
    let Some(op) = t.op.as_mut() else {
        return READY;
    };
    let slot = (f as *mut u8).add(FUT_RESULT_OFFSET);
    match op(context(cx), slot) {
        Poll::Ready(()) => {
            t.op = None;
            READY
        }
        Poll::Pending => PENDING,
    }
}

unsafe extern "C" fn op_drop(f: *mut VeltFut) {
    let t = tail(f);
    let l = op_layout(t.slot_size);
    std::ptr::drop_in_place(t);
    std::alloc::dealloc((f as *mut u8).sub(TAIL), l);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll};
    use std::cell::Cell;
    use std::task::Waker;

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
}
