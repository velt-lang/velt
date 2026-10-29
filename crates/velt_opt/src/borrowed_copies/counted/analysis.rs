//! A family's members, count pointers and releases, and the checks of the module doc.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_vir::vir::{BinOp, BlockId, Local, Operand, Place, Proj, Rvalue, Stmt, Terminator};

use super::{count_word, is_int, local_of, Cx, Family, Point, Release};
use crate::visit::{rvalue_operands, stmt_operands, successors, term_operands};

/// How the code at one point treats the family.
#[derive(Default)]
struct Walk {
    /// Points that use the object (read or write through a member, pass a member to a call,
    /// compare it with anything but null).
    uses: FxHashSet<Point>,
    bail: bool,
}

impl Cx<'_> {
    /// The family whose increment is the store at `inc` through `r`'s count pointer.
    pub(super) fn family(&self, r: Local, inc: Point) -> Option<Family> {
        let mut members: FxHashSet<Local> = [r].into_iter().collect();
        loop {
            let before = members.len();
            for s in self.func.blocks.iter().flat_map(|b| &b.stmts) {
                if let Stmt::Assign(d, Rvalue::Use(op)) = s {
                    if d.proj.is_empty() && local_of(op).is_some_and(|m| members.contains(&m)) {
                        members.insert(d.local);
                    }
                }
            }
            if members.len() == before {
                break;
            }
        }
        let params = self.func.params.len();
        let glue_addrs = self.glue_addrs(&members)?;
        let derived = self.derived(&members)?;
        for &m in &members {
            let scalar = self.func.locals[m.0 as usize].ty == velt_vir::vir::Ty::Ptr;
            let ok = (m.0 as usize) >= params
                && (self.usage.is_register(m) || (scalar && self.usage.get(m).address_taken));
            if !ok {
                return None;
            }
        }
        let counts: FxHashSet<Local> = (self.defs.keys())
            .copied()
            .filter(|&q| self.count_base(q).is_some_and(|b| members.contains(&b)))
            .collect();
        let mut fam = Family {
            root: r,
            members,
            glue_addrs,
            derived,
            counts,
            inc,
            releases: vec![],
            count_stmts: FxHashSet::default(),
        };
        // The increment's `add`, when it is a register of its own.
        if let Some(Rvalue::Use(op)) = self.rvalue_at(inc) {
            fam.count_stmts.insert(*self.defs.get(&local_of(op)?)?);
        }
        for b in 0..self.func.blocks.len() {
            if let Some(r) = self.release(&mut fam, b) {
                fam.releases.push(r);
            }
        }
        Some(fam)
    }

    /// Whether `s` writes a member with the root's own load.
    fn loads_member(&self, fam: &Family, s: &Stmt) -> bool {
        matches!(s, Stmt::Assign(d, rv) if d.proj.is_empty()
            && fam.members.contains(&d.local)
            && (d.local == fam.root || self.repeats_root_load(fam, rv)))
    }

    /// Whether `rv` is the root's own load, `*p` of a register `p` written once.
    fn repeats_root_load(&self, fam: &Family, rv: &Rvalue) -> bool {
        let Some((_, root_rv)) = self.def_of(fam.root) else {
            return false;
        };
        let Rvalue::Use(Operand::Copy(src)) = root_rv else {
            return false;
        };
        let stable_base = matches!(src.proj.as_slice(), [Proj::Deref(_)])
            && self.usage.is_register(src.local)
            && self.usage.get(src.local).defs == 1;
        stable_base && rv == root_rv
    }

    /// The registers taking a member's address, when every such address goes only to release
    /// glue as its one argument (`None` when some address goes elsewhere).
    fn glue_addrs(&self, members: &FxHashSet<Local>) -> Option<FxHashSet<Local>> {
        let mut addrs = FxHashSet::default();
        for s in self.func.blocks.iter().flat_map(|b| &b.stmts) {
            let Stmt::Assign(d, Rvalue::AddrOf(p)) = s else {
                continue;
            };
            if !members.contains(&p.local) || !p.proj.is_empty() {
                continue;
            }
            let reg = self.usage.is_register(d.local) && self.usage.get(d.local).defs == 1;
            if !p.proj.is_empty() || !d.proj.is_empty() || !reg {
                return None;
            }
            addrs.insert(d.local);
        }
        let mut glue_reads: FxHashMap<Local, u32> = FxHashMap::default();
        for b in &self.func.blocks {
            if let Some(a) = self.glue_arg(&b.term).filter(|a| addrs.contains(a)) {
                *glue_reads.entry(a).or_default() += 1;
            }
        }
        let only_glue = addrs
            .iter()
            .all(|a| glue_reads.get(a).copied().unwrap_or(0) == self.usage.get(*a).reads);

        only_glue.then_some(addrs)
    }

    /// The registers written once with the address of a part of a member's object
    /// (`&(*m).rows`): pointers into it, whose uses are uses of the object (`None` when such an
    /// address goes into another kind of local).
    fn derived(&self, members: &FxHashSet<Local>) -> Option<FxHashSet<Local>> {
        let mut out = FxHashSet::default();
        for s in self.func.blocks.iter().flat_map(|b| &b.stmts) {
            let Stmt::Assign(d, Rvalue::AddrOf(p)) = s else {
                continue;
            };
            if !members.contains(&p.local) || !matches!(p.proj.first(), Some(Proj::Deref(_))) {
                continue;
            }
            let reg = self.usage.is_register(d.local) && self.usage.get(d.local).defs == 1;
            if !reg || !d.proj.is_empty() {
                return None;
            }
            out.insert(d.local);
        }
        Some(out)
    }

    /// `call g(a)` of release glue `g` with no result: `a`.
    fn glue_arg(&self, t: &Terminator) -> Option<Local> {
        let Terminator::Call {
            callee: velt_vir::vir::Callee::Func(g),
            args,
            dest: None,
            ..
        } = t
        else {
            return None;
        };
        let glue = self.frees.glue.get(g.0 as usize).copied().unwrap_or(false);
        match args.as_slice() {
            [a] if glue => local_of(a),
            _ => None,
        }
    }

    /// Block `b` as a release of `fam`: it branches on `count == 1`, its other side stores
    /// `count - 1`, or it calls release glue with a member's address. Records the statements
    /// computing them.
    fn release(&self, fam: &mut Family, b: usize) -> Option<Release> {
        if let Some(a) = self.glue_arg(&self.func.blocks[b].term) {
            let Terminator::Call { next, .. } = self.func.blocks[b].term else {
                return None;
            };
            return fam.glue_addrs.contains(&a).then_some(Release {
                test: b,
                dec: next,
                store: None,
            });
        }
        let Terminator::Branch { cond, els, .. } = &self.func.blocks[b].term else {
            return None;
        };
        let (test_at, test) = self.def_of(local_of(cond)?)?;
        let Rvalue::Binary(BinOp::Eq, count, one) = test else {
            return None;
        };
        if !is_int(one, 1) {
            return None;
        }
        let mut stmts = vec![test_at];
        let q = match count {
            Operand::Copy(p) if count_word(p).is_some() => p.local,
            op => {
                let (at, rv) = self.def_of(local_of(op)?)?;
                let Rvalue::Use(Operand::Copy(p)) = rv else {
                    return None;
                };
                stmts.push(at);
                count_word(p)?
            }
        };
        if !fam.counts.contains(&q) {
            return None;
        }
        let dec = &self.func.blocks[els.0 as usize];
        let (store, sub_at) = dec.stmts.iter().enumerate().find_map(|(si, s)| {
            let Stmt::Assign(dst, rv) = s else {
                return None;
            };
            if !count_word(dst).is_some_and(|q2| fam.counts.contains(&q2)) {
                return None;
            }
            let (sub_at, sub) = match rv {
                Rvalue::Use(op) => {
                    let (at, rv) = self.def_of(local_of(op)?)?;
                    (Some(at), rv)
                }
                rv => (None, rv),
            };
            matches!(sub, Rvalue::Binary(BinOp::Sub, _, o) if is_int(o, 1)).then_some((si, sub_at))
        })?;
        fam.count_stmts.extend(stmts.into_iter().chain(sub_at));
        Some(Release {
            test: b,
            dec: *els,
            store: Some((els.0 as usize, store)),
        })
    }

    /// The uses of `fam`'s object when everything but its calls allows it to borrow (module
    /// doc); its calls are checked by [`Cx::unfreed`] once every family is known.
    pub(super) fn check(&self, fam: &Family) -> Option<FxHashSet<Point>> {
        let cut = cut_of(std::iter::once(fam));
        let reach = self.reachable(&cut);
        let mut w = Walk::default();
        for (b, block) in self.func.blocks.iter().enumerate() {
            if !reach[b] {
                continue;
            }
            for (si, s) in block.stmts.iter().enumerate() {
                self.stmt(fam, &mut w, (b, si), s);
            }
            if fam.release_at(b).is_none() {
                self.term(fam, &mut w, (b, block.stmts.len()), &block.term);
            }
        }
        (!w.bail && self.balanced(fam, &reach)).then_some(w.uses)
    }

    /// The blocks reachable once each release of `cut` goes straight to its decrement.
    pub(super) fn reachable(&self, cut: &Cut) -> Vec<bool> {
        let mut seen = vec![false; self.func.blocks.len()];
        let mut work = vec![0usize];
        while let Some(b) = work.pop() {
            if !std::mem::replace(&mut seen[b], true) {
                work.extend(self.next_blocks(cut, b));
            }
        }
        seen
    }

    /// The successors of block `b` once the releases of `cut` go straight to their decrements.
    fn next_blocks(&self, cut: &Cut, b: usize) -> Vec<usize> {
        match cut.get(&b) {
            Some(dec) => vec![dec.0 as usize],
            None => (successors(&self.func.blocks[b].term).iter())
                .map(|s| s.0 as usize)
                .collect(),
        }
    }

    fn explained(&self, fam: &Family, at: Point) -> bool {
        at == fam.inc
            || fam.count_stmts.contains(&at)
            || fam.releases.iter().any(|r| r.store == Some(at))
    }

    fn stmt(&self, fam: &Family, w: &mut Walk, at: Point, s: &Stmt) {
        if self.explained(fam, at) {
            return;
        }
        match s {
            Stmt::Assign(dst, rv) => self.assign(fam, w, at, dst, rv),
            Stmt::Nop => {}
            _ => stmt_operands(s, &mut |op| self.operand(fam, w, at, op, false)),
        }
    }

    fn assign(&self, fam: &Family, w: &mut Walk, at: Point, dst: &Place, rv: &Rvalue) {
        let d = dst.local;
        if fam.counts.contains(&d) {
            // A count pointer's own definition (`q = ptradd m, -8`) is the only write.
            w.bail |= !dst.proj.is_empty() || self.count_base(d).is_none();
            return;
        }
        if fam.members.contains(&d) {
            match dst.proj.first() {
                None => {
                    // The root's definition (a load), the null of a drop flag, a copy, or the
                    // plain copy of a nullable field the root's load repeats once it found the
                    // field set (`c = this.cursor; if (c != null) { r = this.cursor; r.count++;
                    // c = r }`): the same pointer, or null.
                    let root = d == fam.root;
                    let ok = root
                        || self.repeats_root_load(fam, rv)
                        || match rv {
                            Rvalue::Use(op) if is_int(op, 0) => true,
                            Rvalue::Use(op) => {
                                local_of(op).is_some_and(|m| fam.members.contains(&m))
                            }
                            _ => false,
                        };
                    w.bail |= !ok;
                    if root {
                        rvalue_operands(rv, &mut |op| self.operand(fam, w, at, op, false));
                    }
                    return;
                }
                Some(Proj::Deref(_)) => {
                    w.uses.insert(at);
                }
                Some(_) => w.bail = true,
            }
        }
        if fam.derived.contains(&d) && !dst.proj.is_empty() {
            // A write through a pointer into the object: a use of it.
            w.uses.insert(at);
        }
        if let Rvalue::AddrOf(p) = rv {
            // The address release glue is given, or a pointer into the object (a use).
            let glue = fam.glue_addrs.contains(&d) && fam.members.contains(&p.local);
            let inside = fam.derived.contains(&d) && fam.members.contains(&p.local);
            if inside {
                w.uses.insert(at);
            }
            w.bail |= !glue
                && !inside
                && (fam.members.contains(&p.local) || fam.counts.contains(&p.local));
            return;
        }
        // A member's value may only be copied into a member, offset to its count word, or
        // compared; stored into memory, it escapes.
        let value_ok = match rv {
            Rvalue::Use(_) => dst.proj.is_empty() && fam.members.contains(&d),
            Rvalue::Binary(BinOp::PtrAdd, _, off) => is_int(off, -8) && fam.counts.contains(&d),
            Rvalue::Binary(op, a, b) if is_compare(*op) => {
                // Compared with null: a drop's test, not a use of the object.
                let member = |o: &Operand| local_of(o).is_some_and(|m| fam.members.contains(&m));
                if (member(a) || member(b)) && !(is_int(a, 0) || is_int(b, 0)) {
                    w.uses.insert(at);
                }
                true
            }
            _ => false,
        };
        rvalue_operands(rv, &mut |op| self.operand(fam, w, at, op, value_ok));
    }

    /// `op` read at `at`; `value_ok`: the statement may read a member's value itself.
    fn operand(&self, fam: &Family, w: &mut Walk, at: Point, op: &Operand, value_ok: bool) {
        let Operand::Copy(p) = op else { return };
        if fam.counts.contains(&p.local) {
            w.bail = true;
        } else if fam.members.contains(&p.local) || fam.derived.contains(&p.local) {
            if fam.derived.contains(&p.local) {
                // Read through or handed on: a use of the object either way.
                w.uses.insert(at);
            }
            match p.proj.first() {
                None => w.bail |= !value_ok,
                Some(Proj::Deref(_)) => {
                    w.uses.insert(at);
                }
                Some(_) => w.bail = true,
            }
        }
    }

    fn term(&self, fam: &Family, w: &mut Walk, at: Point, t: &Terminator) {
        match t {
            Terminator::Call {
                callee, args, dest, ..
            } => {
                if let velt_vir::vir::Callee::Ptr { target, .. } = callee {
                    self.operand(fam, w, at, target, false);
                }
                for a in args {
                    // Passed to the call, which borrows it.
                    self.operand(fam, w, at, a, true);
                    if local_of(a)
                        .is_some_and(|m| fam.members.contains(&m) || fam.derived.contains(&m))
                    {
                        w.uses.insert(at);
                    }
                }
                if let Some(d) = dest {
                    if fam.members.contains(&d.local) || fam.counts.contains(&d.local) {
                        w.bail |= d.proj.is_empty() || !matches!(d.proj[0], Proj::Deref(_));
                        w.uses.insert(at);
                    }
                }
            }
            t => term_operands(t, &mut |op| self.operand(fam, w, at, op, false)),
        }
    }

    /// Every path from the increment to a return passes a release (module doc), and the
    /// increment never runs again before one.
    fn balanced(&self, fam: &Family, reach: &[bool]) -> bool {
        let n = self.func.blocks.len();
        // Whether the reference may be held on entry to each block.
        let mut held_in = vec![false; n];
        let mut work: Vec<usize> = (0..n).filter(|&b| reach[b]).collect();
        while let Some(b) = work.pop() {
            let mut held = held_in[b];
            for (si, s) in self.func.blocks[b].stmts.iter().enumerate() {
                if (b, si) == fam.inc {
                    if held {
                        return false;
                    }
                    held = true;
                } else if let Stmt::Assign(d, _) = s {
                    // A member (or the root) written while it holds the reference.
                    if d.proj.is_empty() && fam.members.contains(&d.local) && held {
                        let copy = matches!(s, Stmt::Assign(_, Rvalue::Use(op))
                            if local_of(op).is_some_and(|m| fam.members.contains(&m)));
                        if !copy {
                            return false;
                        }
                    }
                }
            }
            let term = &self.func.blocks[b].term;
            if held && matches!(term, Terminator::Return(_)) {
                return false;
            }
            for (s, h) in self.out_edges(fam, b, held) {
                if h && !held_in[s] {
                    held_in[s] = true;
                    work.push(s);
                }
            }
        }
        true
    }

    /// The successors of block `b` and whether the reference may be held on each edge.
    fn out_edges(&self, fam: &Family, b: usize, held: bool) -> Vec<(usize, bool)> {
        if let Some(r) = fam.release_at(b) {
            return vec![(r.dec.0 as usize, false)];
        }
        let term = &self.func.blocks[b].term;
        let null_edge = match term {
            Terminator::Branch { cond, then, els } => self.null_test(fam, cond).map(|ne| {
                // `m != null` is false (or `m == null` true) on this edge: nothing is held.
                if ne {
                    els.0 as usize
                } else {
                    then.0 as usize
                }
            }),
            _ => None,
        };
        successors(term)
            .iter()
            .map(|s| (s.0 as usize, held && Some(s.0 as usize) != null_edge))
            .collect()
    }

    /// `cond` as `m != null` (true) or `m == null` (false) of a member `m`.
    fn null_test(&self, fam: &Family, cond: &Operand) -> Option<bool> {
        let (_, rv) = self.def_of(local_of(cond)?)?;
        let Rvalue::Binary(op, a, b) = rv else {
            return None;
        };
        let member = |o: &Operand| local_of(o).is_some_and(|m| fam.members.contains(&m));
        let tests = (member(a) && is_int(b, 0)) || (member(b) && is_int(a, 0));
        match op {
            BinOp::Ne if tests => Some(true),
            BinOp::Eq if tests => Some(false),
            _ => None,
        }
    }

    /// No call that may free runs while `fam`'s object is still used after it, or is handed the
    /// object (module doc), once the releases of `cut` (every family borrowed with it) no
    /// longer free.
    pub(super) fn unfreed(
        &self,
        fam: &Family,
        cut: &Cut,
        reach: &[bool],
        uses: &FxHashSet<Point>,
    ) -> bool {
        let n = self.func.blocks.len();
        let mut preds = vec![vec![]; n];
        for b in (0..n).filter(|&b| reach[b]) {
            for s in self.next_blocks(cut, b) {
                preds[s].push(b);
            }
        }
        // Whether the object may be used after the start of each block (before its next
        // increment).
        let mut live_in = vec![false; n];
        let mut work: Vec<usize> = (0..n).filter(|&b| reach[b]).collect();
        while let Some(b) = work.pop() {
            let mut live = self.next_blocks(cut, b).iter().any(|&s| live_in[s]);
            let block = &self.func.blocks[b];
            live |= uses.contains(&(b, block.stmts.len()));
            for si in (0..block.stmts.len()).rev() {
                // Before the increment, or before the load the copy starts from (the root's, or
                // its repeat for a nullable field, which the increment follows when the field
                // is set), the copy holds no object yet.
                if (b, si) == fam.inc || self.loads_member(fam, &block.stmts[si]) {
                    live = false;
                } else if uses.contains(&(b, si)) {
                    live = true;
                }
            }
            if live && !live_in[b] {
                live_in[b] = true;
                work.extend(preds[b].iter().copied());
            }
        }
        // A release of a family borrowed with this one (a glue call) goes away too.
        (0..n)
            .filter(|&b| reach[b] && !cut.contains_key(&b))
            .all(|b| {
                let block = &self.func.blocks[b];
                let Terminator::Call { callee, next, .. } = &block.term else {
                    return true;
                };
                let handed = uses.contains(&(b, block.stmts.len()));
                let used = handed || live_in[next.0 as usize];
                !used || !self.frees.may_free(callee, handed)
            })
    }
}

/// Release tests that go straight to their decrements: test block → decrement block.
pub(super) type Cut = FxHashMap<usize, BlockId>;

/// The releases of `fams`.
pub(super) fn cut_of<'a>(fams: impl Iterator<Item = &'a Family>) -> Cut {
    fams.flat_map(|f| f.releases.iter().map(|r| (r.test, r.dec)))
        .collect()
}

fn is_compare(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    )
}
