//! The timer order of `setTimeout` / `setInterval` / `setImmediate` (velt_rt's
//! task/local/order.rs), Node's rule: one list per delay, in insertion order, the lists run by
//! (expiry, list id). A timers pass ([`fire_list`], called by the executor with the `now` it read
//! once) fires, list by list, every timer due by `now`; a list whose next timer is not due yet
//! waits for it with a new id. One thread runs everything here: firing a timer wakes its task at
//! the back of the ready queue, so the callbacks run in firing order, each after the microtasks
//! before it.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::{Rc, Weak};
use std::task::{Context, Poll, Waker};

use super::executor;
use super::leaf::new_leaf;
use super::VeltFut;

/// Whether a timer fired (or was cleared), and whom firing wakes.
#[derive(Default)]
struct Slot {
    fired: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}

impl Slot {
    fn fire(&self) {
        self.fired.set(true);
        if let Some(w) = self.waker.borrow_mut().take() {
            executor::wake_last(w);
        }
    }
}

/// A queued timer: when it was inserted, and its slot.
struct Entry {
    start: u64,
    slot: Weak<Slot>,
}

struct List {
    key: (u64, u64),
    timers: BTreeMap<u64, Entry>,
}

#[derive(Default)]
struct Lists {
    by_ms: BTreeMap<u64, List>,
    /// The lists by (expiry, id), with their delay.
    order: BTreeSet<((u64, u64), u64)>,
    seq: u64,
    ids: u64,
}

thread_local! {
    static LISTS: RefCell<Lists> = RefCell::new(Lists::default());
}

/// The clock in whole milliseconds, one more: a timer is due once the clock (in whole
/// milliseconds) reached its start plus its delay, so at least the delay after it was created.
fn start_now() -> u64 {
    crate::platform::monotonic_ms().floor() as u64 + 1
}

impl Lists {
    fn insert(&mut self, ms: u64, start: u64, slot: &Rc<Slot>) -> (u64, u64) {
        let ids = &mut self.ids;
        let order = &mut self.order;
        let list = self.by_ms.entry(ms).or_insert_with(|| {
            *ids += 1;
            let key = (start.saturating_add(ms), *ids);
            order.insert((key, ms));
            List {
                key,
                timers: BTreeMap::new(),
            }
        });
        self.seq += 1;
        list.timers.insert(
            self.seq,
            Entry {
                start,
                slot: Rc::downgrade(slot),
            },
        );
        (ms, self.seq)
    }

    fn remove(&mut self, (ms, seq): (u64, u64)) {
        let Some(list) = self.by_ms.get_mut(&ms) else {
            return;
        };
        if list.timers.remove(&seq).is_some() && list.timers.is_empty() {
            self.order.remove(&(list.key, ms));
            self.by_ms.remove(&ms);
        }
    }
}

/// The expiry of the first list (whole milliseconds), if any.
pub(super) fn first_expiry() -> Option<f64> {
    LISTS.with(|l| l.borrow().order.first().map(|((e, _), _)| *e as f64))
}

/// One step of a timers pass at `now`: fires the due timers of the first list (in insertion
/// order), or gives it a new place if its next timer is not due yet. False when no list is due.
pub(super) fn fire_list(now: f64) -> bool {
    let now = now.floor() as u64;
    let mut fired = Vec::new();
    let any = LISTS.with(|l| {
        let mut l = l.borrow_mut();
        let l = &mut *l;
        let Some(&((expiry, _), ms)) = l.order.first() else {
            return false;
        };
        if expiry > now {
            return false;
        }
        let list = l
            .by_ms
            .get_mut(&ms)
            .expect("ICE: a timer list in the order");
        while let Some(e) = list.timers.first_entry() {
            let due = e.get().start.saturating_add(ms);
            if due > now {
                l.ids += 1;
                let key = (due.max(now + 1), l.ids);
                l.order.remove(&(list.key, ms));
                list.key = key;
                l.order.insert((key, ms));
                return true;
            }
            fired.push(e.remove().slot);
        }
        let key = list.key;
        l.by_ms.remove(&ms);
        l.order.remove(&(key, ms));
        true
    });
    // Woken outside the borrow: a waker may run nothing here, but stays clear of the lists.
    for s in fired.into_iter().filter_map(|s| s.upgrade()) {
        s.fire();
    }
    any
}

/// A timer's place in the order and its slot (replaced for an interval's next run).
struct Handle {
    key: Option<(u64, u64)>,
    slot: Rc<Slot>,
    /// The clock when its turn last completed: an interval's next run counts from there.
    ran: Option<u64>,
}

fn queue(ms: i64, start: u64) -> (Option<(u64, u64)>, Rc<Slot>) {
    let slot = Rc::new(Slot::default());
    let key = LISTS.with(|l| l.borrow_mut().insert(ms.max(0) as u64, start, &slot));
    (Some(key), slot)
}

/// `velt_rt_timer_new`: a timer due `ms` milliseconds from now.
#[no_mangle]
pub extern "C" fn velt_rt_timer_new(ms: i64) -> u64 {
    let (key, slot) = queue(ms, start_now());
    Box::into_raw(Box::new(Handle {
        key,
        slot,
        ran: None,
    })) as u64
}

/// `velt_rt_timer_again`: the timer is due again `ms` milliseconds after its last turn.
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`].
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_again(h: u64, ms: i64) {
    let h = &mut *(h as *mut Handle);
    if let Some(k) = h.key.take() {
        LISTS.with(|l| l.borrow_mut().remove(k));
    }
    let (key, slot) = queue(ms, h.ran.unwrap_or_else(start_now));
    h.key = key;
    h.slot = slot;
}

/// `velt_rt_timer_clear`: out of the order; its turn completes at once.
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`].
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_clear(h: u64) {
    let h = &mut *(h as *mut Handle);
    if let Some(k) = h.key.take() {
        LISTS.with(|l| l.borrow_mut().remove(k));
        h.slot.fire();
    }
}

/// `velt_rt_timer_free`: out of the order; frees the handle.
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`], not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_free(h: u64) {
    let h = Box::from_raw(h as *mut Handle);
    if let Some(k) = h.key {
        LISTS.with(|l| l.borrow_mut().remove(k));
    }
}

/// `velt_rt_timer_turn`: ready once a timers pass fired the timer (no gate is needed on one
/// thread: velt_rt_task_gate).
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`] that outlives the future.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_turn(h: u64, _gate: u64) -> *mut VeltFut {
    super::local::mark_starting_timer();
    let h = h as *mut Handle;
    let slot = (*h).slot.clone();
    new_leaf(move |cx: &mut Context<'_>| {
        if slot.fired.get() {
            // SAFETY: the handle outlives the turn.
            let h = unsafe { &mut *h };
            // Fired: out of the order. Node reads the clock just before an interval's callback.
            h.key = None;
            h.ran = Some(start_now());
            return Poll::Ready(());
        }
        *slot.waker.borrow_mut() = Some(cx.waker().clone());
        Poll::Pending
    })
}
