//! `velt_rt_group_*` on the current-thread executor: the live-children count of `taskScope`
//! (see velt_rt's task/group.rs). Handles are `(generation << 32) | (slot + 1)` keys into a
//! table whose freed slots are reused with the next generation, so the table stays bounded and
//! a stale handle never reaches a newer group. A wait has one waker slot (waiters.rs).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::task::{Context, Poll};

use super::leaf::new_leaf;
use super::waiters::{Slot, Waiters};
use super::VeltFut;

#[derive(Default)]
struct Group {
    live: Cell<usize>,
    /// The scope's wait saw no live child: later `enter`s fail (see velt_rt).
    closed: Cell<bool>,
    waiters: Waiters,
}

/// A pending group wait: its group and its waker slot.
struct Wait {
    grp: Option<Rc<Group>>,
    slot: Slot,
}

impl Drop for Wait {
    fn drop(&mut self) {
        if let Some(grp) = &self.grp {
            self.slot.release(&grp.waiters);
        }
    }
}

#[derive(Default)]
struct Table {
    slots: Vec<(u32, Option<Rc<Group>>)>,
    free: Vec<usize>,
}

thread_local! {
    static GROUPS: RefCell<Table> = RefCell::new(Table::default());
}

/// `(slot, generation)` of key `g` (none for 0).
fn split(g: u64) -> Option<(usize, u32)> {
    let slot = ((g & 0xffff_ffff) as usize).checked_sub(1)?;
    Some((slot, (g >> 32) as u32))
}

fn get(g: u64) -> Option<Rc<Group>> {
    let (i, generation) = split(g)?;
    GROUPS.with(|t| match t.borrow().slots.get(i) {
        Some((gen, Some(grp))) if *gen == generation => Some(grp.clone()),
        _ => None,
    })
}

#[no_mangle]
pub extern "C" fn velt_rt_group_new() -> u64 {
    GROUPS.with(|t| {
        let mut t = t.borrow_mut();
        let grp = Some(Rc::new(Group::default()));
        let i = match t.free.pop() {
            Some(i) => {
                t.slots[i].1 = grp;
                i
            }
            None => {
                t.slots.push((1, grp));
                t.slots.len() - 1
            }
        };
        ((t.slots[i].0 as u64) << 32) | (i as u64 + 1)
    })
}

#[no_mangle]
pub extern "C" fn velt_rt_group_enter(g: u64) -> bool {
    get(g).is_some_and(|grp| {
        if grp.closed.get() {
            return false;
        }
        grp.live.set(grp.live.get() + 1);
        true
    })
}

#[no_mangle]
pub extern "C" fn velt_rt_group_leave(g: u64) {
    if let Some(grp) = get(g) {
        grp.live.set(grp.live.get().saturating_sub(1));
        if grp.live.get() == 0 {
            grp.waiters.wake_all();
        }
    }
}

#[no_mangle]
pub extern "C" fn velt_rt_group_wait(g: u64) -> *mut VeltFut {
    let mut wait = Wait {
        grp: get(g),
        slot: Slot::default(),
    };
    new_leaf(move |cx: &mut Context<'_>| match &wait.grp {
        Some(grp) if grp.live.get() > 0 => {
            wait.slot.register(&grp.waiters, cx.waker());
            Poll::Pending
        }
        Some(grp) => {
            grp.closed.set(true);
            Poll::Ready(())
        }
        None => Poll::Ready(()),
    })
}

/// Ends the group; freeing it again (or a stale key) does nothing.
#[no_mangle]
pub extern "C" fn velt_rt_group_free(g: u64) {
    let Some((i, generation)) = split(g) else {
        return;
    };
    GROUPS.with(|t| {
        let mut t = t.borrow_mut();
        let live = matches!(t.slots.get(i), Some((gen, Some(_))) if *gen == generation);
        if live {
            let slot = &mut t.slots[i];
            slot.1 = None;
            slot.0 = slot.0.checked_add(1).unwrap_or(1);
            t.free.push(i);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freed_slots_are_reused_with_a_new_generation() {
        let a = velt_rt_group_new();
        velt_rt_group_free(a);
        velt_rt_group_free(a);
        let b = velt_rt_group_new();
        assert_ne!(a, b, "a stale key never names the new group");
        assert_eq!(a & 0xffff_ffff, b & 0xffff_ffff, "the slot is reused");
        assert!(!velt_rt_group_enter(a));
        assert!(velt_rt_group_enter(b));
        velt_rt_group_leave(b);
        velt_rt_group_free(b);
    }
}
