//! Dropping the zero fill of new objects whose fields are all written before anything can read
//! them (issue #560).
//!
//! `new C(...)` lowers to `velt_rt_alloc`, a `memset` of the object to zero, the class header
//! and the constructor's field stores. When the object stays on the heap (it escapes, so
//! `heap_sroa` keeps it), the fill is usually dead: the constructor writes every field before
//! anything reads one. LLVM keeps it when the stores come after calls or in other blocks.
//!
//! **The scan.** From each `p = velt_rt_alloc(..)`, the pass follows the one path the program
//! takes next (gotos and the continuations of calls into blocks no other path enters, up to
//! the first branch, return, switch or join), tracking the locals that point into the new
//! block (`q = p`, `q = p + c`) with their offsets. Every statement it visits runs only after
//! the ones before it on the walk. The block is fresh: nothing else can reach it until one of
//! those pointers escapes, so the pass only has to look at their mentions:
//! - `memset q, 0, n` (the first one) is the fill;
//! - a store through a tracked pointer writes its bytes;
//! - a read through one must only read bytes written since the fill (or outside it);
//! - any other mention (a call argument, a stored or compared pointer, `&q.f`) ends the scan.
//!
//! Calls that do not receive a tracked pointer cannot read the block, so the scan goes on past
//! them. The fill is removed once every byte of it that holds data has been written
//! after it: every byte but the padding of the object aggregate, when every store into the
//! filled range goes through that aggregate (aggregate copies need not preserve padding,
//! vir.rs), every byte otherwise. Bytes written before the fill do not count; the fill
//! overwrote them.

mod access;

use velt_vir::vir::{
    AggLayout, BinOp, Callee, Const, Function, Local, Operand, Place, Rvalue, Stmt, Terminator,
};

use crate::heap_sroa::Allocator;
use crate::srclocs::retain_stmts;
use crate::visit::{rvalue_operands, stmt_operands, successors, term_operands};
use access::{Access, Fill, Touch};

/// Fills longer than this (bytes) are kept: objects are small, and the bitmap is per byte.
const MAX_FILL: i128 = 1024;

/// Statements (and terminators) one scan looks at before giving up.
const BUDGET: usize = 512;

/// Drop the dead zero fills of `func`'s allocations; returns whether any was dropped.
pub(crate) fn run(aggs: &[AggLayout], allocator: Option<Allocator>, func: &mut Function) -> bool {
    let Some(allocator) = allocator else {
        return false;
    };
    let single = single_predecessors(func);
    let mut dead = Vec::new();
    for block in &func.blocks {
        let Terminator::Call {
            callee: Callee::Extern(e),
            dest: Some(d),
            next,
            ..
        } = &block.term
        else {
            continue;
        };
        if *e == allocator.alloc && d.proj.is_empty() {
            let mut scan = Scan::new(aggs, d.local);
            if let Some(at) = scan.from(func, &single, next.0 as usize) {
                dead.push(at);
            }
        }
    }
    // From the back, so earlier positions in a block stay valid; two allocations reaching one
    // fill (the same local, assigned on two paths) both found it dead.
    dead.sort_unstable_by(|a, b| b.cmp(a));
    dead.dedup();
    for &(b, s) in &dead {
        let mut i = 0;
        retain_stmts(func, b, |_| {
            i += 1;
            i - 1 != s
        });
    }
    !dead.is_empty()
}

/// Per block: whether exactly one edge enters it (the entry block is also entered by the call).
fn single_predecessors(func: &Function) -> Vec<bool> {
    let mut preds = vec![0u32; func.blocks.len()];
    if let Some(entry) = preds.first_mut() {
        *entry = 1;
    }
    for block in &func.blocks {
        for s in successors(&block.term) {
            preds[s.0 as usize] += 1;
        }
    }
    preds.into_iter().map(|n| n == 1).collect()
}

/// What the scan does after one statement.
enum Flow {
    Go,
    /// Something may read the block: the fill stays, unless it is already covered.
    Stop,
}

/// The state of one scan from an allocation.
struct Scan<'a> {
    aggs: &'a [AggLayout],
    /// Locals pointing into the block, with their offsets.
    ptrs: Vec<(Local, i128)>,
    fill: Option<Fill>,
    /// A store wrote bytes of the fill since coverage was last checked.
    dirty: bool,
    budget: usize,
}

fn constant(op: &Operand) -> Option<i128> {
    match op {
        Operand::Const(Const::Int(v), _) => Some(*v),
        _ => None,
    }
}

impl<'a> Scan<'a> {
    fn new(aggs: &'a [AggLayout], ptr: Local) -> Scan<'a> {
        Scan {
            aggs,
            ptrs: vec![(ptr, 0)],
            fill: None,
            dirty: false,
            budget: BUDGET,
        }
    }

    /// Scan from the start of block `b`; the fill to drop (block, statement), if any.
    /// Only blocks with one predecessor are entered: a block that another path also reaches
    /// would run the fill (or reads of it) for objects the walk knows nothing about.
    fn from(&mut self, func: &Function, single: &[bool], mut b: usize) -> Option<(usize, usize)> {
        let mut visited = vec![false; func.blocks.len()];
        loop {
            if !single[b] || std::mem::replace(&mut visited[b], true) {
                return None;
            }
            let block = &func.blocks[b];
            for (i, s) in block.stmts.iter().enumerate() {
                self.budget = self.budget.checked_sub(1)?;
                if let Flow::Stop = self.stmt(s, (b, i)) {
                    return None;
                }
                if let Some(at) = self.covered() {
                    return Some(at);
                }
            }
            self.budget = self.budget.checked_sub(1)?;
            b = self.term(&block.term)?;
            if let Some(at) = self.covered() {
                return Some(at);
            }
        }
    }

    /// The fill's position, once every data byte of it is written.
    fn covered(&mut self) -> Option<(usize, usize)> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        let fill = self.fill.as_ref()?;
        fill.covered(self.aggs).then_some(fill.at)
    }

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

    /// Whether reading `op` is fine: it does not mention a tracked pointer, or reads only
    /// written bytes through one.
    fn read(&self, op: &Operand) -> bool {
        let Operand::Copy(p) = op else { return true };
        match self.touch(p) {
            Touch::Elsewhere => true,
            Touch::Escape => false,
            Touch::At(a) => self.readable(&a),
        }
    }

    fn readable(&self, a: &Access) -> bool {
        self.fill
            .as_ref()
            .is_none_or(|fill| fill.readable(self.aggs, a))
    }

    fn reads_rvalue(&self, rv: &Rvalue) -> bool {
        if let Rvalue::AddrOf(p) = rv {
            return matches!(self.touch(p), Touch::Elsewhere);
        }
        let mut ok = true;
        rvalue_operands(rv, &mut |op| ok &= self.read(op));
        ok
    }

    /// A write of place `p`.
    fn store(&mut self, p: &Place) -> Flow {
        if p.proj.is_empty() {
            self.set(p.local, None);
            return Flow::Go;
        }
        match self.touch(p) {
            Touch::Elsewhere => Flow::Go,
            Touch::Escape => Flow::Stop,
            Touch::At(a) if a.through => match self.readable(&a) {
                true => Flow::Go,
                false => Flow::Stop,
            },
            Touch::At(a) => {
                if let Some(fill) = &mut self.fill {
                    self.dirty |= fill.write(self.aggs, &a);
                }
                Flow::Go
            }
        }
    }

    fn stmt(&mut self, s: &Stmt, at: (usize, usize)) -> Flow {
        match s {
            Stmt::Assign(dst, rv) => {
                if dst.proj.is_empty() {
                    if let Some(off) = self.derived(rv) {
                        self.set(dst.local, Some(off));
                        return Flow::Go;
                    }
                }
                if !self.reads_rvalue(rv) {
                    return Flow::Stop;
                }
                self.store(dst)
            }
            Stmt::MemSet {
                dst: Operand::Copy(p),
                byte,
                len,
            } if p.proj.is_empty() && self.offset(p.local).is_some() => {
                match (self.fill.is_none(), constant(byte), constant(len)) {
                    (true, Some(0), Some(n)) if 0 < n && n <= MAX_FILL => {
                        let start = self.offset(p.local).unwrap_or_default();
                        self.fill = Some(Fill::new(at, start, n as u32));
                        Flow::Go
                    }
                    _ => Flow::Stop,
                }
            }
            _ => {
                let mut ok = true;
                stmt_operands(s, &mut |op| ok &= self.read(op));
                match ok {
                    true => Flow::Go,
                    false => Flow::Stop,
                }
            }
        }
    }

    /// The block the path continues in after `t`, if it continues.
    fn term(&mut self, t: &Terminator) -> Option<usize> {
        match t {
            Terminator::Goto(b) => Some(b.0 as usize),
            Terminator::Call { dest, next, .. } => {
                let mut ok = true;
                term_operands(t, &mut |op| ok &= self.read(op));
                if !ok {
                    return None;
                }
                if let Some(d) = dest {
                    if let Flow::Stop = self.store(d) {
                        return None;
                    }
                }
                Some(next.0 as usize)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
