//! Waker slots for leaf futures that wait on an object of the single-threaded runtime (a
//! signal, a task group): one slot per pending wait, replaced only when it would wake another
//! task, freed when the wait is dropped and reused by the next wait. So a wait polled again and
//! again, or many short waits (`Promise.race([work(), signal.whenAborted()])` in a loop), keep
//! the object's memory bounded.

use std::cell::RefCell;
use std::task::Waker;

/// The wakers of an object's pending waits.
#[derive(Default)]
pub struct Waiters {
    /// One slot per pending wait (`None`: free, or already woken).
    slots: RefCell<Vec<Option<Waker>>>,
    free: RefCell<Vec<usize>>,
}

impl Waiters {
    /// Wake every registered wait (each registers again if it polls pending again).
    pub fn wake_all(&self) {
        let ws: Vec<Waker> = self
            .slots
            .borrow_mut()
            .iter_mut()
            .filter_map(Option::take)
            .collect();
        for w in ws {
            w.wake();
        }
    }
}

#[cfg(test)]
impl Waiters {
    /// The number of slots (pending waits plus free slots).
    pub fn slots(&self) -> usize {
        self.slots.borrow().len()
    }
}

/// One wait's slot in a [`Waiters`]; the wait calls [`Slot::release`] when it is dropped.
#[derive(Default)]
pub struct Slot(Option<usize>);

impl Slot {
    /// Register `w` as this wait's waker.
    pub fn register(&mut self, ws: &Waiters, w: &Waker) {
        let mut slots = ws.slots.borrow_mut();
        match self.0 {
            Some(i) => {
                if !slots[i].as_ref().is_some_and(|old| old.will_wake(w)) {
                    slots[i] = Some(w.clone());
                }
            }
            None => {
                let i = match ws.free.borrow_mut().pop() {
                    Some(i) => i,
                    None => {
                        slots.push(None);
                        slots.len() - 1
                    }
                };
                slots[i] = Some(w.clone());
                self.0 = Some(i);
            }
        }
    }

    /// Free the slot (the wait is gone).
    pub fn release(&mut self, ws: &Waiters) {
        if let Some(i) = self.0.take() {
            if let Some(w) = ws.slots.borrow_mut().get_mut(i) {
                *w = None;
            }
            ws.free.borrow_mut().push(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_polled_again_keeps_one_slot_and_frees_it() {
        let ws = Waiters::default();
        let mut a = Slot::default();
        for _ in 0..100 {
            a.register(&ws, Waker::noop());
        }
        assert_eq!(ws.slots.borrow().len(), 1);
        a.release(&ws);
        let mut b = Slot::default();
        b.register(&ws, Waker::noop());
        assert_eq!(ws.slots.borrow().len(), 1, "the freed slot is reused");
        ws.wake_all();
        b.register(&ws, Waker::noop());
        assert_eq!(ws.slots.borrow().len(), 1);
        b.release(&ws);
    }
}
