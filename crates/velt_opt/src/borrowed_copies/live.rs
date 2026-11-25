//! When a family's text may still be read, and what may change a clone's source then.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_vir::vir::{Callee, Function, Local, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty};

use super::{Cx, Family, Point, Source};
use crate::locals::Usage;
use crate::visit::{derefs, rvalue_operands, successors, term_operands};

impl Cx<'_> {
    /// Whether nothing can change a source of `f` while the family's text may still be read.
    pub(super) fn borrowable(&self, f: &Family) -> bool {
        let sources: Vec<Source> = f.sources.iter().map(|s| self.source(*s)).collect();
        let tracked: Vec<Local> = f.members.iter().chain(f.pointers.iter()).copied().collect();
        let index: FxHashMap<Local, usize> =
            tracked.iter().enumerate().map(|(i, l)| (*l, i)).collect();
        let n = self.func.blocks.len();
        let preds = crate::map_probe::region::predecessors(self.func);
        // Strong liveness of the tracked locals, backwards to a fixed point.
        let mut live_in: Vec<Vec<bool>> = vec![vec![false; tracked.len()]; n];
        let mut work: Vec<usize> = (0..n).collect();
        let mut queued = vec![true; n];
        while let Some(b) = work.pop() {
            queued[b] = false;
            let mut live = self.live_out(b, &live_in, tracked.len());
            self.walk_block(&index, b, &mut live, &mut |_, _| {});
            if live != live_in[b] {
                live_in[b] = live;
                for &p in &preds[b] {
                    if !std::mem::replace(&mut queued[p], true) {
                        work.push(p);
                    }
                }
            }
        }
        // Check every point after which a tracked local is still live.
        for b in 0..n {
            let mut live = self.live_out(b, &live_in, tracked.len());
            let mut clobbered = false;
            self.walk_block(&index, b, &mut live, &mut |at, live_after| {
                if !clobbered && live_after.contains(&true) && self.clobbers(f, &sources, at) {
                    clobbered = true;
                }
            });
            if clobbered {
                return false;
            }
        }
        true
    }

    fn live_out(&self, b: usize, live_in: &[Vec<bool>], len: usize) -> Vec<bool> {
        let mut out = vec![false; len];
        for s in successors(&self.func.blocks[b].term) {
            if let Some(l) = live_in.get(s.0 as usize) {
                for (o, x) in out.iter_mut().zip(l) {
                    *o |= *x;
                }
            }
        }
        out
    }

    /// Walk block `b` backwards from `live` (its live-out), calling `visit` with each point and
    /// the set live after it; leaves the live-in in `live`.
    fn walk_block(
        &self,
        index: &FxHashMap<Local, usize>,
        b: usize,
        live: &mut [bool],
        visit: &mut impl FnMut(Point, &[bool]),
    ) {
        let block = &self.func.blocks[b];
        let n = block.stmts.len();
        visit((b, n), live);
        if let Terminator::Call { callee, args, .. } = &block.term {
            // The clone into a member defines it; reads of the text are uses.
            if self.rt.is(callee, self.rt.clone) && args.len() == 2 {
                if let Some(m) = self.addr_of_local(&args[1]) {
                    if let Some(&i) = index.get(&m) {
                        live[i] = false;
                    }
                }
                self.gen_operand(index, &args[0], live);
            } else if self.views && self.rt.is(callee, self.rt.slice) && args.len() == 4 {
                // A slice into a member defines it and reads its input's text (`views.rs`).
                if let Some(m) = self.addr_of_local(&args[3]) {
                    if let Some(&i) = index.get(&m) {
                        live[i] = false;
                    }
                }
                self.gen_operand(index, &args[0], live);
            } else if self.rt.reads_only(callee) {
                for a in args {
                    self.gen_operand(index, a, live);
                }
            }
        }
        // An operand read through a family pointer (a branch on a text byte) is a use.
        term_operands(&block.term, &mut |op| self.gen_deref(index, op, live));
        for (i, s) in block.stmts.iter().enumerate().rev() {
            if let Stmt::Assign(dst, rv) = s {
                if dst.proj.is_empty() {
                    if let Some(&x) = index.get(&dst.local) {
                        let needed = std::mem::replace(&mut live[x], false);
                        if needed {
                            rvalue_operands(rv, &mut |op| {
                                self.gen_operand(index, op, live);
                            });
                            if let Rvalue::AddrOf(a) = rv {
                                if let Some(&j) = index.get(&a.local) {
                                    live[j] = true;
                                }
                            }
                        }
                        visit((b, i), live);
                        continue;
                    }
                }
                // Reads of the text through a family pointer are uses, wherever they go.
                rvalue_operands(rv, &mut |op| self.gen_deref(index, op, live));
            }
            visit((b, i), live);
        }
    }

    /// `op` read through a pointer: the pointer is used.
    fn gen_deref(&self, index: &FxHashMap<Local, usize>, op: &Operand, live: &mut [bool]) {
        if matches!(op, Operand::Copy(p) if derefs(p)) {
            self.gen_operand(index, op, live);
        }
    }

    fn gen_operand(&self, index: &FxHashMap<Local, usize>, op: &Operand, live: &mut [bool]) {
        if let Operand::Copy(p) = op {
            if let Some(&i) = index.get(&p.local) {
                live[i] = true;
            }
        }
    }

    /// Whether place `p` holds a pointer (or a 64-bit word, which could be one).
    fn pointer_typed(&self, p: &Place) -> bool {
        matches!(
            crate::locals::place_ty(self.aggs, self.func, p),
            Some(Ty::Ptr | Ty::U64 | Ty::I64) | None
        )
    }

    /// Where the source pointer `s` points.
    fn source(&self, s: Local) -> Source {
        // Views mode: a parameter (written once, its address never taken) points at a string the
        // caller holds, never at one of this function's locals.
        if self.views && self.outside_locals(s, 0) && (s.0 as usize) < self.func.params.len() {
            return Source::Caller;
        }
        if (s.0 as usize) < self.func.params.len() || self.usage.get(s).defs != 1 {
            return Source::Unknown;
        }
        let Some(place) = self.addrs.get(&s).copied() else {
            return Source::Unknown;
        };
        match self.object_range(place) {
            Some((start, _)) if self.outside_locals(place.local, 4) => Source::Field { start },
            _ => Source::Unknown,
        }
    }

    /// Whether pointer local `b` never holds the address of one of this function's locals: a
    /// parameter, or a register only ever written with call results, pointers loaded from
    /// memory (Velt stores no address of a local there) or copies of such registers.
    fn outside_locals(&self, b: Local, depth: u32) -> bool {
        if (b.0 as usize) < self.func.params.len() {
            return self.usage.get(b).defs == 1 && !self.usage.get(b).address_taken;
        }
        if depth == 0 || !self.usage.is_register(b) {
            return false;
        }
        let mut ok = true;
        for block in &self.func.blocks {
            for st in &block.stmts {
                if let Stmt::Assign(d, rv) = st {
                    if d.proj.is_empty() && d.local == b {
                        ok &= match rv {
                            Rvalue::Use(Operand::Copy(p)) if derefs(p) => true,
                            Rvalue::Use(Operand::Copy(p)) if p.proj.is_empty() => {
                                p.local != b && self.outside_locals(p.local, depth - 1)
                            }
                            _ => false,
                        };
                    }
                }
            }
        }
        ok
    }

    /// `(*p as O).f.g…` with `O` a class object's layout: the byte range of the place in the
    /// object.
    fn object_range(&self, place: &Place) -> Option<(u64, u64)> {
        let [Proj::Deref(Ty::Agg(o)), rest @ ..] = &place.proj[..] else {
            return None;
        };
        let layout = self.aggs.get(o.0 as usize)?;
        if !layout.name.ends_with(velt_vir::OBJECT_AGG_SUFFIX) {
            return None;
        }
        let mut ty = Ty::Agg(*o);
        let mut start = 0u64;
        for p in rest {
            let (Proj::Field(n), Ty::Agg(a)) = (p, ty) else {
                return None;
            };
            let (fty, off) = *self.aggs.get(a.0 as usize)?.fields.get(*n as usize)?;
            start += u64::from(off);
            ty = fty;
        }
        let size = match ty {
            Ty::Agg(a) => u64::from(self.aggs.get(a.0 as usize)?.size),
            t => u64::from(t.scalar_size()?),
        };
        Some((start, size))
    }

    /// Whether the code at `at` may change a source (see the module doc).
    fn clobbers(&self, f: &Family, sources: &[Source], at: Point) -> bool {
        // Writes to this function's locals cannot reach a source outside them.
        let known = (sources.iter()).all(|s| matches!(s, Source::Field { .. } | Source::Caller));
        let block = &self.func.blocks[at.0];
        let local_write =
            |l: Local| known || !self.usage.get(l).address_taken || f.members.contains(&l);
        let store = |pl: &Place| {
            if !derefs(pl) {
                return !local_write(pl.local);
            }
            let Some((start, size)) = self.object_range(pl) else {
                return true;
            };
            sources.iter().any(|s| match s {
                Source::Field { start: s0 } => start < s0 + 24 && *s0 < start + size,
                Source::Caller | Source::Unknown => true,
            })
        };
        match block.stmts.get(at.1) {
            Some(Stmt::Assign(dst, _)) => store(dst),
            Some(
                Stmt::MemCopy { dst, .. } | Stmt::MemCopyDyn { dst, .. } | Stmt::MemSet { dst, .. },
            ) => !(known && self.addr_of_local(dst).is_some()),
            Some(Stmt::Nop) => false,
            None => match &block.term {
                Terminator::Call {
                    callee, args, dest, ..
                } => {
                    if dest.as_ref().is_some_and(store) {
                        return true;
                    }
                    let rt = self.rt;
                    if rt.reads_only(callee) || rt.is(callee, rt.alloc) {
                        return false;
                    }
                    let local_arg = |i: usize| {
                        args.get(i)
                            .and_then(|a| self.addr_of_local(a))
                            .is_some_and(local_write)
                    };
                    if rt.is(callee, rt.clone) && args.len() == 2 {
                        return !local_arg(1);
                    }
                    if rt.is(callee, rt.drop) && args.len() == 1 {
                        return !local_arg(0);
                    }
                    // A slice allocates or shares, and writes only its output (`views.rs`).
                    if self.views && rt.is(callee, rt.slice) && args.len() == 4 {
                        return !local_arg(3);
                    }
                    // A string the caller holds stays put during this call (semantics.md "Borrows
                    // through counted objects": a borrowed argument is stabilized), so a call can
                    // change it only through a pointer that may lead to it: one that is not the
                    // address of a local of this function.
                    let caller = sources.iter().all(|s| matches!(s, Source::Caller));
                    if caller && !sources.is_empty() {
                        let leads = |a: &Operand| match a {
                            Operand::Copy(p) => {
                                derefs(p)
                                    || !self.local_addresses.contains(&p.local)
                                        && self.pointer_typed(p)
                            }
                            _ => false,
                        };
                        let target = matches!(callee, Callee::Ptr { .. });
                        return target || args.iter().any(leads);
                    }
                    true
                }
                _ => false,
            },
        }
    }
}

/// The registers that only ever hold the address of one of `func`'s locals: every definition is
/// `&x…` with `x` not a parameter, or a copy of such a register (the largest such set, so
/// registers copied around a loop count).
pub(super) fn local_address_registers(func: &Function, usage: &Usage) -> FxHashSet<Local> {
    let params = func.params.len();
    let register = |l: Local| (l.0 as usize) >= params && usage.is_register(l);
    // Each candidate's definitions: `None` for one that disqualifies it, else the register it
    // copies (if any).
    let mut copies: FxHashMap<Local, Vec<Local>> = FxHashMap::default();
    let mut out: FxHashSet<Local> = FxHashSet::default();
    let mut bad: FxHashSet<Local> = FxHashSet::default();
    for block in &func.blocks {
        for s in &block.stmts {
            let Stmt::Assign(d, rv) = s else { continue };
            if !d.proj.is_empty() || !register(d.local) {
                continue;
            }
            match rv {
                Rvalue::AddrOf(a) if (a.local.0 as usize) >= params && !derefs(a) => {}
                Rvalue::Use(Operand::Copy(q)) if q.proj.is_empty() && register(q.local) => {
                    copies.entry(d.local).or_default().push(q.local);
                }
                _ => {
                    bad.insert(d.local);
                }
            }
            out.insert(d.local);
        }
        if let Terminator::Call { dest: Some(d), .. } = &block.term {
            bad.insert(d.local);
        }
    }
    out.retain(|l| !bad.contains(l));
    loop {
        let before = out.len();
        let kept: Vec<Local> = (out.iter().copied())
            .filter(|l| copies.get(l).into_iter().flatten().all(|q| out.contains(q)))
            .collect();
        out = kept.into_iter().collect();
        if out.len() == before {
            return out;
        }
    }
}
