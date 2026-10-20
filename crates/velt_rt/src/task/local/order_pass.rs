//! Which timer comes next, as in Node's event loop (order.rs): a timers pass reads the clock once
//! and runs, list by list, every timer due by then; once none is left, the immediates created
//! so far run (the check phase), and the next pass reads the clock again.
//!
//! [`Order::peek`] only looks: the first poll of a timer while `main` still runs, a timer that
//! waits behind another and the wake-ups after a turn never start a pass or a check phase.
//! [`Order::commit`] moves the loop on, when a timer takes its turn: that is when Node's loop
//! reaches it. The last look is kept until the order changes, so a turn usually costs one search.

use std::sync::Arc;

use super::{Gate, Key, ListKey, Order, IMMEDIATE};

/// Where the loop is when a timer takes its turn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Phase {
    /// The timers pass with this `now`.
    Pass(u64),
    /// The check phase, running the immediates created up to this number.
    Check(u64),
}

/// The result of [`Order::peek`].
#[derive(Clone, Copy, Default, Debug)]
pub(super) struct Peek {
    /// The timer that comes next, and the phase it runs in.
    pub(super) next: Option<(Key, Phase)>,
    /// The `now` of the pass it was chosen in (an immediate: of the pass that is over).
    now: Option<u64>,
    /// The list of the timer chosen.
    list: Option<ListKey>,
    /// Lists of a task busy in another poll were passed over.
    pub(super) skipped: bool,
    /// Lists due by `now` whose next timer is not were passed over: they get their new place
    /// when the turn is taken.
    partly: bool,
    /// The clock was read: the result holds only for now.
    fresh: bool,
    /// Lists were passed over in the pass under way (not one the clock was read for).
    passed_in_pass: bool,
}

impl Order {
    /// The first immediate created up to `limit` whose task is not busy elsewhere.
    fn immediate_upto(
        &self,
        current: Option<&Arc<Gate>>,
        skip_busy: bool,
        limit: u64,
        skipped: &mut bool,
    ) -> Option<Key> {
        for &(seq, gate) in &self.immediates {
            if seq > limit {
                break;
            }
            if skip_busy && self.groups[&gate].busy_elsewhere(current) {
                *skipped = true;
                continue;
            }
            return Some(Key {
                gate,
                ms: IMMEDIATE,
                seq,
            });
        }
        None
    }

    /// The first timer due by `now`, in the order of the lists (a list due by then whose next
    /// timer is not is passed over), and its list.
    fn first_due(
        &self,
        current: Option<&Arc<Gate>>,
        skip_busy: bool,
        now: u64,
        p: &mut Peek,
    ) -> Option<(Key, ListKey)> {
        let mut best: Option<(Key, ListKey)> = None;
        for &(head, gate) in &self.heads {
            if head.0 > now || best.is_some_and(|b| head >= b.1) {
                break;
            }
            let group = &self.groups[&gate];
            if skip_busy && group.busy_elsewhere(current) {
                p.skipped = true;
                continue;
            }
            for &(lk, ms) in &group.order {
                if lk.0 > now || best.is_some_and(|b| lk >= b.1) {
                    break;
                }
                let (seq, e) = group.lists[&ms]
                    .timers
                    .first()
                    .expect("ICE: an empty timer list");
                if e.start.saturating_add(ms) <= now {
                    best = Some((Key { gate, ms, seq }, lk));
                    break;
                }
                p.partly = true;
            }
        }
        best
    }

    /// What comes next, seen from the task with gate `current` (`skip_busy`: the lists of a task
    /// busy in another poll are passed over), without changing anything.
    pub(super) fn peek(
        &self,
        current: Option<&Arc<Gate>>,
        skip_busy: bool,
        clock: &mut dyn FnMut() -> u64,
    ) -> Peek {
        let mut p = Peek::default();
        let mut check = self.check;
        if let Some(limit) = check {
            if let Some(k) = self.immediate_upto(current, skip_busy, limit, &mut p.skipped) {
                p.next = Some((k, Phase::Check(limit)));
                return p;
            }
            // Over, unless only immediates of tasks busy elsewhere are left, which do not hold
            // back the timers of the others.
            if self.immediates.first().is_none_or(|i| i.0 > limit) {
                check = None;
            }
        }
        let (mut now, mut fresh) = match self.pass {
            Some(n) => (n, false),
            None => (clock(), true),
        };
        p.fresh = fresh;
        loop {
            if let Some((k, lk)) = self.first_due(current, skip_busy, now, &mut p) {
                p.next = Some((k, Phase::Pass(now)));
                p.now = Some(now);
                p.list = Some(lk);
                p.passed_in_pass |= !fresh && p.partly;
                return p;
            }
            p.passed_in_pass |= !fresh && p.partly;
            // The pass is over: the immediates created so far run next.
            if check.is_none() {
                if let Some(k) = self.immediate_upto(current, skip_busy, self.seq, &mut p.skipped) {
                    p.next = Some((k, Phase::Check(self.seq)));
                    p.now = Some(now);
                    return p;
                }
            }
            if fresh {
                return p;
            }
            p.passed_in_pass = p.partly;
            now = clock();
            fresh = true;
            p.fresh = true;
            p.partly = false;
        }
    }

    /// [`Order::peek`], answered from the last look while the order has not changed since and
    /// the clock was not read for it.
    pub(super) fn peek_cached(
        &mut self,
        current: Option<&Arc<Gate>>,
        skip_busy: bool,
        clock: &mut dyn FnMut() -> u64,
    ) -> Peek {
        if let Some(c) = self.cache {
            let valid = match c.next {
                Some((k, _)) => !skip_busy || !self.groups[&k.gate].busy_elsewhere(current),
                None => true,
            };
            if valid {
                return c;
            }
        }
        let mut p = self.peek(current, skip_busy, clock);
        if let (true, Some(now)) = (p.passed_in_pass, self.pass) {
            // The pass under way has met lists whose next timer is not due: they get their new
            // places now (with the pass's own clock, so this changes nothing a later look would
            // not), and the look is taken again.
            let before = p.now.filter(|n| *n == now).and(p.list);
            self.rekey_passed(current, now, before);
            p = self.peek(current, skip_busy, clock);
        }
        self.cache = (!p.fresh && !p.skipped).then_some(p);
        p
    }

    /// The lists due by `now` (and before list `before`) whose next timer is not get their new
    /// places, in the order the pass meets them: `max(start + ms, now + 1)`, a new id.
    fn rekey_passed(&mut self, current: Option<&Arc<Gate>>, now: u64, before: Option<ListKey>) {
        let mut passed = Vec::new();
        for (&gate, group) in &self.groups {
            if group.busy_elsewhere(current) {
                continue;
            }
            for &(lk, ms) in &group.order {
                if lk.0 > now || before.is_some_and(|l| lk >= l) {
                    break;
                }
                let (_, e) = group.lists[&ms]
                    .timers
                    .first()
                    .expect("ICE: an empty timer list");
                let due = e.start.saturating_add(ms);
                if due > now {
                    passed.push((lk, gate, ms, due));
                }
            }
        }
        passed.sort_unstable();
        for (_, gate, ms, due) in passed {
            self.rekey(gate, ms, due.max(now + 1));
        }
    }

    /// The timer `p` chose takes its turn: the loop moves on to its phase. The lists `p` passed
    /// over get their new places first, as the pass met them (`max(start + ms, now + 1)`, a new
    /// id).
    pub(super) fn commit(&mut self, p: &Peek, current: Option<&Arc<Gate>>) {
        self.cache = None;
        let Some((_, phase)) = p.next else {
            return;
        };
        if let (true, Some(now)) = (p.partly, p.now) {
            self.rekey_passed(current, now, p.list);
        }
        match phase {
            Phase::Pass(now) => {
                self.pass = Some(now);
                self.check = None;
            }
            Phase::Check(limit) => {
                self.pass = None;
                self.check = Some(limit);
            }
        }
    }
}
