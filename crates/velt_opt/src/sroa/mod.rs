//! Scalar replacement of aggregates: an aggregate local whose address is never taken is
//! split into one local per field, so struct temporaries (`Vec3` math, copied-out elements,
//! closure values) live in registers instead of stack memory, and the value-tracking passes
//! see their fields. Cranelift has no such pass of its own; LLVM does, but runs it after
//! this crate's constant folding and inlining decisions.
//!
//! A candidate is only used as:
//! - `a = agg { … }` / `a = b` (whole assignment from an aggregate value or place);
//! - `x = a` (whole copy out, rewritten to `x = agg { a.0, a.1, … }`);
//! - places starting with a field `a.f…` (reads, writes, pointers loaded from fields).
//!
//! Fields that are aggregates themselves become aggregate locals, split again by the next
//! iteration. VIR's definite-assignment rule counts writing one field as initializing the
//! whole local, so a candidate with partial writes has its (scalar) field locals
//! zero-initialized on entry; otherwise every whole assignment defines all fields at once.

use std::collections::HashMap;

use velt_vir::vir::{
    AggId, AggLayout, Const, Function, Local, LocalDecl, Operand, Place, Proj, Rvalue, Stmt,
    Terminator, Ty,
};

use crate::locals::Usage;
use crate::srclocs::{prepend_stmts, rewrite_stmts};
use crate::visit::{stmt_operands, stmt_operands_mut, term_operands, term_operands_mut};

/// Nested aggregates are split at most this many levels deep.
const MAX_DEPTH: usize = 4;
/// Aggregates with more fields stay in memory (large records are mostly copied around).
pub(crate) const MAX_FIELDS: usize = 16;

/// Split aggregate locals in `func`; returns whether anything changed.
pub(crate) fn run(aggs: &[AggLayout], func: &mut Function) -> bool {
    let mut changed = false;
    for _ in 0..MAX_DEPTH {
        if !split_once(aggs, func) {
            break;
        }
        changed = true;
    }
    changed
}

/// What is known about one candidate while scanning.
struct Candidate {
    agg: AggId,
    /// Fields are written individually somewhere.
    partial: bool,
}

fn split_once(aggs: &[AggLayout], func: &mut Function) -> bool {
    let mut cands = candidates(aggs, func);
    disqualify(aggs, func, &mut cands);
    if cands.is_empty() {
        return false;
    }
    let mut fields: HashMap<Local, Vec<Local>> = HashMap::new();
    let mut entry_inits = Vec::new();
    let mut sorted: Vec<(&Local, &Candidate)> = cands.iter().collect();
    sorted.sort_by_key(|(l, _)| l.0);
    for (&a, cand) in sorted {
        let layout = &aggs[cand.agg.0 as usize];
        let base = func.locals[a.0 as usize].name.clone();
        let mut locals = Vec::with_capacity(layout.fields.len());
        for (f, &(ty, _)) in layout.fields.iter().enumerate() {
            let l = Local(func.locals.len() as u32);
            let name = base.as_ref().map(|n| format!("{n}.{f}"));
            func.locals.push(LocalDecl::new(ty, name));
            if cand.partial {
                entry_inits.push(Stmt::Assign(Place::local(l), Rvalue::Use(zero(ty))));
            }
            locals.push(l);
        }
        fields.insert(a, locals);
    }
    let split = Split { fields, cands };
    for bi in 0..func.blocks.len() {
        rewrite_stmts(func, bi, |s, out| split.stmt(s, out));
    }
    places_mut_terms(func, &split);
    prepend_stmts(func, 0, entry_inits);
    true
}

/// Aggregate locals (not params) whose address is never taken.
fn candidates(aggs: &[AggLayout], func: &Function) -> HashMap<Local, Candidate> {
    let usage = Usage::of(func);
    let mut out = HashMap::new();
    for (i, decl) in func.locals.iter().enumerate().skip(func.params.len()) {
        let Ty::Agg(id) = decl.ty else { continue };
        let Some(layout) = aggs.get(id.0 as usize) else {
            continue;
        };
        let l = Local(i as u32);
        let n = layout.fields.len();
        let splittable = (1..=MAX_FIELDS).contains(&n) && fields_cover(aggs, layout);
        if splittable && !usage.get(l).address_taken {
            out.insert(
                l,
                Candidate {
                    agg: id,
                    partial: false,
                },
            );
        }
    }
    out
}

/// Whether the fields hold every meaningful byte of the layout, i.e. the only gaps are
/// alignment padding. Enum aggregates fail this (their payload is only reachable through
/// variant views), and copying them field by field would lose the payload.
pub(crate) fn fields_cover(aggs: &[AggLayout], layout: &AggLayout) -> bool {
    let mut fields: Vec<(u32, u32, u32)> = Vec::with_capacity(layout.fields.len());
    for &(ty, offset) in &layout.fields {
        let (size, align) = match ty {
            Ty::Agg(id) => match aggs.get(id.0 as usize) {
                Some(inner) if fields_cover(aggs, inner) => (inner.size, inner.align),
                _ => return false,
            },
            Ty::Unit => return false,
            scalar => {
                let n = scalar.scalar_size().unwrap_or(0);
                (n, n)
            }
        };
        fields.push((offset, size, align.max(1)));
    }
    fields.sort_unstable();
    let mut end = 0u32;
    for (offset, size, align) in fields {
        // A gap is padding only if the field could not have started earlier.
        if offset < end || offset - end >= align {
            return false;
        }
        end = offset + size;
    }
    layout.size >= end && layout.size - end < layout.align.max(1)
}

/// Remove candidates used in ways the rewrite does not handle.
fn disqualify(aggs: &[AggLayout], func: &Function, cands: &mut HashMap<Local, Candidate>) {
    let mut bad: Vec<Local> = Vec::new();
    let mut whole_reads: HashMap<Local, u32> = HashMap::new();
    let mut allowed_reads: HashMap<Local, u32> = HashMap::new();
    let check_place = |p: &Place, bad: &mut Vec<Local>| {
        if cands.contains_key(&p.local) && !matches!(p.proj.first(), None | Some(Proj::Field(_))) {
            bad.push(p.local);
        }
    };
    for block in &func.blocks {
        for s in &block.stmts {
            stmt_operands(s, &mut |op| {
                if let Operand::Copy(p) = op {
                    check_place(p, &mut bad);
                    if p.proj.is_empty() {
                        *whole_reads.entry(p.local).or_default() += 1;
                    }
                }
            });
            let Stmt::Assign(dst, rv) = s else { continue };
            check_place(dst, &mut bad);
            if let Rvalue::Use(Operand::Copy(src)) = rv {
                if src.proj.is_empty() {
                    *allowed_reads.entry(src.local).or_default() += 1;
                }
            }
            if cands.contains_key(&dst.local) {
                classify_write(dst, rv, &mut bad);
            }
        }
        term_operands(&block.term, &mut |op| {
            if let Operand::Copy(p) = op {
                check_place(p, &mut bad);
                if p.proj.is_empty() {
                    *whole_reads.entry(p.local).or_default() += 1;
                }
            }
        });
        if let Terminator::Call { dest: Some(d), .. } = &block.term {
            check_place(d, &mut bad);
            if d.proj.is_empty() {
                bad.push(d.local);
            }
        }
    }
    for (l, n) in whole_reads {
        if allowed_reads.get(&l).copied().unwrap_or(0) < n {
            bad.push(l);
        }
    }
    for l in bad {
        cands.remove(&l);
    }
    mark_partial(aggs, func, cands);
}

/// A whole write must come from an aggregate value or a place that does not read the
/// candidate itself (its fields are overwritten one by one).
fn classify_write(dst: &Place, rv: &Rvalue, bad: &mut Vec<Local>) {
    if !dst.proj.is_empty() {
        return;
    }
    let reads_self = |op: &Operand| matches!(op, Operand::Copy(p) if p.local == dst.local);
    let ok = match rv {
        Rvalue::Aggregate(_, ops) => !ops.iter().any(reads_self),
        Rvalue::Use(Operand::Copy(src)) => src.local != dst.local || src.proj.is_empty(),
        _ => false,
    };
    if !ok {
        bad.push(dst.local);
    }
}

/// Candidates written field by field need their field locals initialized on entry; nested
/// aggregate fields cannot be (no aggregate constants), so those candidates are dropped.
fn mark_partial(aggs: &[AggLayout], func: &Function, cands: &mut HashMap<Local, Candidate>) {
    let mut partial = Vec::new();
    for block in &func.blocks {
        let dests = block.stmts.iter().filter_map(|s| match s {
            Stmt::Assign(dst, _) => Some(dst),
            _ => None,
        });
        let call_dest = match &block.term {
            Terminator::Call { dest: Some(d), .. } => Some(d),
            _ => None,
        };
        for d in dests.chain(call_dest) {
            if cands.contains_key(&d.local) && !d.proj.is_empty() {
                partial.push(d.local);
            }
        }
    }
    for l in partial {
        if let Some(c) = cands.get_mut(&l) {
            c.partial = true;
        }
    }
    let nested_partial: Vec<Local> = cands
        .iter()
        .filter(|(l, c)| c.partial && has_aggregate_field(aggs, func, **l))
        .map(|(l, _)| *l)
        .collect();
    for l in nested_partial {
        cands.remove(&l);
    }
}

fn has_aggregate_field(aggs: &[AggLayout], func: &Function, l: Local) -> bool {
    match func.locals[l.0 as usize].ty {
        Ty::Agg(id) => aggs[id.0 as usize]
            .fields
            .iter()
            .any(|(t, _)| matches!(t, Ty::Agg(_))),
        _ => false,
    }
}

/// The rewrite of one function's candidates.
struct Split {
    fields: HashMap<Local, Vec<Local>>,
    cands: HashMap<Local, Candidate>,
}

impl Split {
    /// `p` with a candidate base replaced by the field local its first projection names.
    fn place(&self, p: &Place) -> Place {
        match (self.fields.get(&p.local), p.proj.first()) {
            (Some(locals), Some(Proj::Field(f))) => Place {
                local: locals[*f as usize],
                proj: p.proj[1..].to_vec(),
            },
            _ => p.clone(),
        }
    }

    /// Field `f` of the value at `p`, as an operand.
    fn field_of(&self, p: &Place, f: u32) -> Operand {
        let mut q = p.clone();
        q.proj.push(Proj::Field(f));
        Operand::Copy(self.place(&q))
    }

    fn stmt(&self, s: Stmt, out: &mut Vec<Stmt>) {
        if let Stmt::Assign(dst, rv) = &s {
            if let Some(locals) = self.fields.get(&dst.local).filter(|_| dst.proj.is_empty()) {
                self.split_def(locals, rv, out);
                return;
            }
            if let Rvalue::Use(Operand::Copy(src)) = rv {
                if let (Some(locals), true) = (self.fields.get(&src.local), src.proj.is_empty()) {
                    let agg = self.cands[&src.local].agg;
                    let ops = locals.iter().map(|&l| Operand::Copy(Place::local(l)));
                    out.push(Stmt::Assign(
                        self.place(dst),
                        Rvalue::Aggregate(agg, ops.collect()),
                    ));
                    return;
                }
            }
        }
        let mut s = s;
        stmt_places_mut(&mut s, &mut |p| *p = self.place(p));
        out.push(s);
    }

    /// `a = rv` for a split candidate: one assignment per field.
    fn split_def(&self, locals: &[Local], rv: &Rvalue, out: &mut Vec<Stmt>) {
        match rv {
            Rvalue::Aggregate(_, ops) => {
                for (&l, op) in locals.iter().zip(ops) {
                    let op = match op {
                        Operand::Copy(p) => Operand::Copy(self.place(p)),
                        c => c.clone(),
                    };
                    out.push(Stmt::Assign(Place::local(l), Rvalue::Use(op)));
                }
            }
            Rvalue::Use(Operand::Copy(src)) => {
                if self.fields.get(&src.local).map(|ls| ls.as_slice()) == Some(locals) {
                    return; // `a = a`
                }
                for (f, &l) in locals.iter().enumerate() {
                    let op = self.field_of(src, f as u32);
                    out.push(Stmt::Assign(Place::local(l), Rvalue::Use(op)));
                }
            }
            _ => unreachable!("ICE: sroa candidate with an unsupported whole assignment"),
        }
    }
}

/// Rewrite the places of every terminator.
fn places_mut_terms(func: &mut Function, split: &Split) {
    for block in &mut func.blocks {
        let mut term = std::mem::replace(&mut block.term, Terminator::Unreachable);
        term_places_mut(&mut term, &mut |p| *p = split.place(p));
        block.term = term;
    }
}

/// Every place in a statement (operands, destination, `AddrOf` place).
fn stmt_places_mut(s: &mut Stmt, f: &mut impl FnMut(&mut Place)) {
    stmt_operands_mut(s, &mut |op| {
        if let Operand::Copy(p) = op {
            f(p);
        }
    });
    if let Stmt::Assign(dst, rv) = s {
        if let Rvalue::AddrOf(p) = rv {
            f(p);
        }
        f(dst);
    }
}

/// Every place in a terminator (operands and call destination).
fn term_places_mut(t: &mut Terminator, f: &mut impl FnMut(&mut Place)) {
    term_operands_mut(t, &mut |op| {
        if let Operand::Copy(p) = op {
            f(p);
        }
    });
    if let Terminator::Call { dest: Some(d), .. } = t {
        f(d);
    }
}

/// The zero value of a scalar type (initial value of a field local).
pub(crate) fn zero(ty: Ty) -> Operand {
    let c = match ty {
        Ty::F32 | Ty::F64 => Const::Float(0.0),
        Ty::Bool => Const::Bool(false),
        _ => Const::Int(0),
    };
    Operand::Const(c, ty)
}

#[cfg(test)]
mod tests;
