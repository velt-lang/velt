//! Definite-assignment check (vir invariant 6): on every path from the entry, a local is
//! assigned before it is used. Taking the address of a local counts as initializing it, since
//! out-pointer calls write through it.
//!
//! A forward must-analysis over all locals costs blocks × locals per pass, which is quadratic
//! in a large generated `main` (thousands of blocks and locals). Instead each block is
//! summarized once — its *exposed* uses (before any assignment in the block) and the locals it
//! assigns — and only exposed uses need the CFG. A use in block `b` is safe when a block
//! assigning the local strictly dominates `b` (dominators.rs; this settles almost every use,
//! including far-away reads of locals assigned once, like drops at the end of `main`).
//! Otherwise it is unsafe exactly when a backward walk from `b` reaches the entry without
//! passing a block that assigns the local. One walk per local covers all its remaining use
//! blocks (it only fails if some path is unassigned; the culprits are then found one by one,
//! which only happens for broken VIR).

use super::dominators::Dominators;
use super::FnCheck;
use crate::lower::successors;
use crate::vir::*;

/// Per-block facts the CFG part needs.
struct Summary {
    /// Per block: the locals read before the block assigns them, in program order (a local
    /// read twice appears twice, so each read gets its diagnostic).
    exposed: Vec<Vec<Local>>,
    /// Per local: the blocks that assign it.
    assigned_in: Vec<Vec<u32>>,
}

impl FnCheck<'_> {
    pub(super) fn check_definite_init(&mut self) {
        let f = self.f;
        let summary = summarize(f);
        let cfg = Cfg::new(f);
        let mut uses_of: Vec<Vec<u32>> = vec![vec![]; f.locals.len()];
        for (b, locals) in summary.exposed.iter().enumerate() {
            if cfg.reachable[b] {
                for l in locals {
                    uses_of[l.0 as usize].push(b as u32);
                }
            }
        }
        let mut walk = Walk::new(f.blocks.len());
        let mut unsafe_uses: Vec<(u32, Local)> = vec![];
        for (l, blocks) in uses_of.iter().enumerate().skip(f.params.len()) {
            let assigned = &summary.assigned_in[l];
            let open: Vec<u32> = blocks
                .iter()
                .copied()
                .filter(|&b| !cfg.assigned_before(assigned, b))
                .collect();
            if open.is_empty() || !walk.reaches_entry(&cfg, assigned, &open) {
                continue;
            }
            for &b in &open {
                if walk.reaches_entry(&cfg, assigned, &[b]) {
                    unsafe_uses.push((b, Local(l as u32)));
                }
            }
        }
        self.report_unassigned(&summary, &unsafe_uses);
    }

    /// One diagnostic per exposed read that some path reaches unassigned, in block order.
    fn report_unassigned(&mut self, summary: &Summary, unsafe_uses: &[(u32, Local)]) {
        if unsafe_uses.is_empty() {
            return;
        }
        for (b, locals) in summary.exposed.iter().enumerate() {
            for &l in locals {
                if unsafe_uses.contains(&(b as u32, l)) {
                    let name = self.f.locals[l.0 as usize].name.as_deref().unwrap_or("");
                    let msg = format!("local _{} {name} may be used before it is assigned", l.0);
                    self.err(Some(b), msg);
                }
            }
        }
    }
}

/// Predecessors, reachability from the entry and dominators.
struct Cfg {
    preds: Vec<Vec<u32>>,
    reachable: Vec<bool>,
    doms: Dominators,
}

/// Up to this many assigning blocks, each is tested for dominance directly; beyond it the
/// read's dominator chain is climbed instead (locals assigned in many blocks, such as
/// accumulators, are usually assigned close to their reads).
const FEW_ASSIGNMENTS: usize = 8;

impl Cfg {
    fn new(f: &Function) -> Cfg {
        let n = f.blocks.len();
        let succs: Vec<Vec<u32>> = f
            .blocks
            .iter()
            .map(|b| successors(&b.term).iter().map(|s| s.0).collect())
            .collect();
        let mut preds = vec![vec![]; n];
        for (b, ss) in succs.iter().enumerate() {
            for &s in ss {
                preds[s as usize].push(b as u32);
            }
        }
        let doms = Dominators::new(&succs, &preds);
        let reachable = doms.idom.iter().map(Option::is_some).collect();
        Cfg {
            preds,
            reachable,
            doms,
        }
    }

    /// Whether a block assigning the local strictly dominates reachable block `b`: then every
    /// path to `b` assigns it first, with no CFG search.
    fn assigned_before(&self, assigned: &[u32], b: u32) -> bool {
        if b == 0 {
            return false;
        }
        if assigned.len() <= FEW_ASSIGNMENTS {
            return assigned
                .iter()
                .any(|&d| d != b && self.doms.dominates(d, b));
        }
        let mut x = b;
        while x != 0 {
            x = self.doms.idom[x as usize].expect("ICE: reachable block has a dominator");
            // `assigned` is sorted: blocks are summarized in order.
            if assigned.binary_search(&x).is_ok() {
                return true;
            }
        }
        false
    }
}

/// Backward walks with epoch-stamped marks, so starting a walk costs nothing.
struct Walk {
    epoch: u32,
    visited: Vec<u32>,
    assigns: Vec<u32>,
}

impl Walk {
    fn new(blocks: usize) -> Walk {
        Walk {
            epoch: 0,
            visited: vec![0; blocks],
            assigns: vec![0; blocks],
        }
    }

    /// Whether some path from the entry reaches the start of one of `uses` without passing a
    /// block in `assigned`. The entry itself starts with only the params assigned.
    fn reaches_entry(&mut self, cfg: &Cfg, assigned: &[u32], uses: &[u32]) -> bool {
        self.epoch += 1;
        let epoch = self.epoch;
        for &b in assigned {
            self.assigns[b as usize] = epoch;
        }
        let mut stack: Vec<u32> = vec![];
        for &b in uses {
            if b == 0 {
                return true;
            }
            stack.extend(&cfg.preds[b as usize]);
        }
        while let Some(p) = stack.pop() {
            let p = p as usize;
            if self.visited[p] == epoch || !cfg.reachable[p] || self.assigns[p] == epoch {
                continue;
            }
            if p == 0 {
                return true;
            }
            self.visited[p] = epoch;
            stack.extend(&cfg.preds[p]);
        }
        false
    }
}

/// Exposed reads and assignments of every block, following the order in which a statement
/// reads its operands and then writes its destination.
fn summarize(f: &Function) -> Summary {
    let mut summary = Summary {
        exposed: vec![vec![]; f.blocks.len()],
        assigned_in: vec![vec![]; f.locals.len()],
    };
    // `assigned_at[l] == b + 1`: block `b` has assigned `l` already.
    let mut assigned_at = vec![0u32; f.locals.len()];
    for (b, blk) in f.blocks.iter().enumerate() {
        let mut block = BlockScan {
            b: b as u32,
            assigned_at: &mut assigned_at,
            summary: &mut summary,
        };
        for s in &blk.stmts {
            block.stmt(s);
        }
        block.term(&blk.term);
    }
    summary
}

/// The summary of one block being built.
struct BlockScan<'a> {
    b: u32,
    assigned_at: &'a mut [u32],
    summary: &'a mut Summary,
}

impl BlockScan<'_> {
    fn use_local(&mut self, l: Local) {
        if self.assigned_at[l.0 as usize] != self.b + 1 {
            self.summary.exposed[self.b as usize].push(l);
        }
    }

    fn assign(&mut self, l: Local) {
        let slot = &mut self.assigned_at[l.0 as usize];
        if *slot != self.b + 1 {
            *slot = self.b + 1;
            self.summary.assigned_in[l.0 as usize].push(self.b);
        }
    }

    fn use_operand(&mut self, o: &Operand) {
        if let Operand::Copy(p) = o {
            self.use_local(p.local);
        }
    }

    /// Writing through a pointer reads the pointer local; any other write (whole local or a
    /// field of it) assigns the local.
    fn def_place(&mut self, p: &Place) {
        if starts_with_deref(p) {
            self.use_local(p.local);
        } else {
            self.assign(p.local);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Assign(pl, rv) => {
                self.use_rvalue(rv);
                self.def_place(pl);
                if let Rvalue::AddrOf(p) = rv {
                    if !starts_with_deref(p) {
                        self.assign(p.local);
                    }
                }
            }
            Stmt::MemCopy { dst, src, .. } => {
                self.use_operand(dst);
                self.use_operand(src);
            }
            Stmt::MemCopyDyn { dst, src, len, .. } => {
                [dst, src, len]
                    .into_iter()
                    .for_each(|o| self.use_operand(o));
            }
            Stmt::MemSet { dst, byte, len } => {
                [dst, byte, len]
                    .into_iter()
                    .for_each(|o| self.use_operand(o));
            }
            Stmt::Nop => {}
        }
    }

    fn term(&mut self, t: &Terminator) {
        match t {
            Terminator::Branch { cond: o, .. }
            | Terminator::Switch { value: o, .. }
            | Terminator::Return(o) => self.use_operand(o),
            Terminator::Call {
                callee, args, dest, ..
            } => {
                if let Callee::Ptr { target, .. } = callee {
                    self.use_operand(target);
                }
                args.iter().for_each(|a| self.use_operand(a));
                if let Some(d) = dest {
                    self.def_place(d);
                }
            }
            Terminator::Goto(_) | Terminator::Unreachable => {}
        }
    }

    fn use_rvalue(&mut self, rv: &Rvalue) {
        match rv {
            Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Cast(o, _) => self.use_operand(o),
            Rvalue::Binary(_, a, b) => {
                self.use_operand(a);
                self.use_operand(b);
            }
            Rvalue::AddrOf(p) if starts_with_deref(p) => self.use_local(p.local),
            Rvalue::AddrOf(_) => {}
            Rvalue::Aggregate(_, ops) => ops.iter().for_each(|o| self.use_operand(o)),
        }
    }
}

fn starts_with_deref(p: &Place) -> bool {
    matches!(p.proj.first(), Some(Proj::Deref(_)))
}
