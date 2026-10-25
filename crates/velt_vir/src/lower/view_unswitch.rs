//! Loops over a plain array of a view type run as in a program without views (#1485).
//!
//! On a view type every length, element access and growing push tests the view bit of the
//! array's header (array_view.rs), and the view side calls the prelude's helpers. A plain
//! array never takes that side, but the calls on it are in the loop, so the backend must assume
//! they write memory: it reloads the header's length and data on every step and checks the
//! bounds again.
//!
//! So every loop with view tests is *versioned* when the function is finished: the loop is
//! lowered once more as a copy without the view side, which the original loop, kept for views,
//! still has.
//! - A test of a box held in a local the loop does not change (by assignment, or through the
//!   local's address, which must not escape: `addresses`) is false in the copy, and a
//!   test before the loop picks the copy when the box is not a view. That is sound because the
//!   bit of a box never gets set once the box exists: a view is made only as a new box
//!   (`array_view_new`), and a plain array stays plain whatever is done to it (a push
//!   reallocates its buffer, which is never at an odd address), while a view may become plain
//!   (`view_materialize`).
//! - Any other test (a box in an object field, an element, a variable the loop or a closure may
//!   assign) stays in the copy, but when it finds a view the copy goes on in the original loop
//!   at the same point, its locals handed over. The copy then has no call on the view side, so
//!   the backend keeps a plain array's header in registers there.
//! - An element access whose index is not below the header's length (a view's is 0) goes on
//!   in the original there too, whatever its box. So the copy reads only a plain array's
//!   element, with no way on from the test where the index may be past the length: the index
//!   is known to be below it after the test, which lets `velt_opt` keep it an integer.

use rustc_hash::{FxHashMap, FxHashSet};

use super::FnLower;
use crate::vir::{self, BlockId, Local, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// What lowering noted for [`FnLower::version_view_loops`].
#[derive(Default)]
pub(super) struct ViewLoops {
    /// The view bit tests: the `Bool` local holding the result, the local holding the box
    /// whose header it read with the header's type (when a local holds it).
    tests: Vec<(Local, Option<(Local, Ty)>)>,
    /// The tests of an element access's index against the header's length, whose false side
    /// tests the view bit (array_view_index.rs).
    slows: FxHashSet<Local>,
    /// The loops lowered: their header (the block entered from before the loop) and the
    /// range of blocks made while lowering them.
    loops: Vec<(BlockId, u32, u32)>,
}

impl FnLower<'_, '_> {
    /// The test `is` reads the view bit of the header at `hdr`. A branch on it must go to its
    /// first target for a view (the copy of a versioned loop goes to the second).
    pub(super) fn note_view_test(&mut self, hdr: &Place, is: &Operand) {
        let Operand::Copy(Place { local, proj }) = is else {
            return;
        };
        if !proj.is_empty() {
            return;
        }
        let held = match &hdr.proj[..] {
            [Proj::Deref(t)] if self.locals[hdr.local.0 as usize].ty == Ty::Ptr => {
                Some((hdr.local, *t))
            }
            _ => None,
        };
        self.view_loops.tests.push((*local, held));
    }

    /// The test `inb` is an element access's `i < len`, whose false side tests the view bit.
    pub(super) fn note_view_slow(&mut self, inb: &Operand) {
        if let Operand::Copy(Place { local, proj }) = inb {
            if proj.is_empty() {
                self.view_loops.slows.insert(*local);
            }
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
        let mut tested: FxHashSet<Local> = self.view_loops.tests.iter().map(|x| x.0).collect();
        tested.extend(self.view_loops.slows.iter().copied());
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
            let here: Vec<(Local, Option<(Local, Ty)>)> = self
                .view_loops
                .tests
                .iter()
                .filter(|(t, _)| !handled.contains(t) && used_here(t))
                .copied()
                .collect();
            if here.is_empty() {
                continue;
            }
            let mut seen = FxHashSet::default();
            let boxes: Vec<(Local, Ty)> = here
                .iter()
                .filter_map(|(_, held)| *held)
                .filter(|(b, _)| seen.insert(*b))
                .filter(|(b, _)| !region.iter().any(|&r| self.block_assigns(r, *b)))
                .filter(|(b, _)| !self.written_through(*b, &region, &addrs))
                .collect();
            type Test = (Local, Option<(Local, Ty)>);
            let (plain, leave): (Vec<&Test>, Vec<_>) = here.iter().partition(|(_, held)| {
                held.is_some_and(|(b, _)| boxes.iter().any(|(x, _)| *x == b))
            });
            let plain: FxHashSet<Local> = plain.iter().map(|x| x.0).collect();
            let leave: FxHashSet<Local> = leave.iter().map(|x| x.0).collect();
            // The loops inside this one are versioned with it (the copy holds theirs, and the
            // original runs only for a view).
            handled.extend(here.iter().map(|x| x.0));
            let slows: FxHashSet<Local> = (self.view_loops.slows.iter())
                .filter(|t| !handled.contains(t) && used_here(t))
                .copied()
                .collect();
            handled.extend(slows.iter().copied());
            let tests = Tests {
                plain,
                leave,
                slows,
            };
            self.version(h, &region, &boxes, &tests, params);
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

    /// Copy the loop `region` (header `h`) with its tests as `tests` says, and enter the copy
    /// from before the loop when none of `boxes` (their header type each) is a view.
    fn version(
        &mut self,
        h: BlockId,
        region: &[BlockId],
        boxes: &[(Local, Ty)],
        tests: &Tests,
        params: usize,
    ) {
        let Tests {
            plain,
            leave,
            slows,
        } = tests;
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
        let mut copied = vec![];
        for &b in region {
            let (mut stmts, mut term) = self.blocks[b.0 as usize].clone();
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
            copied.push((b, stmts, term));
        }
        let named = |set: &FxHashSet<Local>, o: &Operand| {
            set.iter()
                .any(|&p| is_local(o, p) || fresh.get(&p).is_some_and(|&q| is_local(o, q)))
        };
        // Where the copy goes on in the original, the original's locals take the values of the
        // copy's that are live there (and assigned on every way there).
        let renamed: FxHashSet<Local> = fresh.keys().copied().collect();
        let (live, set) = match leave.is_empty() && slows.is_empty() {
            true => (FxHashMap::default(), FxHashMap::default()),
            false => (
                self.live_ins(region, &renamed),
                self.assigned_outs(h, region, &renamed),
            ),
        };
        let mut edges = vec![];
        for (b, stmts, term) in copied {
            let c = map[&b];
            let locs = self.block_locs[b.0 as usize].clone();
            // A block going on in the original at `to` from the end of `b` in the copy: the
            // original's locals live there take the values of the copy's.
            let mut hand_over = |this: &mut Self, to: BlockId| {
                let mut back: Vec<Local> = live
                    .get(&to)
                    .into_iter()
                    .flatten()
                    .filter(|l| set.get(&b).is_some_and(|s| s.contains(l)))
                    .copied()
                    .collect();
                back.sort_by_key(|l| l.0);
                let stmts: Vec<vir::Stmt> = back
                    .iter()
                    .map(|l| {
                        let from = Operand::Copy(Place::local(fresh[l]));
                        vir::Stmt::Assign(Place::local(*l), Rvalue::Use(from))
                    })
                    .collect();
                let edge = this.new_block();
                this.live[edge.0 as usize] = true;
                let n = stmts.len();
                edges.push((edge, to));
                this.blocks[edge.0 as usize] = (stmts, None);
                this.block_locs[edge.0 as usize] = (vec![locs.1; n], locs.1);
                edge
            };
            let term = match term {
                // A test of one of `boxes` is false in the copy.
                Some(Terminator::Branch { cond, els, .. }) if named(plain, &cond) => {
                    Some(Terminator::Goto(els))
                }
                Some(Terminator::Branch { cond, then, els }) if named(leave, &cond) => {
                    let then = hand_over(self, then);
                    Some(Terminator::Branch { cond, then, els })
                }
                // Past the header's length: a view, or an error on a plain array.
                Some(Terminator::Branch { cond, then, els }) if named(slows, &cond) => {
                    let els = hand_over(self, els);
                    Some(Terminator::Branch { cond, then, els })
                }
                t => t,
            };
            let term =
                term.map(|t| super::cfg::remap_term(t, |s| map.get(&s).copied().unwrap_or(s)));
            self.blocks[c.0 as usize] = (stmts, term);
            self.block_locs[c.0 as usize] = locs;
        }
        for (edge, then) in edges {
            self.blocks[edge.0 as usize].1 = Some(Terminator::Goto(then));
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

    /// The locals of `cands` live into each of the `blocks` (those of a loop when only the loop
    /// mentions them, or all of them).
    fn live_ins(
        &self,
        region: &[BlockId],
        cands: &FxHashSet<Local>,
    ) -> FxHashMap<BlockId, FxHashSet<Local>> {
        let mut live: FxHashMap<BlockId, FxHashSet<Local>> =
            region.iter().map(|&b| (b, FxHashSet::default())).collect();
        let mut grew = true;
        while grew {
            grew = false;
            for &b in region.iter().rev() {
                let (stmts, term) = &self.blocks[b.0 as usize];
                let mut l = FxHashSet::default();
                if let Some(t) = term {
                    for s in super::cfg::successors(t) {
                        l.extend(live.get(&s).into_iter().flatten().copied());
                    }
                    term_flow(t, &mut l, cands);
                }
                for s in stmts.iter().rev() {
                    stmt_flow(s, &mut l, cands);
                }
                if l.len() != live[&b].len() {
                    grew = true;
                    live.insert(b, l);
                }
            }
        }
        live
    }

    /// The locals of `cands` (which only the loop `region` with header `h` mentions) assigned
    /// on every way from the header to the end of each block of the loop, as the verifier
    /// counts assignments (vir invariant 6).
    fn assigned_outs(
        &self,
        h: BlockId,
        region: &[BlockId],
        cands: &FxHashSet<Local>,
    ) -> FxHashMap<BlockId, FxHashSet<Local>> {
        let inside: FxHashSet<BlockId> = region.iter().copied().collect();
        let mut preds: FxHashMap<BlockId, Vec<BlockId>> = FxHashMap::default();
        for &b in region {
            if let Some(t) = &self.blocks[b.0 as usize].1 {
                for s in super::cfg::successors(t) {
                    if inside.contains(&s) {
                        preds.entry(s).or_default().push(b);
                    }
                }
            }
        }
        let mut out: FxHashMap<BlockId, FxHashSet<Local>> =
            region.iter().map(|&b| (b, cands.clone())).collect();
        let mut changed = true;
        while changed {
            changed = false;
            for &b in region {
                let mut set = match b == h {
                    true => FxHashSet::default(),
                    false => {
                        let mut ps = preds.get(&b).into_iter().flatten();
                        let first = ps.next().map(|p| out[p].clone()).unwrap_or_default();
                        ps.fold(first, |acc, p| &acc & &out[p])
                    }
                };
                let (stmts, term) = &self.blocks[b.0 as usize];
                for s in stmts {
                    if let vir::Stmt::Assign(p, rv) = s {
                        assigns(p, cands, &mut set);
                        if let Rvalue::AddrOf(a) = rv {
                            assigns(a, cands, &mut set);
                        }
                    }
                }
                if let Some(Terminator::Call { dest: Some(p), .. }) = term {
                    assigns(p, cands, &mut set);
                }
                if set.len() != out[&b].len() {
                    changed = true;
                    out.insert(b, set);
                }
            }
        }
        out
    }
}

/// The tests in a loop's copy: false there (`plain`), going on in the original when true
/// (`leave`), and element accesses' `i < len` going on in the original when false (`slows`).
struct Tests {
    plain: FxHashSet<Local>,
    leave: FxHashSet<Local>,
    slows: FxHashSet<Local>,
}

/// A write to `p` assigns its local (one of `cands`) unless it goes through a pointer.
fn assigns(p: &Place, cands: &FxHashSet<Local>, set: &mut FxHashSet<Local>) {
    if !matches!(p.proj.first(), Some(Proj::Deref(_))) && cands.contains(&p.local) {
        set.insert(p.local);
    }
}

/// Liveness through `s` (backwards) of the locals in `cands`: an assignment to a whole local
/// defines it, any other mention uses it.
fn stmt_flow(s: &vir::Stmt, l: &mut FxHashSet<Local>, cands: &FxHashSet<Local>) {
    let mut s = s.clone();
    let mut skip = false;
    if let vir::Stmt::Assign(p, _) = &s {
        if p.proj.is_empty() {
            l.remove(&p.local);
            skip = true;
        }
    }
    // `stmt_locals` visits an assignment's destination first.
    stmt_locals(&mut s, &mut |x: &mut Local| {
        if !std::mem::take(&mut skip) && cands.contains(x) {
            l.insert(*x);
        }
    });
}

/// Liveness through `t` (backwards), as [`stmt_flow`].
fn term_flow(t: &Terminator, l: &mut FxHashSet<Local>, cands: &FxHashSet<Local>) {
    let mut t = t.clone();
    if let Terminator::Call { dest, .. } = &mut t {
        if let Some(p) = dest.take_if(|p| p.proj.is_empty()) {
            l.remove(&p.local);
        }
    }
    term_locals(&mut t, &mut |x: &mut Local| {
        if cands.contains(x) {
            l.insert(*x);
        }
    });
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
