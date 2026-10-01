//! CFG construction primitives. Emission into an unreachable position is silently skipped, so
//! code after `return`/`break`/noreturn calls never materializes; unreachable blocks are pruned
//! and the rest renumbered when the function is finished.

use super::rt::Rt;
use super::{ice, FnLower};
use crate::vir::{
    self, BasicBlock, BlockId, Const, Function, Linkage, Local, LocalDecl, Operand, Place, Rvalue,
};
use crate::vir::{Terminator, Ty, STR_AGG};

/// Successor blocks of a terminator.
pub(crate) fn successors(t: &Terminator) -> Vec<BlockId> {
    match t {
        Terminator::Goto(b) => vec![*b],
        Terminator::Branch { then, els, .. } => vec![*then, *els],
        Terminator::Switch { cases, default, .. } => cases
            .iter()
            .map(|c| c.1)
            .chain(std::iter::once(*default))
            .collect(),
        Terminator::Call { next, .. } => vec![*next],
        Terminator::Return(_) | Terminator::Unreachable => vec![],
    }
}

fn remap_term(t: Terminator, map: impl Fn(BlockId) -> BlockId) -> Terminator {
    match t {
        Terminator::Goto(b) => Terminator::Goto(map(b)),
        Terminator::Branch { cond, then, els } => Terminator::Branch {
            cond,
            then: map(then),
            els: map(els),
        },
        Terminator::Switch {
            value,
            cases,
            default,
        } => Terminator::Switch {
            value,
            cases: cases.into_iter().map(|(v, b)| (v, map(b))).collect(),
            default: map(default),
        },
        Terminator::Call {
            callee,
            args,
            dest,
            next,
        } => Terminator::Call {
            callee,
            args,
            dest,
            next: map(next),
        },
        t @ (Terminator::Return(_) | Terminator::Unreachable) => t,
    }
}

impl FnLower<'_, '_> {
    pub(super) fn new_local(&mut self, ty: Ty, name: Option<String>) -> Local {
        if ty == Ty::Unit {
            ice("attempted to create a Unit local");
        }
        self.locals.push(LocalDecl { ty, name });
        Local(self.locals.len() as u32 - 1)
    }

    pub(super) fn temp(&mut self, ty: Ty) -> Local {
        self.new_local(ty, None)
    }

    pub(super) fn new_block(&mut self) -> BlockId {
        self.blocks.push((vec![], None));
        self.block_locs.push((vec![], None));
        self.live.push(false);
        BlockId(self.blocks.len() as u32 - 1)
    }

    /// The current position is unreachable (no live predecessor, or already terminated).
    pub(super) fn dead(&self) -> bool {
        let c = self.cur.0 as usize;
        !self.live[c] || self.blocks[c].1.is_some()
    }

    pub(super) fn switch_to(&mut self, b: BlockId) {
        self.cur = b;
    }

    pub(super) fn assign(&mut self, p: Place, rv: Rvalue) {
        self.push_stmt(vir::Stmt::Assign(p, rv));
    }

    /// Append a statement (with the current source location) unless the position is dead.
    pub(super) fn push_stmt(&mut self, s: vir::Stmt) {
        if !self.dead() {
            let b = self.cur.0 as usize;
            self.blocks[b].0.push(s);
            self.block_locs[b].0.push(self.loc);
        }
    }

    pub(super) fn terminate(&mut self, t: Terminator) {
        if self.dead() {
            return;
        }
        for s in successors(&t) {
            self.live[s.0 as usize] = true;
        }
        self.blocks[self.cur.0 as usize].1 = Some(t);
        self.block_locs[self.cur.0 as usize].1 = self.loc;
    }

    pub(super) fn goto(&mut self, b: BlockId) {
        self.terminate(Terminator::Goto(b));
    }

    /// Conditional jump; constant conditions become a plain `goto`.
    pub(super) fn branch(&mut self, cond: Operand, then: BlockId, els: BlockId) {
        match cond {
            Operand::Const(Const::Bool(b), _) => self.goto(if b { then } else { els }),
            cond => self.terminate(Terminator::Branch { cond, then, els }),
        }
    }

    /// Emit a call terminator and continue in a fresh block (`Unreachable` for noreturn callees).
    pub(super) fn call(
        &mut self,
        callee: vir::Callee,
        args: Vec<Operand>,
        dest: Option<Place>,
        noreturn: bool,
    ) {
        if self.dead() {
            return;
        }
        let next = self.new_block();
        self.terminate(Terminator::Call {
            callee,
            args,
            dest,
            next,
        });
        self.switch_to(next);
        if noreturn {
            self.terminate(Terminator::Unreachable);
        }
    }

    pub(super) fn call_rt(&mut self, r: Rt, args: Vec<Operand>, dest: Option<Place>) {
        let id = self.cx.rt(r);
        self.call(vir::Callee::Extern(id), args, dest, r.sig().3);
    }

    /// Materialize deferred blocks, prune unreachable ones and build the VIR function.
    pub(super) fn finish(mut self, symbol: String, params: Vec<Ty>, ret: Ty) -> Function {
        self.build_div_zero_blocks();
        let mut remap = vec![None; self.blocks.len()];
        for (n, (i, _)) in self
            .live
            .iter()
            .enumerate()
            .filter(|(_, l)| **l)
            .enumerate()
        {
            remap[i] = Some(BlockId(n as u32));
        }
        let map = |b: BlockId| remap[b.0 as usize].unwrap_or_else(|| ice("jump to a dead block"));
        let with_locs = self.cx.locs.is_some();
        let mut blocks = vec![];
        let mut locs = vec![];
        let pending = self.blocks.into_iter().zip(self.block_locs);
        for (i, ((stmts, term), (mut stmt_locs, term_loc))) in pending.enumerate() {
            if self.live[i] {
                let term = term.unwrap_or_else(|| {
                    ice(format_args!("block bb{i} of {symbol} has no terminator"))
                });
                blocks.push(BasicBlock {
                    stmts,
                    term: remap_term(term, map),
                });
                if with_locs {
                    stmt_locs.push(term_loc);
                    locs.push(stmt_locs);
                }
            }
        }
        Function {
            symbol,
            params,
            ret,
            locals: self.locals,
            blocks,
            linkage: Linkage::Internal,
            locs,
            param_attrs: vec![],
            is_poll: false,
        }
    }

    /// The live `panic("division by zero at …")` blocks, one per panic location.
    fn build_div_zero_blocks(&mut self) {
        for (at, bb) in std::mem::take(&mut self.div_zero_bbs) {
            if !self.live[bb.0 as usize] {
                continue;
            }
            self.switch_to(bb);
            self.loc = at;
            let msg = format!("division by zero{}", self.panic_suffix());
            let msg = self.str_lit(&msg);
            let a = self.operand_addr(msg, Ty::Agg(STR_AGG));
            self.call_rt(Rt::Panic, vec![a], None);
        }
    }
}
