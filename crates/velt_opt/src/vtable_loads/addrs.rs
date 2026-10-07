//! Which pointer locals (and pointer fields of aggregate locals, such as the vtable half of an
//! interface value) hold `static + offset` at each statement: a forward dataflow (module docs
//! of `vtable_loads`). A slot is known where every path to the statement last assigned it the
//! same static and offset: a static, a copy of a known slot, or one plus a constant.
//! Parameters, locals whose address is taken and call results are never known.

use velt_vir::vir::{
    AggLayout, BinOp, Const, Function, Local, Operand, Place, Proj, Rvalue, StaticData, StaticId,
    Stmt, Terminator, Ty,
};

use crate::fresh::Pos;
use crate::visit::{rvalue_operands, successors};

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

/// The known static addresses of one function.
pub(super) struct Addrs {
    slots: Vec<Option<Slots>>,
    /// Per block: the state on entry (`None`: unreachable so far).
    entry: Vec<Option<Vec<Lat>>>,
}

type State = Vec<Lat>;

impl Addrs {
    /// The analysis of `func`, or `None` when no local can hold the address of a static with
    /// relocations (a vtable; string literals and other plain data do not matter here).
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
            entry: vec![None; func.blocks.len()],
        };
        addrs.solve(func, n);
        Some(addrs)
    }

    fn solve(&mut self, func: &Function, n: usize) {
        if let Some(first) = self.entry.first_mut() {
            *first = Some(vec![Lat::None; n]);
        }
        let mut changed = true;
        while changed {
            changed = false;
            for (b, block) in func.blocks.iter().enumerate() {
                let Some(mut st) = self.entry[b].clone() else {
                    continue;
                };
                for s in &block.stmts {
                    self.step(&mut st, s);
                }
                self.step_term(&mut st, &block.term);
                for next in successors(&block.term) {
                    changed |= meet_into(&mut self.entry[next.0 as usize], &st);
                }
            }
        }
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
            Operand::Copy(p) => self.slot(p).map_or(Lat::Varies, |i| st[i]),
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
        st[range].fill(Lat::Varies);
    }

    fn step(&self, st: &mut [Lat], s: &Stmt) {
        let Stmt::Assign(d, rv) = s else { return };
        if let Some(i) = self.slot(d) {
            st[i] = self.eval(st, rv);
            return;
        }
        let whole_agg = match self.slots.get(d.local.0 as usize).copied().flatten() {
            Some(Slots::Agg(i, len)) if d.proj.is_empty() => Some((i, len)),
            _ => None,
        };
        match (whole_agg, rv) {
            (Some((i, len)), Rvalue::Aggregate(_, ops)) if ops.len() == len => {
                let values: Vec<Lat> = ops.iter().map(|op| self.value(st, op)).collect();
                st[i..i + len].copy_from_slice(&values);
            }
            (Some((i, len)), Rvalue::Use(Operand::Copy(src))) => {
                match self.slots.get(src.local.0 as usize).copied().flatten() {
                    Some(Slots::Agg(j, n)) if n == len && src.proj.is_empty() => {
                        st.copy_within(j..j + len, i);
                    }
                    _ => self.clobber(st, d.local),
                }
            }
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
            let Some(mut st) = self.entry[b].clone() else {
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
            Slots::Ptr(i) => match st[i] {
                Lat::Known(id, off) => Some((id, off)),
                _ => None,
            },
            Slots::Agg(..) => None,
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
    cands.iter().any(|&c| c).then_some(cands)
}
