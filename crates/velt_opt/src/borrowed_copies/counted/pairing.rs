//! Which of a family's releases give back its increment's reference, and which the family
//! keeps (#2455).
//!
//! A copy made from a field leaves the field its reference: the family holds one, the
//! increment's, and gives it back once on every path. A copy made from an owner is different.
//! When the root owns its object (a fresh allocation, an array the runtime made, an element
//! moved out of its array) and the copy is made through the root itself, the family holds two
//! references after the increment and releases both: the copy's and the owner's. Only the
//! first of them on a path is the increment's to give back; the second frees the object, and
//! stays.
//!
//! A forward analysis follows what may be so on entry to each block: the family holds the
//! increment's reference, has given it back, or never took it. Each way it knows which members
//! may hold the object, which hold it for certain, and which hold the same value:
//! - none holds it on entry; a copy holds what its source holds; null holds nothing; a pointer
//!   into a new block (`p + 8`) is never null;
//! - a member's null test tells, on each edge, about every member with the same value, and an
//!   edge saying that a member which holds the object is null is never taken.
//!
//! A release through a member that holds the object is *paired* while the reference is held
//! and the family's *own* otherwise. Paired releases go, and so do releases only ever reached
//! with a member that holds nothing (they do nothing); own releases stay. The family is left
//! as it is when a release is paired on one path and its own on another, when a path
//! increments again or returns while the reference is held, or when a member is overwritten
//! while it is.
//!
//! A family is *owned* when every path that gave the increment's reference back goes on to a
//! release of its own before it returns: it then had a second reference all the while the
//! increment's was held. Its values never leave it, so nothing else can give that reference
//! back, and no call can free the object while the copy is used.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_vir::vir::{BinOp, Local, Operand, Rvalue, Stmt, Terminator};

use super::{is_int, local_of, Cx, Family};
use crate::visit::successors;

/// A set of a family's members.
#[derive(Clone, PartialEq)]
struct Bits(Vec<u64>);

impl Bits {
    fn empty(n: usize) -> Bits {
        Bits(vec![0; n.div_ceil(64)])
    }

    fn get(&self, i: usize) -> bool {
        self.0[i / 64] >> (i % 64) & 1 == 1
    }

    fn put(&mut self, i: usize, on: bool) {
        if on {
            self.0[i / 64] |= 1 << (i % 64);
        } else {
            self.0[i / 64] &= !(1 << (i % 64));
        }
    }

    /// Adds `other`'s members; whether any was new.
    fn union(&mut self, other: &Bits) -> bool {
        let mut grew = false;
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            grew |= *b & !*a != 0;
            *a |= *b;
        }
        grew
    }
}

/// The members that may hold the object, those that hold it on every path, and which members
/// hold the same value as which (a copy and its source, until one is written again).
#[derive(Clone)]
struct Sets {
    may: Bits,
    must: Bits,
    /// Per member, the first member of its class of equal values.
    class: Vec<u32>,
}

impl Sets {
    fn empty(n: usize) -> Sets {
        Sets {
            may: Bits::empty(n),
            must: Bits::empty(n),
            class: (0..n as u32).collect(),
        }
    }

    fn holds(&self, m: usize) -> bool {
        self.may.get(m)
    }

    /// The members holding the same value as `m`, itself included.
    fn equals(&self, m: usize) -> Vec<usize> {
        let c = self.class[m];
        (0..self.class.len())
            .filter(|&j| self.class[j] == c)
            .collect()
    }

    /// Member `d` written: with a copy of member `from`, or else with something that may be
    /// the object (`may`) or is it for certain (`must`).
    fn write(&mut self, d: usize, from: Option<usize>, may: bool, must: bool) {
        if from == Some(d) {
            return;
        }
        let (may, must) = match from {
            Some(m) => (self.may.get(m), self.must.get(m)),
            None => (may, must),
        };
        self.may.put(d, may);
        self.must.put(d, must);
        // Out of its class (whose first member it may have been), into the source's.
        let rest = self.equals(d).into_iter().find(|&j| j != d);
        for j in 0..self.class.len() {
            if self.class[j] as usize == d && j != d {
                self.class[j] = rest.unwrap_or(j) as u32;
            }
        }
        self.class[d] = d as u32;
        if let Some(m) = from {
            let first = (self.class[m] as usize).min(d) as u32;
            for j in self.equals(m) {
                self.class[j] = first;
            }
            self.class[d] = first;
        }
    }

    /// Adds what `other` allows; whether anything changed.
    fn join(&mut self, other: &Sets) -> bool {
        let grew = self.may.union(&other.may);
        let mut shrank = false;
        for (a, b) in self.must.0.iter_mut().zip(&other.must.0) {
            shrank |= *a & !*b != 0;
            *a &= *b;
        }
        // Equal here only when equal on both sides.
        let mut firsts: FxHashMap<(u32, u32), u32> = FxHashMap::default();
        for j in 0..self.class.len() {
            let both = (self.class[j], other.class[j]);
            let first = *firsts.entry(both).or_insert(j as u32);
            shrank |= first != self.class[j];
            self.class[j] = first;
        }
        grew || shrank
    }
}

/// What may be so at a point: the family holds the increment's reference (`held`), has given
/// it back and released nothing of its own since (`after`), or neither (`free`). Each with the
/// members holding the object then; `None` when no path gets there that way.
#[derive(Clone, Default)]
struct Held {
    held: Option<Sets>,
    after: Option<Sets>,
    free: Option<Sets>,
}

impl Held {
    /// Adds what `other` allows; whether anything changed.
    fn join(&mut self, other: &Held) -> bool {
        let mut changed = false;
        for (into, from) in [
            (&mut self.held, &other.held),
            (&mut self.after, &other.after),
            (&mut self.free, &other.free),
        ] {
            if let Some(from) = from {
                changed |= join_sets(into, from);
            }
        }
        changed
    }

    fn is_empty(&self) -> bool {
        self.held.is_none() && self.after.is_none() && self.free.is_none()
    }

    fn sets(&mut self) -> impl Iterator<Item = &mut Sets> {
        [&mut self.held, &mut self.after, &mut self.free]
            .into_iter()
            .flatten()
    }

    /// What is left on an edge where member `m`, and with it every member holding the same
    /// value, is null (`set` false) or not.
    fn tested(mut self, m: usize, set: bool) -> Held {
        if set {
            // No path on which `m` holds nothing.
            for s in [&mut self.held, &mut self.after, &mut self.free] {
                *s = s.take().filter(|s| s.may.get(m));
            }
            for s in self.sets() {
                for j in s.equals(m) {
                    s.must.put(j, true);
                }
            }
            return self;
        }
        // No path on which `m` holds the object.
        for s in [&mut self.held, &mut self.after, &mut self.free] {
            *s = s.take().filter(|s| !s.must.get(m));
        }
        for s in self.sets() {
            for j in s.equals(m) {
                s.may.put(j, false);
            }
        }
        // Nothing is held (the reference went with a member that was never set).
        if let Some(h) = self.held.take() {
            join_sets(&mut self.free, &h);
        }
        self
    }
}

fn join_sets(into: &mut Option<Sets>, from: &Sets) -> bool {
    match into {
        Some(have) => have.join(from),
        None => {
            *into = Some(from.clone());
            true
        }
    }
}

/// What a statement does to the family.
#[derive(Clone, Copy)]
enum Event {
    /// The increment.
    Increment,
    /// Member `member` written: with a copy of member `from`, or else with something that may
    /// be the object (`may`) or is it for certain (`must`).
    Write {
        member: usize,
        from: Option<usize>,
        may: bool,
        must: bool,
    },
}

/// A block's null test of `members`: the successor where they are null and the one where they
/// are set.
struct Test {
    null: usize,
    set: usize,
    members: Vec<usize>,
}

impl Cx<'_> {
    /// Pairs `fam`'s increment with its releases (module doc): leaves in `fam.releases` the
    /// ones that give its reference back, moves the others to `fam.own`, and notes the calls
    /// made while the reference is held and whether the family is owned. `false` when the increment has no such pairing: some
    /// path returns or increments again while it is held, a member is overwritten while it is,
    /// or a release gives back the increment's reference on one path and another on another.
    pub(super) fn pair(&self, fam: &mut Family) -> bool {
        let n = self.func.blocks.len();
        let index: FxHashMap<Local, usize> = (fam.members.iter().enumerate())
            .map(|(i, &m)| (m, i))
            .collect();
        // What each block does to the family, found once: most blocks do nothing.
        let events: Vec<Vec<Event>> = (self.func.blocks.iter().enumerate())
            .map(|(b, block)| self.events(fam, &index, b, &block.stmts))
            .collect();
        let tests: Vec<Option<Test>> = (self.func.blocks.iter().enumerate())
            .map(|(b, block)| match &block.term {
                Terminator::Branch { cond, then, els } => {
                    self.null_test(fam, b, cond).map(|(ne, ms)| {
                        let (null, set) = if ne { (els, then) } else { (then, els) };
                        Test {
                            null: null.0 as usize,
                            set: set.0 as usize,
                            members: ms.iter().map(|m| index[m]).collect(),
                        }
                    })
                }
                _ => None,
            })
            .collect();
        let mut states: Vec<Held> = vec![Held::default(); n];
        // On entry no member is written yet: none holds the object.
        states[0].free = Some(Sets::empty(index.len()));
        // Per release (its test block): reached holding the reference, and reached without.
        let mut paired: FxHashSet<usize> = FxHashSet::default();
        let mut own: FxHashSet<usize> = FxHashSet::default();
        let mut held_calls: FxHashSet<usize> = FxHashSet::default();
        // Some path returns having given the reference back, with no release of its own after.
        let mut unowned = false;
        let (mut work, mut queued) = (vec![0usize], vec![false; n]);
        while let Some(b) = work.pop() {
            queued[b] = false;
            let term = &self.func.blocks[b].term;
            // A block that does nothing to the family hands its state on as it is.
            let passes = events[b].is_empty()
                && fam.release_at(b).is_none()
                && !successors(term).iter().any(|s| s.0 as usize == b);
            let mut st = match passes {
                true => std::mem::take(&mut states[b]),
                false => states[b].clone(),
            };
            for e in &events[b] {
                match *e {
                    Event::Increment => {
                        if st.held.is_some() {
                            return false;
                        }
                        st.held = st.free.take();
                        if let Some(after) = st.after.take() {
                            join_sets(&mut st.held, &after);
                        }
                    }
                    Event::Write {
                        member,
                        from,
                        may,
                        must,
                    } => {
                        // A member (or the root) written while it holds the reference.
                        if st.held.is_some() && from.is_none() {
                            return false;
                        }
                        for set in st.sets() {
                            set.write(member, from, may, must);
                        }
                    }
                }
            }
            match term {
                Terminator::Return(_) if st.held.is_some() => return false,
                Terminator::Return(_) => unowned |= st.after.is_some(),
                Terminator::Call { .. } if st.held.is_some() => {
                    held_calls.insert(b);
                }
                _ => {}
            }
            let mut push = |states: &mut Vec<Held>, s: usize, to: &Held| {
                if states[s].join(to) && !std::mem::replace(&mut queued[s], true) {
                    work.push(s);
                }
            };
            if let Some(r) = fam.release_at(b) {
                let Some(&m) = index.get(&r.through) else {
                    return false;
                };
                if st.held.as_ref().is_some_and(|set| set.holds(m)) {
                    // It gives the increment's reference back: borrowed, it goes straight to
                    // its decrement. The members still hold the object.
                    paired.insert(b);
                    let after = Held {
                        after: st.held.take(),
                        ..Held::default()
                    };
                    push(&mut states, r.dec.0 as usize, &after);
                }
                let holds = |set: &Option<Sets>| set.as_ref().is_some_and(|s| s.holds(m));
                if holds(&st.after) || holds(&st.free) {
                    // A member holds the object and the increment's reference is not held:
                    // this gives back another one, the family's own.
                    own.insert(b);
                }
                if holds(&st.after) {
                    if let Some(after) = st.after.take() {
                        join_sets(&mut st.free, &after);
                    }
                }
                if paired.contains(&b) && own.contains(&b) {
                    return false;
                }
                // Whatever is left goes on as through any block: a release the family keeps,
                // or one of a member that holds nothing (which does nothing).
            }
            if !st.is_empty() {
                for s in successors(term) {
                    let s = s.0 as usize;
                    match &tests[b] {
                        Some(t) if (s == t.null) != (s == t.set) => {
                            let mut to = st.clone();
                            for &m in &t.members {
                                to = to.tested(m, s == t.set);
                            }
                            push(&mut states, s, &to);
                        }
                        _ => push(&mut states, s, &st),
                    }
                }
            }
            if passes {
                states[b] = st;
            }
        }
        // A release no path reaches with its member holding the object does nothing: it goes
        // with the paired ones.
        let (kept, releases) = (fam.releases.drain(..)).partition(|r| own.contains(&r.test));
        fam.releases = releases;
        fam.own = kept;
        fam.owned = !unowned && !own.is_empty();
        fam.held_calls = held_calls;
        true
    }

    /// What the statements of block `b` do to the family, in order.
    fn events(
        &self,
        fam: &Family,
        index: &FxHashMap<Local, usize>,
        b: usize,
        stmts: &[Stmt],
    ) -> Vec<Event> {
        let mut events = vec![];
        for (si, s) in stmts.iter().enumerate() {
            if (b, si) == fam.inc {
                events.push(Event::Increment);
                continue;
            }
            let Stmt::Assign(d, rv) = s else { continue };
            let Some(&member) = index.get(&d.local).filter(|_| d.proj.is_empty()) else {
                continue;
            };
            let from = match rv {
                Rvalue::Use(op) => local_of(op).and_then(|m| index.get(&m)).copied(),
                _ => None,
            };
            // A copy holds what its source holds; null holds nothing; anything else (the
            // root's definition, its load repeated) may hold the object, and a pointer into a
            // block (`p + 8` of a new one) is never null.
            events.push(Event::Write {
                member,
                from,
                may: !matches!(rv, Rvalue::Use(op) if is_int(op, 0)),
                must: matches!(rv, Rvalue::Binary(BinOp::PtrAdd, ..)),
            });
        }
        events
    }

    /// `cond`, the test block `b` ends with, as `m != null` (true) or `m == null` (false) of
    /// members `m`: of the member compared, or, when the root's own load is what is compared
    /// (`c = *p; t = *p; if (t != null)`, lowering's copy of a nullable field), of the members
    /// that load was read into in this block, with nothing written through a pointer between
    /// the two reads and the member not written again. (Their copies hold the same value: the
    /// analysis knows those.)
    fn null_test(&self, fam: &Family, b: usize, cond: &Operand) -> Option<(bool, Vec<Local>)> {
        let (_, rv) = self.def_of(local_of(cond)?)?;
        let Rvalue::Binary(op, x, y) = rv else {
            return None;
        };
        let ne = match op {
            BinOp::Ne => true,
            BinOp::Eq => false,
            _ => return None,
        };
        let tested = match (local_of(x), local_of(y)) {
            (Some(t), None) if is_int(y, 0) => t,
            (None, Some(t)) if is_int(x, 0) => t,
            _ => return None,
        };
        if fam.members.contains(&tested) {
            return Some((ne, vec![tested]));
        }
        let ((at, load), stmts) = (self.def_of(tested)?, &self.func.blocks[b].stmts);
        if at.0 != b || !self.repeats_root_load(fam, load) {
            return None;
        }
        let plain = |s: &Stmt| match s {
            Stmt::Assign(d, _) => d.proj.is_empty(),
            Stmt::Nop => true,
            _ => false,
        };
        let same: Vec<Local> = (stmts.iter().enumerate())
            .filter_map(|(si, s)| {
                let Stmt::Assign(d, rv) = s else { return None };
                let is_load = d.proj.is_empty() && fam.members.contains(&d.local) && rv == load;
                let (lo, hi) = (si.min(at.1), si.max(at.1));
                let quiet = stmts[lo..hi].iter().all(plain);
                let last = !(stmts[si + 1..].iter())
                    .any(|s| matches!(s, Stmt::Assign(d2, _) if d2.local == d.local));
                (is_load && quiet && last).then_some(d.local)
            })
            .collect();
        (!same.is_empty()).then_some((ne, same))
    }
}
