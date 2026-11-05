//! Which pointer locals (and pointer fields of aggregate locals, such as the vtable half of an
//! interface value) hold `static + offset` at each statement: a forward dataflow (module docs
//! of `vtable_loads`). A slot is known where every path to the statement last assigned it the
//! same static and offset: a static, a copy of a known slot, or one plus a constant.
//! Parameters, locals whose address is taken and call results are never known.
//!
//! The cost follows what a fold can use (#2342). A state holds only the slots a fold can read:
//! the pointers that a relocated static flows into and that a load of a pointer goes through,
//! with the slots their values come from (`track`). Every slot of that closure stays exact, also
//! where it is unassigned, since an unassigned path does not spoil a join. Blocks are visited in
//! reverse postorder: the transfer functions are monotone, so the fixed point does not depend on
//! the order, and this order reaches it in about two visits per block.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use velt_vir::vir::{
    AggLayout, BinOp, Const, Function, Local, Operand, Place, Proj, Rvalue, StaticData, StaticId,
    Stmt, Terminator, Ty,
};

use super::slot_read;
use crate::bounds::rpo_rank;
use crate::fresh::Pos;
use crate::visit::{rvalue_operands, stmt_operands, successors, term_operands};

/// What a slot holds at a point.
#[derive(Clone, Copy, PartialEq)]
enum Lat {
    /// Not assigned on any path yet.
    None,
    Known(StaticId, i128),
    Varies,
}

impl Lat {
    fn meet(self, other: Lat) -> Lat {
        match (self, other) {
            (Lat::None, x) | (x, Lat::None) => x,
            (a, b) if a == b => a,
            _ => Lat::Varies,
        }
    }
}

/// The slots of one tracked local.
#[derive(Clone, Copy)]
enum Slots {
    /// A pointer local: one slot.
    Ptr(usize),
    /// An aggregate local: one slot per top-level field, from this index.
    Agg(usize, usize),
}

/// Where a slot's value comes from.
enum Source {
    Slot(usize),
    Static(StaticId),
}

/// The known static addresses of one function.
pub(super) struct Addrs {
    slots: Vec<Option<Slots>>,
    /// Per slot: its index in a state (`None`: no fold can use it, so it is not tracked).
    tracked: Vec<Option<usize>>,
    /// Per block: the state on entry (`None`: unreachable so far).
    entry: Vec<Option<Vec<Lat>>>,
    /// Per block: whether a fold can read through a tracked slot in it.
    reads: Vec<bool>,
}

type State = Vec<Lat>;

impl Addrs {
    /// The analysis of `func`, or `None` when no load can be folded: no local can hold the
    /// address of a static with relocations (a vtable; string literals and other plain data do
    /// not matter here), or no load of a pointer goes through one.
    pub fn of(aggs: &[AggLayout], statics: &[StaticData], func: &Function) -> Option<Addrs> {
        let cands = candidates(statics, func)?;
        let mut slots = vec![None; func.locals.len()];
        let mut n = 0;
        for (i, decl) in func.locals.iter().enumerate() {
            if !cands[i] {
                continue;
            }
            let (s, len) = match decl.ty {
                Ty::Ptr => (Slots::Ptr(n), 1),
                Ty::Agg(id) => match aggs.get(id.0 as usize) {
                    Some(a) => (Slots::Agg(n, a.fields.len()), a.fields.len()),
                    None => continue,
                },
                _ => continue,
            };
            slots[i] = Some(s);
            n += len;
        }
        let mut addrs = Addrs {
            slots,
            tracked: vec![None; n],
            entry: vec![None; func.blocks.len()],
            reads: vec![false; func.blocks.len()],
        };
        let tracked = addrs.track(aggs, statics, func, n);
        if tracked == 0 {
            return None;
        }
        addrs.solve(func, tracked);
        Some(addrs)
    }

    /// Decide which of the `n` slots a state holds; returns how many. A fold reads a slot that
    /// a load of a pointer goes through, when the slot can hold a relocated static: any other
    /// value has no relocation to fold to, so such a load is left alone whatever the slot holds.
    /// What the slot holds at a point depends on the slots it is assigned from, transitively,
    /// and on nothing else: the statements that only overwrite a slot need no other slot.
    fn track(
        &mut self,
        aggs: &[AggLayout],
        statics: &[StaticData],
        func: &Function,
        n: usize,
    ) -> usize {
        let mut relocated = vec![false; n];
        // (from, to) over slots.
        let mut flows: Vec<(usize, usize)> = Vec::new();
        for s in func.blocks.iter().flat_map(|b| &b.stmts) {
            let Stmt::Assign(d, rv) = s else { continue };
            self.sources(d, rv, &mut |to, source| match source {
                Source::Slot(from) => flows.push((from, to)),
                Source::Static(id) => {
                    let has_relocs = statics
                        .get(id.0 as usize)
                        .is_some_and(|s| !s.relocs.is_empty());
                    relocated[to] |= has_relocs;
                }
            });
        }
        close(&mut relocated, &mut flows);
        let mut used = vec![false; n];
        for (b, block) in func.blocks.iter().enumerate() {
            let mut read = |op: &Operand| {
                let Operand::Copy(p) = op else { return };
                let slot = match self.slots.get(p.local.0 as usize).copied().flatten() {
                    Some(Slots::Ptr(i)) if relocated[i] => i,
                    _ => return,
                };
                if slot_read(aggs, 0, p).is_some() {
                    used[slot] = true;
                    self.reads[b] = true;
                }
            };
            block.stmts.iter().for_each(|s| stmt_operands(s, &mut read));
            term_operands(&block.term, &mut read);
        }
        for flow in &mut flows {
            *flow = (flow.1, flow.0);
        }
        close(&mut used, &mut flows);
        let mut count = 0;
        for (slot, _) in used.iter().enumerate().filter(|(_, used)| **used) {
            self.tracked[slot] = Some(count);
            count += 1;
        }
        count
    }

    /// Call `f` with each slot the assignment of `rv` to `d` gives a value that may be known,
    /// and where the value comes from (`step` and `eval` are the same cases).
    fn sources(&self, d: &Place, rv: &Rvalue, f: &mut impl FnMut(usize, Source)) {
        let source = |op: &Operand| match op {
            Operand::Const(Const::Static(id), Ty::Ptr) => Some(Source::Static(*id)),
            Operand::Copy(p) => self.slot(p).map(Source::Slot),
            _ => None,
        };
        if let Some(to) = self.slot(d) {
            let op = match rv {
                Rvalue::Use(op) => op,
                Rvalue::Binary(BinOp::PtrAdd, a, Operand::Const(Const::Int(_), _)) => a,
                _ => return,
            };
            if let Some(from) = source(op) {
                f(to, from);
            }
            return;
        }
        let Some((i, len)) = self.whole_agg(d) else {
            return;
        };
        match rv {
            Rvalue::Aggregate(_, ops) if ops.len() == len => {
                for (k, op) in ops.iter().enumerate() {
                    if let Some(from) = source(op) {
                        f(i + k, from);
                    }
                }
            }
            Rvalue::Use(Operand::Copy(src)) => {
                if let Some(j) = self.same_shape(src, len) {
                    (0..len).for_each(|k| f(i + k, Source::Slot(j + k)));
                }
            }
            _ => {}
        }
    }

    fn solve(&mut self, func: &Function, n: usize) {
        if let Some(first) = self.entry.first_mut() {
            *first = Some(vec![Lat::None; n]);
        }
        // Blocks in reverse postorder, the earliest pending one first; a block is processed
        // again only when its entry state changed.
        let rank = rpo_rank(func);
        let mut by_rank = vec![0; func.blocks.len()];
        for (b, &r) in rank.iter().enumerate() {
            if let Some(at) = by_rank.get_mut(r) {
                *at = b;
            }
        }
        let mut queued = vec![false; func.blocks.len()];
        queued[0] = true;
        let mut work = BinaryHeap::from([Reverse(rank[0])]);
        let mut st = vec![Lat::None; n];
        while let Some(Reverse(r)) = work.pop() {
            let b = by_rank[r];
            queued[b] = false;
            let Some(entry) = &self.entry[b] else {
                continue;
            };
            st.copy_from_slice(entry);
            let block = &func.blocks[b];
            for s in &block.stmts {
                self.step(&mut st, s);
            }
            self.step_term(&mut st, &block.term);
            for next in successors(&block.term) {
                let next = next.0 as usize;
                if meet_into(&mut self.entry[next], &st)
                    && !std::mem::replace(&mut queued[next], true)
                {
                    work.push(Reverse(rank[next]));
                }
            }
        }
    }

    /// The slots of the aggregate local `p` names as a whole: from this index, `len` of them.
    fn whole_agg(&self, p: &Place) -> Option<(usize, usize)> {
        match self.slots.get(p.local.0 as usize).copied().flatten() {
            Some(Slots::Agg(i, len)) if p.proj.is_empty() => Some((i, len)),
            _ => None,
        }
    }

    /// The first slot of `src`, when it names a whole aggregate local of `len` slots.
    fn same_shape(&self, src: &Place, len: usize) -> Option<usize> {
        self.whole_agg(src)
            .and_then(|(j, n)| (n == len).then_some(j))
    }

    /// The slot a place names: a pointer local, or a top-level field of an aggregate one.
    fn slot(&self, p: &Place) -> Option<usize> {
        match (
            self.slots.get(p.local.0 as usize).copied().flatten()?,
            &p.proj[..],
        ) {
            (Slots::Ptr(i), []) => Some(i),
            (Slots::Agg(i, len), [Proj::Field(k)]) if (*k as usize) < len => Some(i + *k as usize),
            _ => None,
        }
    }

    fn value(&self, st: &[Lat], op: &Operand) -> Lat {
        match op {
            Operand::Const(Const::Static(id), Ty::Ptr) => Lat::Known(*id, 0),
            Operand::Copy(p) => self
                .slot(p)
                .and_then(|i| self.tracked[i])
                .map_or(Lat::Varies, |i| st[i]),
            _ => Lat::Varies,
        }
    }

    fn eval(&self, st: &[Lat], rv: &Rvalue) -> Lat {
        match rv {
            Rvalue::Use(op) => self.value(st, op),
            Rvalue::Binary(BinOp::PtrAdd, a, Operand::Const(Const::Int(c), _)) => {
                match self.value(st, a) {
                    Lat::Known(id, off) => Lat::Known(id, off + c),
                    other => other,
                }
            }
            _ => Lat::Varies,
        }
    }

    /// Forget everything about `l`.
    fn clobber(&self, st: &mut [Lat], l: Local) {
        let range = match self.slots.get(l.0 as usize).copied().flatten() {
            Some(Slots::Ptr(i)) => i..i + 1,
            Some(Slots::Agg(i, len)) => i..i + len,
            None => return,
        };
        for i in self.tracked[range].iter().flatten() {
            st[*i] = Lat::Varies;
        }
    }

    fn step(&self, st: &mut [Lat], s: &Stmt) {
        let Stmt::Assign(d, rv) = s else { return };
        if let Some(i) = self.slot(d) {
            if let Some(i) = self.tracked[i] {
                st[i] = self.eval(st, rv);
            }
            return;
        }
        match (self.whole_agg(d), rv) {
            (Some((i, len)), Rvalue::Aggregate(_, ops)) if ops.len() == len => {
                // All operands are read before any field is written: `a = (a.1, a.0)`.
                let values: Vec<(usize, Lat)> = (self.tracked[i..i + len].iter().zip(ops))
                    .filter_map(|(t, op)| t.map(|t| (t, self.value(st, op))))
                    .collect();
                for (t, value) in values {
                    st[t] = value;
                }
            }
            (Some((i, len)), Rvalue::Use(Operand::Copy(src))) => match self.same_shape(src, len) {
                // Another local, or the same one: the order of the fields does not matter.
                Some(j) => {
                    for k in 0..len {
                        if let (Some(to), Some(from)) = (self.tracked[i + k], self.tracked[j + k]) {
                            st[to] = st[from];
                        }
                    }
                }
                None => self.clobber(st, d.local),
            },
            _ => self.clobber(st, d.local),
        }
    }

    fn step_term(&self, st: &mut [Lat], t: &Terminator) {
        if let Terminator::Call { dest: Some(d), .. } = t {
            self.clobber(st, d.local);
        }
    }

    /// Call `f` for every statement and terminator with the addresses known before it.
    pub fn walk(
        &self,
        func: &Function,
        mut f: impl FnMut(Pos, &dyn Fn(Local) -> Option<(StaticId, i128)>),
    ) {
        for (b, block) in func.blocks.iter().enumerate() {
            let (true, Some(mut st)) = (self.reads[b], self.entry[b].clone()) else {
                continue;
            };
            for (i, s) in block.stmts.iter().enumerate() {
                f((b, i), &|l| self.known(&st, l));
                self.step(&mut st, s);
            }
            f((b, block.stmts.len()), &|l| self.known(&st, l));
        }
    }

    /// The address a pointer local holds.
    fn known(&self, st: &[Lat], l: Local) -> Option<(StaticId, i128)> {
        match self.slots.get(l.0 as usize).copied().flatten()? {
            Slots::Ptr(i) => match st[self.tracked[i]?] {
                Lat::Known(id, off) => Some((id, off)),
                _ => None,
            },
            Slots::Agg(..) => None,
        }
    }
}

/// Mark everything `edges` (from, to) lead to from a marked slot.
fn close(marked: &mut [bool], edges: &mut [(usize, usize)]) {
    edges.sort_unstable();
    let mut stack: Vec<usize> = (0..marked.len()).filter(|&s| marked[s]).collect();
    while let Some(s) = stack.pop() {
        let first = edges.partition_point(|e| e.0 < s);
        for &(_, to) in edges[first..].iter().take_while(|e| e.0 == s) {
            if !std::mem::replace(&mut marked[to], true) {
                stack.push(to);
            }
        }
    }
}

/// Meet `st` into a block's entry state; returns whether it changed.
fn meet_into(entry: &mut Option<State>, st: &[Lat]) -> bool {
    match entry {
        None => {
            *entry = Some(st.to_vec());
            true
        }
        Some(old) => {
            let mut changed = false;
            for (o, &x) in old.iter_mut().zip(st) {
                let met = o.meet(x);
                changed |= met != *o;
                *o = met;
            }
            changed
        }
    }
}

/// The locals that may hold a vtable address: assigned (wholly or a field) from an operand
/// that is a relocated static or such a local, transitively. Parameters and locals whose
/// address is taken are left out; `None` when there is no such local.
fn candidates(statics: &[StaticData], func: &Function) -> Option<Vec<bool>> {
    let vtable = |id: &StaticId| {
        statics
            .get(id.0 as usize)
            .is_some_and(|s| !s.relocs.is_empty())
    };
    let n = func.locals.len();
    let mut excluded = vec![false; n];
    excluded
        .iter_mut()
        .take(func.params.len())
        .for_each(|e| *e = true);
    for s in func.blocks.iter().flat_map(|b| &b.stmts) {
        if let Stmt::Assign(_, Rvalue::AddrOf(p)) = s {
            excluded[p.local.0 as usize] = true;
        }
    }
    let mut cands = vec![false; n];
    let mut changed = true;
    while changed {
        changed = false;
        for s in func.blocks.iter().flat_map(|b| &b.stmts) {
            let Stmt::Assign(d, rv) = s else { continue };
            let l = d.local.0 as usize;
            if cands[l] || excluded[l] {
                continue;
            }
            let mut feeds = false;
            rvalue_operands(rv, &mut |op| {
                feeds |= match op {
                    Operand::Const(Const::Static(id), Ty::Ptr) => vtable(id),
                    Operand::Copy(p) => cands[p.local.0 as usize],
                    _ => false,
                };
            });
            if feeds {
                cands[l] = true;
                changed = true;
            }
        }
    }
    // Only worth solving when a candidate is dereferenced somewhere.
    let mut loads = false;
    for s in func.blocks.iter().flat_map(|b| &b.stmts) {
        stmt_operands(s, &mut |op| {
            loads |= matches!(op, Operand::Copy(p) if cands[p.local.0 as usize]
                && matches!(p.proj.first(), Some(Proj::Deref(_))));
        });
    }
    loads.then_some(cands)
}

#[cfg(test)]
mod tests;
