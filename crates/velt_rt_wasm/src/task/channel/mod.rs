//! `velt_rt_chan_*` on the current-thread executor: `Channel<T>` (std/channel.vlt) with the
//! semantics of velt_rt's task/channel (see there): items are the bytes of Velt values, the
//! receive result is a `T | null`, and a closed, drained channel leaves the handle table. One
//! thread, so a `RefCell` queue whose waiters are all woken on every change and re-check.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use super::all::ResultDropFn;
use super::VeltFut;
use fut::{new_op, Item};

mod fut;

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

#[cfg(test)]
mod tests;
