//! Walking the code right after an allocation, while the new block is still private to the
//! function: what `dead_fills` (#560) and `vtable_loads` (#559) learn about new objects.
//!
//! From `p = velt_rt_alloc(..)`, the walk follows the one path the program takes next (gotos,
//! the continuations of calls and branches on a null test of the new pointer, which is never
//! null; up to any other branch, a return or a switch), tracking the locals that point into the
//! block (`q = p`, `q = p + c`) with their offsets. Nothing else can reach the block until one
//! of those pointers escapes, so their mentions are all there is to see; an [`Observer`] is
//! told about each:
//! - `memset q, b, n` with constant `b` and `n`;
//! - a store through a tracked pointer (with the stored operand, for a plain one);
//! - a read through one (a store through a pointer *stored* in the block reads it too).
//!
//! Any other mention (a call argument, a stored or compared pointer, `&q.f`) ends the walk, as
//! does the observer. Calls that do not receive a tracked pointer cannot see the block, so the
//! walk goes on past them.

pub(crate) mod access;

use velt_vir::vir::{
    AggLayout, BinOp, Const, Function, Local, Operand, Place, Rvalue, Stmt, Terminator, Ty,
};

use crate::visit::{rvalue_operands, stmt_operands, term_operands};
use access::{Access, Touch};

/// Statements (and terminators) one walk looks at before giving up.
const BUDGET: usize = 512;

/// A statement (block, index); the index of a terminator is its block's statement count.
pub(crate) type Pos = (usize, usize);

/// What a walk reports; each method returns whether the walk goes on.
pub(crate) trait Observer {
    /// `memset` of `len` bytes of `byte` at offset `start`.
    fn fill(&mut self, at: Pos, start: i128, byte: i128, len: i128) -> bool;
    /// A store of `a` (`value`: the stored operand of a plain `place = operand`).
    fn store(&mut self, a: &Access, value: Option<&Operand>) -> bool;
    /// A read of `a`, through place `p` of the statement at `at`.
    fn read(&mut self, at: Pos, p: &Place, a: &Access) -> bool;
}

/// Walk from the start of block `from`, where `ptr` points at the new block.
pub(crate) fn walk(
    aggs: &[AggLayout],
    func: &Function,
    ptr: Local,
    from: usize,
    obs: &mut impl Observer,
) {
    let mut walk = Walk {
        aggs,
        ptrs: vec![(ptr, 0)],
        tests: Vec::new(),
        obs,
    };
    let mut visited = vec![false; func.blocks.len()];
    let mut budget = BUDGET;
    let mut b = from;
    loop {
        if std::mem::replace(&mut visited[b], true) {
            return;
        }
        let block = &func.blocks[b];
        for (i, s) in block.stmts.iter().enumerate() {
            budget -= 1;
            if budget == 0 || !walk.stmt(s, (b, i)) {
                return;
            }
        }
        budget -= 1;
        match walk.term(&block.term, (b, block.stmts.len())) {
            Some(next) if budget > 0 => b = next,
            _ => return,
        }
    }
}

/// The state of one walk.
struct Walk<'a, O> {
    aggs: &'a [AggLayout],
    /// Locals pointing into the block, with their offsets.
    ptrs: Vec<(Local, i128)>,
    /// Bool locals holding a null test of a tracked pointer, with their value: the allocator
    /// never returns null (it aborts when out of memory), so a drop guard is decided.
    tests: Vec<(Local, bool)>,
    obs: &'a mut O,
}

fn constant(op: &Operand) -> Option<i128> {
    match op {
        Operand::Const(Const::Int(v), _) => Some(*v),
        _ => None,
    }
}

impl<O: Observer> Walk<'_, O> {
    fn offset(&self, l: Local) -> Option<i128> {
        self.ptrs
            .iter()
            .find(|&&(p, _)| p == l)
            .map(|&(_, off)| off)
    }

    /// `l` now points at `off` into the block (`Some`), or elsewhere.
    fn set(&mut self, l: Local, off: Option<i128>) {
        self.ptrs.retain(|&(p, _)| p != l);
        if let Some(off) = off {
            self.ptrs.push((l, off));
        }
    }

    fn touch(&self, p: &Place) -> Touch {
        match self.offset(p.local) {
            Some(base) => access::of(self.aggs, base, p),
            None => Touch::Elsewhere,
        }
    }

    /// A pointer into the block computed by `rv`: a copy or a constant offset of one.
    fn derived(&self, rv: &Rvalue) -> Option<i128> {
        let whole = |op: &Operand| match op {
            Operand::Copy(p) if p.proj.is_empty() => self.offset(p.local),
            _ => None,
        };
        match rv {
            Rvalue::Use(op) => whole(op),
            Rvalue::Binary(BinOp::PtrAdd, a, b) => Some(whole(a)? + constant(b)?),
            _ => None,
        }
    }

    /// `p != null` (true) or `p == null` (false) of a tracked pointer `p`.
    fn null_test(&self, rv: &Rvalue) -> Option<bool> {
        let Rvalue::Binary(op @ (BinOp::Eq | BinOp::Ne), a, b) = rv else {
            return None;
        };
        let tracked = |x: &Operand| matches!(x, Operand::Copy(p) if p.proj.is_empty() && self.offset(p.local).is_some());
        let null = |x: &Operand| matches!(x, Operand::Const(Const::Int(0), Ty::Ptr));
        ((tracked(a) && null(b)) || (null(a) && tracked(b))).then_some(*op == BinOp::Ne)
    }

    /// Report a read of `op`; false when it lets the pointer escape or the observer stops.
    fn read(&mut self, at: Pos, op: &Operand) -> bool {
        let Operand::Copy(p) = op else { return true };
        match self.touch(p) {
            Touch::Elsewhere => true,
            Touch::Escape => false,
            Touch::At(a) => self.obs.read(at, p, &a),
        }
    }

    fn rvalue(&mut self, at: Pos, rv: &Rvalue) -> bool {
        if let Rvalue::AddrOf(p) = rv {
            return matches!(self.touch(p), Touch::Elsewhere);
        }
        let mut ok = true;
        rvalue_operands(rv, &mut |op| ok = ok && self.read(at, op));
        ok
    }

    /// A write of place `p` (with the stored operand of a plain use).
    fn store(&mut self, at: Pos, p: &Place, value: Option<&Operand>) -> bool {
        if p.proj.is_empty() {
            self.set(p.local, None);
            return true;
        }
        match self.touch(p) {
            Touch::Elsewhere => true,
            Touch::Escape => false,
            Touch::At(a) if a.through => self.obs.read(at, p, &a),
            Touch::At(a) => self.obs.store(&a, value),
        }
    }

    fn stmt(&mut self, s: &Stmt, at: Pos) -> bool {
        match s {
            Stmt::Assign(dst, rv) => {
                if dst.proj.is_empty() {
                    self.tests.retain(|&(l, _)| l != dst.local);
                    if let Some(off) = self.derived(rv) {
                        self.set(dst.local, Some(off));
                        return true;
                    }
                    if let Some(nonnull) = self.null_test(rv) {
                        self.set(dst.local, None);
                        self.tests.push((dst.local, nonnull));
                        return true;
                    }
                }
                let value = match rv {
                    Rvalue::Use(op) => Some(op),
                    _ => None,
                };
                self.rvalue(at, rv) && self.store(at, dst, value)
            }
            Stmt::MemSet {
                dst: Operand::Copy(p),
                byte,
                len,
            } if p.proj.is_empty() && self.offset(p.local).is_some() => {
                let start = self.offset(p.local).unwrap_or_default();
                match (constant(byte), constant(len)) {
                    (Some(byte), Some(len)) => self.obs.fill(at, start, byte, len),
                    _ => false,
                }
            }
            _ => {
                let mut ok = true;
                stmt_operands(s, &mut |op| ok = ok && self.read(at, op));
                ok
            }
        }
    }

    /// The block the path continues in after `t`, if it continues.
    fn term(&mut self, t: &Terminator, at: Pos) -> Option<usize> {
        match t {
            Terminator::Goto(b) => Some(b.0 as usize),
            Terminator::Branch {
                cond: Operand::Copy(c),
                then,
                els,
            } if c.proj.is_empty() => {
                let &(_, nonnull) = self.tests.iter().find(|&&(l, _)| l == c.local)?;
                Some(if nonnull { then.0 } else { els.0 } as usize)
            }
            Terminator::Call { dest, next, .. } => {
                let mut ok = true;
                term_operands(t, &mut |op| ok = ok && self.read(at, op));
                if !ok {
                    return None;
                }
                if let Some(d) = dest {
                    if !self.store(at, d, None) {
                        return None;
                    }
                }
                Some(next.0 as usize)
            }
            _ => None,
        }
    }
}
