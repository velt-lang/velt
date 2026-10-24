//! Loops over a plain array of a view type run as in a program without views (#1485).
//!
//! On a view type every length, element access and growing push tests the view bit of the
//! array's header (array_view.rs), and the view side calls the prelude's helpers. A plain
//! array never takes that side, but the calls on it are in the loop, so the backend must assume
//! they write memory: it reloads the header's length and data on every step and checks the
//! bounds again.
//!
//! So a loop whose view tests read the header of a box held in a local the loop does not assign
//! is *versioned* when the function is finished: the loop is lowered once more as a copy in
//! which those tests are false, and a test before the loop picks the copy when the box is not a
//! view. That is sound because the bit of a box never gets set once the box exists: a view is
//! made only as a new box (`array_view_new`), and a plain array stays plain whatever is done to
//! it (a push reallocates its buffer, which is never at an odd address), while a view may become
//! plain (`view_materialize`), which the original loop, kept for views, handles as before.

use rustc_hash::{FxHashMap, FxHashSet};

use super::FnLower;
use crate::vir::{self, BlockId, Local, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// What lowering noted for [`FnLower::version_view_loops`].
#[derive(Default)]
pub(super) struct ViewLoops {
    /// The view bit tests: the `Bool` local holding the result, the local holding the box
    /// whose header it read, and the header's type.
    tests: Vec<(Local, Local, Ty)>,
    /// The loops lowered: their header (the block entered from before the loop) and the
    /// range of blocks made while lowering them.
    loops: Vec<(BlockId, u32, u32)>,
}

impl FnLower<'_, '_> {
    /// The test `is` reads the view bit of the header at `hdr`. A branch on it must go to its
    /// first target for a view (the copy of a versioned loop goes to the second).
    pub(super) fn note_view_test(&mut self, hdr: &Place, is: &Operand) {
        let ([Proj::Deref(t)], Operand::Copy(Place { local, proj })) = (&hdr.proj[..], is) else {
            return;
        };
        if proj.is_empty() && self.locals[hdr.local.0 as usize].ty == Ty::Ptr {
            self.view_loops.tests.push((*local, hdr.local, *t));
        }
    }

    /// The first block of a loop about to be lowered (for [`Self::note_loop`]).
    pub(super) fn loop_start(&self) -> u32 {
        self.blocks.len() as u32
    }

    /// A loop with header `header` was lowered into the blocks from `first` on.
    pub(super) fn note_loop(&mut self, header: BlockId, first: u32) {
        if !self.cx.boxing.array_views.is_empty() {
            let end = self.blocks.len() as u32;
            self.view_loops.loops.push((header, first, end));
        }
    }

    /// Version the loops of the module docs, outermost first; a loop inside one versioned for
    /// a box is not versioned for it again.
    pub(super) fn version_view_loops(&mut self, params: usize) {
        if self.view_loops.tests.is_empty() {
            return;
        }
        let mut loops = std::mem::take(&mut self.view_loops.loops);
        loops.sort_by_key(|&(_, first, end)| (first, std::cmp::Reverse(end)));
        let addrs = self.addresses();
        // The blocks using each test (the copies made below are in no loop's range).
        let tested: FxHashSet<Local> = self.view_loops.tests.iter().map(|x| x.0).collect();
        let mut uses: FxHashMap<Local, FxHashSet<BlockId>> = FxHashMap::default();
        for i in 0..self.blocks.len() {
            let b = BlockId(i as u32);
            for &t in &tested {
                if self.block_uses(b, t) {
                    uses.entry(t).or_default().insert(b);
                }
            }
        }
        let mut handled: FxHashSet<Local> = FxHashSet::default();
        for (h, first, end) in loops {
            let Some(region) = self.natural_loop(h, first, end) else {
                continue;
            };
            let used_here = |t: &Local| {
                uses.get(t)
                    .is_some_and(|bs| region.iter().any(|b| bs.contains(b)))
            };
            let mut seen = FxHashSet::default();
            let boxes: Vec<(Local, Ty)> = self
                .view_loops
                .tests
                .iter()
                .filter(|(t, _, _)| !handled.contains(t) && used_here(t))
                .filter(|(_, b, _)| seen.insert(*b))
                .filter(|(_, b, _)| !region.iter().any(|&r| self.block_assigns(r, *b)))
                .filter(|(_, b, _)| !self.written_through(*b, &region, &addrs))
                .map(|(_, b, t)| (*b, *t))
                .collect();
            if boxes.is_empty() {
                continue;
            }
            let plain: FxHashSet<Local> = self
                .view_loops
                .tests
                .iter()
                .filter(|(_, b, _)| boxes.iter().any(|(x, _)| x == b))
                .map(|(t, _, _)| *t)
                .collect();
            // The loops inside this one keep the tests of these boxes (in the original, the
            // copy has none).
            handled.extend(plain.iter().filter(|t| used_here(t)));
            self.version(h, &region, &boxes, &plain, params);
        }
    }

    /// The blocks of the loop with header `h` among the live blocks `first..end`: those
    /// reached from `h` that reach `h` again, when the loop is entered only through `h`.
    fn natural_loop(&self, h: BlockId, first: u32, end: u32) -> Option<Vec<BlockId>> {
        let inr = |b: BlockId| b.0 >= first && b.0 < end && self.live[b.0 as usize];
        if !inr(h) {
            return None;
        }
        let succ = |b: BlockId| match &self.blocks[b.0 as usize].1 {
            Some(t) => super::cfg::successors(t),
            None => vec![],
        };
        // Reached from the header.
        let mut fwd = FxHashSet::default();
        let mut work = vec![h];
        while let Some(b) = work.pop() {
            if fwd.insert(b) {
                work.extend(succ(b).into_iter().filter(|&s| inr(s)));
            }
        }
        // Of those, the ones reaching the header again.
        let mut back = FxHashSet::default();
        back.insert(h);
        let mut grew = true;
        while grew {
            grew = false;
            for &b in &fwd {
                if !back.contains(&b) && succ(b).iter().any(|s| back.contains(s)) {
                    back.insert(b);
                    grew = true;
                }
            }
        }
        if !fwd.iter().any(|&b| succ(b).contains(&h)) {
            return None;
        }
        // Entered only through the header.
        for (i, (_, t)) in self.blocks.iter().enumerate() {
            let b = BlockId(i as u32);
            if !self.live[i] || back.contains(&b) {
                continue;
            }
            let into = t.iter().flat_map(super::cfg::successors);
            if into.filter(|s| back.contains(s)).any(|s| s != h) {
                return None;
            }
        }
        let mut region: Vec<BlockId> = back.into_iter().collect();
        region.sort_by_key(|b| b.0);
        Some(region)
    }

    /// Does block `b` assign the test `t` or branch on it?
    fn block_uses(&self, b: BlockId, t: Local) -> bool {
        let (stmts, term) = &self.blocks[b.0 as usize];
        let assigns = stmts
            .iter()
            .any(|s| matches!(s, vir::Stmt::Assign(p, _) if p.local == t && p.proj.is_empty()));
        assigns || matches!(term, Some(Terminator::Branch { cond, .. }) if is_local(cond, t))
    }

    /// Does block `b` assign the local `l` (as a whole)?
    fn block_assigns(&self, b: BlockId, l: Local) -> bool {
        let (stmts, term) = &self.blocks[b.0 as usize];
        let whole = |p: &Place| p.local == l && p.proj.is_empty();
        stmts
            .iter()
            .any(|s| matches!(s, vir::Stmt::Assign(p, _) if whole(p)))
            || matches!(term, Some(Terminator::Call { dest: Some(p), .. }) if whole(p))
    }

    /// The locals whose own address is taken, each with the locals holding that address
    /// (through whole-local copies), or `None` when the address escapes: any other use of a
    /// holder than a whole-local copy or an access through it (a store into a projected place,
    /// an aggregate, cast or arithmetic operand, a call argument, a return, a derived address)
    /// may let code the loop calls reach the local.
    fn addresses(&self) -> FxHashMap<Local, Option<Vec<Local>>> {
        let mut out: FxHashMap<Local, Option<Vec<Local>>> = FxHashMap::default();
        let stmts = || self.blocks.iter().flat_map(|(s, _)| s.iter());
        for s in stmts() {
            let vir::Stmt::Assign(d, Rvalue::AddrOf(p)) = s else {
                continue;
            };
            if matches!(p.proj.first(), Some(Proj::Deref(_))) {
                continue;
            }
            let held = out.entry(p.local).or_insert_with(|| Some(vec![]));
            match (d.proj.is_empty(), held) {
                (true, Some(v)) => v.push(d.local),
                (_, held) => *held = None,
            }
        }
        for held in out.values_mut() {
            if let Some(v) = held {
                if self.address_escapes(v) {
                    *held = None;
                }
            }
        }
        out
    }

    /// Follow the whole-local copies of the addresses in `v` (adding the copies to it), and
    /// tell whether any holder is used in another way than [`Self::addresses`] allows.
    fn address_escapes(&self, v: &mut Vec<Local>) -> bool {
        let whole = |o: &Operand, v: &[Local]| matches!(o, Operand::Copy(p) if p.proj.is_empty() && v.contains(&p.local));
        let mut i = 0;
        while i < v.len() {
            i = v.len();
            for (stmts, term) in &self.blocks {
                for s in stmts {
                    match s {
                        vir::Stmt::Assign(d, Rvalue::Use(o)) if whole(o, v) => {
                            if !d.proj.is_empty() {
                                return true;
                            }
                            if !v.contains(&d.local) {
                                v.push(d.local);
                            }
                        }
                        vir::Stmt::Assign(_, Rvalue::AddrOf(p)) if v.contains(&p.local) => {
                            return true;
                        }
                        vir::Stmt::Assign(_, Rvalue::Unary(_, o) | Rvalue::Cast(o, _))
                            if whole(o, v) =>
                        {
                            return true;
                        }
                        vir::Stmt::Assign(_, Rvalue::Binary(_, a, b))
                            if whole(a, v) || whole(b, v) =>
                        {
                            return true;
                        }
                        vir::Stmt::Assign(_, Rvalue::Aggregate(_, os))
                            if os.iter().any(|o| whole(o, v)) =>
                        {
                            return true;
                        }
                        _ => {}
                    }
                }
                match term {
                    Some(Terminator::Call { callee, args, .. }) => {
                        let target =
                            matches!(callee, vir::Callee::Ptr { target, .. } if whole(target, v));
                        if target || args.iter().any(|o| whole(o, v)) {
                            return true;
                        }
                    }
                    Some(Terminator::Return(o)) if whole(o, v) => return true,
                    _ => {}
                }
            }
        }
        false
    }

    /// Might the loop `region` change the local `l` through its address?
    fn written_through(
        &self,
        l: Local,
        region: &[BlockId],
        addrs: &FxHashMap<Local, Option<Vec<Local>>>,
    ) -> bool {
        let Some(held) = addrs.get(&l) else {
            return false;
        };
        let Some(held) = held else {
            return true;
        };
        let mut seen = region.iter().any(|&b| {
            self.blocks[b.0 as usize].0.iter().any(|s| {
                matches!(s, vir::Stmt::Assign(_, Rvalue::AddrOf(p))
                    if p.local == l && !matches!(p.proj.first(), Some(Proj::Deref(_))))
            })
        });
        let mut note = |x: &mut Local| seen |= held.contains(x);
        for &b in region {
            let (mut stmts, mut term) = self.blocks[b.0 as usize].clone();
            stmts.iter_mut().for_each(|s| stmt_locals(s, &mut note));
            term.iter_mut().for_each(|t| term_locals(t, &mut note));
        }
        seen
    }

    /// Copy the loop `region` (header `h`) with the tests in `plain` false, and enter the copy
    /// from before the loop when none of `boxes` (their header type each) is a view.
    fn version(
        &mut self,
        h: BlockId,
        region: &[BlockId],
        boxes: &[(Local, Ty)],
        plain: &FxHashSet<Local>,
        params: usize,
    ) {
        let inside: FxHashSet<BlockId> = region.iter().copied().collect();
        let copies = self.blocks.len();
        let mut map = FxHashMap::default();
        for &b in region {
            let c = self.new_block();
            self.live[c.0 as usize] = true;
            map.insert(b, c);
        }
        // The copy's own locals: those only the loop mentions (not the parameters) are new in
        // the copy, so that each temporary keeps its single assignment.
        let mut outside = FxHashSet::default();
        for (i, (stmts, term)) in self.blocks.iter_mut().enumerate().take(copies) {
            if self.live[i] && !inside.contains(&BlockId(i as u32)) {
                let mut note = |l: &mut Local| {
                    outside.insert(*l);
                };
                stmts.iter_mut().for_each(|s| stmt_locals(s, &mut note));
                term.iter_mut().for_each(|t| term_locals(t, &mut note));
            }
        }
        let mut fresh = FxHashMap::default();
        for &b in region {
            let c = map[&b];
            let (mut stmts, term) = self.blocks[b.0 as usize].clone();
            let mut term = term;
            let mut rename = |l: &mut Local| {
                if l.0 as usize >= params && !outside.contains(l) {
                    *l = *fresh.entry(*l).or_insert_with(|| {
                        let d = self.locals[l.0 as usize].clone();
                        self.locals.push(d);
                        Local(self.locals.len() as u32 - 1)
                    });
                }
            };
            stmts.iter_mut().for_each(|s| stmt_locals(s, &mut rename));
            term.iter_mut().for_each(|t| term_locals(t, &mut rename));
            // A test of one of `boxes` (renamed or not) is false in the copy.
            let plain_here = |o: &Operand| {
                plain
                    .iter()
                    .any(|&p| is_local(o, p) || fresh.get(&p).is_some_and(|&q| is_local(o, q)))
            };
            let locs = self.block_locs[b.0 as usize].clone();
            let term = term.map(|t| {
                let t = match t {
                    Terminator::Branch { cond, els, .. } if plain_here(&cond) => {
                        Terminator::Goto(els)
                    }
                    t => t,
                };
                super::cfg::remap_term(t, |s| map.get(&s).copied().unwrap_or(s))
            });
            self.blocks[c.0 as usize] = (stmts, term);
            self.block_locs[c.0 as usize] = locs;
        }
        // The test before the loop, entered by every jump into it from outside.
        let entry = self.new_block();
        for i in 0..copies {
            if !self.live[i] || inside.contains(&BlockId(i as u32)) {
                continue;
            }
            if let Some(t) = self.blocks[i].1.take() {
                let t = super::cfg::remap_term(t, |s| if s == h { entry } else { s });
                self.blocks[i].1 = Some(t);
            }
        }
        self.live[entry.0 as usize] = true;
        self.loc = self.block_locs[h.0 as usize].1;
        let mut at = entry;
        for (l, t) in boxes {
            self.switch_to(at);
            let bx = Operand::Copy(Place::local(*l));
            let some = self.non_null(bx);
            let test = self.new_block();
            self.branch(some, test, h);
            self.switch_to(test);
            let hdr = Place {
                local: *l,
                proj: vec![Proj::Deref(*t)],
            };
            let is = self.proxy_tagged(Operand::Copy(super::operand::proj(&hdr, Proj::Field(0))));
            at = self.new_block();
            self.branch(is, h, at);
        }
        self.switch_to(at);
        self.goto(map[&h]);
    }
}

fn is_local(o: &Operand, l: Local) -> bool {
    matches!(o, Operand::Copy(Place { local, proj }) if *local == l && proj.is_empty())
}

/// Visit every local `s` mentions.
fn stmt_locals(s: &mut vir::Stmt, f: &mut impl FnMut(&mut Local)) {
    match s {
        vir::Stmt::Assign(p, rv) => {
            f(&mut p.local);
            match rv {
                Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Cast(o, _) => op_locals(o, f),
                Rvalue::Binary(_, a, b) => {
                    op_locals(a, f);
                    op_locals(b, f);
                }
                Rvalue::AddrOf(p) => f(&mut p.local),
                Rvalue::Aggregate(_, os) => os.iter_mut().for_each(|o| op_locals(o, f)),
            }
        }
        vir::Stmt::MemCopy { dst, src, .. } => {
            op_locals(dst, f);
            op_locals(src, f);
        }
        vir::Stmt::MemCopyDyn { dst, src, len, .. } => {
            op_locals(dst, f);
            op_locals(src, f);
            op_locals(len, f);
        }
        vir::Stmt::MemSet { dst, byte, len } => {
            op_locals(dst, f);
            op_locals(byte, f);
            op_locals(len, f);
        }
        vir::Stmt::Nop => {}
    }
}

/// Visit every local `t` mentions.
fn term_locals(t: &mut Terminator, f: &mut impl FnMut(&mut Local)) {
    match t {
        Terminator::Branch { cond: o, .. }
        | Terminator::Switch { value: o, .. }
        | Terminator::Return(o) => op_locals(o, f),
        Terminator::Call {
            callee, args, dest, ..
        } => {
            if let vir::Callee::Ptr { target, .. } = callee {
                op_locals(target, f);
            }
            args.iter_mut().for_each(|o| op_locals(o, f));
            if let Some(p) = dest {
                f(&mut p.local);
            }
        }
        Terminator::Goto(_) | Terminator::Unreachable => {}
    }
}

fn op_locals(o: &mut Operand, f: &mut impl FnMut(&mut Local)) {
    if let Operand::Copy(p) = o {
        f(&mut p.local);
    }
}
