//! `velt_rt_chan_*`: `Channel<T>` (std/channel.vlt, docs/std/channel.md), a multi-producer,
//! multi-consumer FIFO of Velt values between tasks, bounded (`send` waits while full) or
//! unbounded.
//!
//! The runtime never sees a `T`, only its bytes: `send` moves the value's bytes into the
//! channel (ownership moves with them), `receive` moves an item's bytes into the result slot of
//! its future, laid out as the compiler's `T | null` (see [`write_option`]). The only code
//! pointers kept are drop glue: the `item_drop` of a `send` future that owns the bytes in flight
//! (like `velt_rt_race_ok`'s), and the channel's copy of it (rt_abi_async.md §2.2, §13.5).
//!
//! Handles: `Channel<T>` is a Copy struct (like `TcpStream`, std/net.vlt), so its handle is a
//! [`Registry`] key. A channel leaves the table once it is closed and drained; every copy then
//! sees a closed, empty channel (`send` fails, `receive` gets null) instead of freed memory.
//! Operations in flight keep their own `Arc`. Since any copy may still receive, values queued in
//! a channel nobody drains (closed or not) are only known to be abandoned when the program ends:
//! [`drop_abandoned_items`] drops them then, with the drop glue the channel kept from its sends.

mod fut;
mod queue;
mod ring;

use self::fut::{done_op, new_op, ItemBuf, NOT_SENT, SENT};
use self::queue::{Chan, Pop, Push};
use super::all::ResultDropFn;
use super::{SendPtr, VeltFut};
use crate::registry::{Key, Registry};

static CHANNELS: Registry<Chan> = Registry::new();

/// Drop the table's reference once `chan` is closed and empty (nothing can come out of it).
fn retire_if_done(key: Key<Chan>, chan: &Chan) {
    if chan.is_closed() && chan.len() == 0 {
        CHANNELS.remove(key);
    }
}

/// At program end (`main` returned and the tasks settled): drop the items still queued in every
/// channel, which nobody can receive any more.
pub(crate) fn drop_abandoned_items() {
    for chan in CHANNELS.all() {
        chan.drop_items();
    }
}

/// A new channel; `capacity == 0` is unbounded. Items are passed with their size (align <= 16).
#[no_mangle]
pub extern "C" fn velt_rt_chan_new(capacity: u64) -> Key<Chan> {
    CHANNELS.insert(Chan::new(capacity as usize))
}

/// Closes the channel: `send` fails from now on (pending sends too); receivers get what is
/// queued, then `null`.
#[no_mangle]
pub extern "C" fn velt_rt_chan_close(h: Key<Chan>) {
    if let Some(c) = CHANNELS.get(h) {
        c.close();
        retire_if_done(h, &c);
    }
}

/// Whether the channel is closed.
#[no_mangle]
pub extern "C" fn velt_rt_chan_closed(h: Key<Chan>) -> bool {
    CHANNELS.get(h).is_none_or(|c| c.is_closed())
}

/// Number of queued items.
#[no_mangle]
pub extern "C" fn velt_rt_chan_len(h: Key<Chan>) -> u64 {
    CHANNELS.get(h).map_or(0, |c| c.len() as u64)
}

/// `await ch.send(value)`: moves the `size`-byte item at `src` (the value's ownership moves with
/// them) into a future whose `bool` result is true once the item is queued (after waiting for
/// space in a bounded channel) and false if the channel is closed first. An item that was not
/// queued (closed, or the future dropped while waiting) is dropped with `item_drop` (null:
/// nothing to drop).
///
/// # Safety
/// `src` must hold a `size`-byte item.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_chan_send(
    h: Key<Chan>,
    src: *const u8,
    size: u64,
    item_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    let c = CHANNELS.get(h);
    let size = size as usize;
    // Fast path: queued (or refused) at once, with no allocation.
    let now = c
        .as_ref()
        .map_or(Push::Closed, |c| c.try_push(src, size, item_drop));
    match now {
        Push::Sent => return SENT.ptr(),
        Push::Closed => {
            if let Some(d) = item_drop {
                d(src as *mut u8);
            }
            return NOT_SENT.ptr();
        }
        Push::Full => {}
    }
    let item = ItemBuf::take(src, size, item_drop);
    new_op(1, move |slot| async move {
        // Whole-value captures: the closure would otherwise capture the raw `slot.0` (not Send).
        let (slot, mut item) = (slot, item);
        let sent = match c {
            Some(c) => c.send(item.ptr(), size, item.item_drop()).await,
            None => false,
        };
        if sent {
            item.moved();
        }
        *slot.0 = sent as u8;
    })
}

/// `ch.trySend(value)`: moves the `size`-byte item at `src` (and its ownership) into the
/// channel if it has room now; true if queued. An item that was not queued (the channel is full
/// or closed) is dropped with `item_drop` (null: nothing to drop).
///
/// # Safety
/// `src` must hold a `size`-byte item.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_chan_try_send(
    h: Key<Chan>,
    src: *const u8,
    size: u64,
    item_drop: Option<ResultDropFn>,
) -> bool {
    let now = CHANNELS
        .get(h)
        .map_or(Push::Closed, |c| c.try_push(src, size as usize, item_drop));
    let sent = matches!(now, Push::Sent);
    if !sent {
        if let Some(d) = item_drop {
            d(src as *mut u8);
        }
    }
    sent
}

/// `await ch.receive()`: a future whose result slot gets a `T | null` (see [`write_option`];
/// `T` is `size` bytes): the oldest item, or null once the channel is closed and drained.
///
/// # Safety
/// `payload` and `slot_size` describe `T | null`, with `T` of `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_chan_receive(
    h: Key<Chan>,
    size: u64,
    payload: u64,
    slot_size: u64,
) -> *mut VeltFut {
    let c = CHANNELS.get(h);
    let (size, payload) = (size as usize, payload as usize);
    // Fast path: an item is queued, or the channel is gone or closed and drained.
    let now = match &c {
        Some(c) => {
            let probe = done_op(slot_size as usize, |_| {});
            let slot = (probe as *mut u8).add(super::FUT_RESULT_OFFSET);
            match c.try_pop(slot.add(payload), size) {
                Pop::Empty => {
                    ((*probe).drop)(probe);
                    None
                }
                p => {
                    retire_if_done(h, c);
                    write_option(slot, payload, size, matches!(p, Pop::Item));
                    Some(probe)
                }
            }
        }
        None => Some(done_op(slot_size as usize, |slot| {
            write_option(slot, payload, size, false)
        })),
    };
    if let Some(f) = now {
        return f;
    }
    new_op(slot_size as usize, move |slot| async move {
        let slot = slot;
        let got = match c {
            Some(c) => {
                let got = c.receive(SendPtr(slot.0.add(payload)), size).await;
                retire_if_done(h, &c);
                got
            }
            None => false,
        };
        write_option(slot.0, payload, size, got);
    })
}

/// `ch.tryReceive()`: moves the oldest (`size`-byte) item into the `T | null` at `dst` (see
/// [`write_option`]), or writes null when none is queued.
///
/// # Safety
/// `dst` must hold a `T | null` as `payload` describes, with `T` of `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_chan_try_receive(
    h: Key<Chan>,
    dst: *mut u8,
    size: u64,
    payload: u64,
) {
    let (size, payload) = (size as usize, payload as usize);
    let got = CHANNELS.get(h).is_some_and(|c| {
        let got = matches!(c.try_pop(dst.add(payload), size), Pop::Item);
        retire_if_done(h, &c);
        got
    });
    write_option(dst, payload, size, got);
}

/// Complete the `T | null` at `slot` whose payload (the item) is at offset `payload`: with a
/// nonzero `payload` the value is `{ present: bool @0, item @payload }`; `payload == 0` means a
/// pointer-like `T` whose null is all zero bits (the `size`-byte item *is* the value).
unsafe fn write_option(slot: *mut u8, payload: usize, size: usize, present: bool) {
    if payload == 0 {
        if !present {
            std::ptr::write_bytes(slot, 0, size);
        }
    } else {
        *slot = present as u8;
    }
}

/// `SendPtr` for the result slot an operation writes.
type Slot = SendPtr<u8>;

#[cfg(test)]
mod tests;
