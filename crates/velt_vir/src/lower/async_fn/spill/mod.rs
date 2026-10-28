//! State struct of a poll function, derived from the finished VIR function (dataflow in
//! flow.rs):
//!
//! 1. A local must survive suspension `k` if it is **live** at `k`'s resume or cancel block and
//!    **maybe written** before the block that suspended (set the tag to `k` and returned).
//!    Inputs (params/captures) are written by whoever built the state, so they survive when live
//!    at the start or at the cancel-before-start block.
//! 2. **Points-to**: a surviving pointer may point at a local that is not itself read after the
//!    suspension (`p = &x; await; *p`), so every local whose address may reach a spilled local
//!    is spilled too (pts.rs).
//! 3. **Layout**: `[result region @0] tag: u32, spilled locals…`; locals that are never needed
//!    at the same time share bytes (interference in overlap.rs, first-fit placement).
//! 4. **Rewrite**: every place rooted at a spilled local becomes `(*state).field…`, the
//!    placeholder state type is patched, and the remaining locals are renumbered.

mod flow;
mod overlap;
mod pts;
#[cfg(test)]
mod tests;

use rustc_hash::FxHashMap;

use flow::{Bits, Susp, Universe};
use overlap::Interference;
use pts::Set;

use super::{CLOSE_BIT, DONE, DROP_BIT, PLACEHOLDER};
use crate::lower::{ice, Cx};
use crate::vir::{
    AggId, AggLayout, Const, Function, Local, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty,
};

/// Locals 0 and 1 are the poll function's params (`state`, `cx`).
const PARAMS: usize = 2;

/// Spill `f`'s suspension-crossing locals into a new state aggregate. Returns the aggregate
/// and the state field of every spilled (original) local.
pub(super) fn spill(
    cx: &mut Cx,
    f: &mut Function,
    result: Ty,
    name: &str,
    inputs: &[Local],
) -> (AggId, FxHashMap<Local, u32>) {
    let (spilled, interf) = analyse(f, inputs);
    let (agg, fields) = layout(cx, f, &spilled, &interf, result, name);
    rewrite(f, agg, &fields);
    (agg, fields)
}

/// Steps 1 and 2 and the interference of step 3: the locals to spill, and which of them may
/// not share bytes.
fn analyse(f: &Function, inputs: &[Local]) -> (Bits, Interference) {
    let susp = suspension(f, inputs);
    let needed = suspended_locals(f, &susp);
    let pts = pts::points_to(f);
    let spilled = close_over_pointers(needed, &pts);
    let interf = overlap::interference(f, &pts, &spilled, &susp);
    (spilled, interf)
}

/// The suspension edges (suspending block → resume/cancel block) and start blocks of the
/// semantic CFG.
fn suspension(f: &Function, inputs: &[Local]) -> Susp {
    let Terminator::Switch { cases, .. } = &f.blocks[0].term else {
        ice("poll function without a dispatch switch")
    };
    let pend = suspending_blocks(f);
    let (mut extra, mut starts) = (vec![], vec![]);
    for &(tag, blk) in cases {
        let k = tag & !(DROP_BIT | CLOSE_BIT);
        let to = blk.0 as usize;
        if k == 0 {
            starts.push(to);
        } else if let Some(&from) = pend.get(&k) {
            extra.push((from, to));
        }
    }
    let inputs = inputs.iter().map(|l| l.0 as usize).collect();
    Susp {
        extra,
        starts,
        inputs,
    }
}

/// Locals whose value must survive some suspension (step 1 of the module docs). Liveness is
/// asked only at the resume/cancel and start blocks, and maybe-written only for the locals
/// live there.
fn suspended_locals(f: &Function, s: &Susp) -> Bits {
    let n = f.locals.len();
    let query: Vec<usize> = s
        .extra
        .iter()
        .map(|e| e.1)
        .chain(s.starts.iter().copied())
        .collect();
    let live = flow::live_in_at(f, &query);
    let mut cands = Bits::new(n);
    for &b in &query {
        live[b].iter().for_each(|&l| cands.set(l));
    }
    let u = Universe::of_set(n, &cands);
    let written = flow::maybe_written(f, s, &u);
    let mut ins = Bits::new(n);
    s.inputs.iter().for_each(|&l| ins.set(l));
    let mut out = Bits::new(n);
    for &(from, to) in &s.extra {
        for &l in &live[to] {
            if u.index(l).is_some_and(|i| written[from].get(i)) {
                out.set(l);
            }
        }
    }
    for &b in &s.starts {
        for &l in live[b].iter().filter(|&&l| ins.get(l)) {
            out.set(l);
        }
    }
    out
}

/// Block that suspends with tag `k` (its last statement stores `k` into the tag), per `k`.
fn suspending_blocks(f: &Function) -> FxHashMap<i128, usize> {
    let mut out = FxHashMap::default();
    for (b, blk) in f.blocks.iter().enumerate() {
        if !matches!(blk.term, Terminator::Return(_)) {
            continue;
        }
        if let Some(Stmt::Assign(p, Rvalue::Use(Operand::Const(Const::Int(k), _)))) =
            blk.stmts.last()
        {
            let is_tag = p.local == Local(0)
                && p.proj == [Proj::Deref(Ty::Agg(PLACEHOLDER)), Proj::Field(0)];
            if is_tag && *k != DONE && *k != super::generator::GEN_RUNNING {
                out.insert(*k, b);
            }
        }
    }
    out
}

/// The live set plus every local whose address may be held by a spilled local.
fn close_over_pointers(mut spilled: Bits, pts: &[Set]) -> Bits {
    let mut work: Vec<usize> = spilled.iter().collect();
    while let Some(x) = work.pop() {
        for y in pts[x].iter() {
            if y >= PARAMS && !spilled.get(y) {
                spilled.set(y);
                work.push(y);
            }
        }
    }
    spilled
}

/// `[result @0] tag: u32, spilled…`; returns the aggregate and local → field index. Spilled
/// locals are placed largest first at the lowest offset that does not overlap a local they
/// interfere with (overlap.rs), so locals of disjoint suspension points share bytes.
fn layout(
    cx: &mut Cx,
    f: &Function,
    spilled: &Bits,
    interf: &Interference,
    result: Ty,
    name: &str,
) -> (AggId, FxHashMap<Local, u32>) {
    let (rsize, ralign) = cx.size_align(result);
    let tag_off = rsize.next_multiple_of(4);
    let mut fields = vec![(Ty::U32, tag_off)];
    let (base, mut align) = (tag_off + 4, ralign.max(4));
    let mut locals: Vec<(u32, u32, usize)> = spilled
        .iter()
        .filter(|&l| l >= PARAMS && l < f.locals.len())
        .map(|l| {
            let (s, a) = cx.size_align(f.locals[l].ty);
            (a, s, l)
        })
        .collect();
    locals.sort_by(|x, y| y.1.cmp(&x.1).then(y.0.cmp(&x.0)).then(x.2.cmp(&y.2)));
    let mut map = FxHashMap::default();
    // (local, offset, size) of the locals placed so far.
    let mut placed: Vec<(usize, u32, u32)> = vec![];
    let mut end = base;
    for (a, s, l) in locals {
        let mut off = base.next_multiple_of(a);
        while let Some(&(_, o, os)) = placed
            .iter()
            .find(|&&(p, o, os)| interf.conflict(l, p) && off < o + os && o < off + s)
        {
            off = (o + os).next_multiple_of(a);
        }
        placed.push((l, off, s));
        map.insert(Local(l as u32), fields.len() as u32);
        fields.push((f.locals[l].ty, off));
        end = end.max(off + s);
        align = align.max(a);
    }
    let agg = cx.push_agg(AggLayout {
        name: format!("{name} state"),
        size: end.next_multiple_of(align),
        align,
        fields,
    });
    (agg, map)
}

/// Move spilled locals into the state, patch the placeholder type, renumber the rest.
fn rewrite(f: &mut Function, agg: AggId, fields: &FxHashMap<Local, u32>) {
    let mut remap = vec![Local(0); f.locals.len()];
    let mut locals = vec![];
    for (i, l) in f.locals.iter().enumerate() {
        if !fields.contains_key(&Local(i as u32)) {
            remap[i] = Local(locals.len() as u32);
            locals.push(l.clone());
        }
    }
    places_mut(f, &mut |p: &mut Place| {
        for x in p.proj.iter_mut() {
            if *x == Proj::Deref(Ty::Agg(PLACEHOLDER)) {
                *x = Proj::Deref(Ty::Agg(agg));
            }
        }
        match fields.get(&p.local) {
            Some(&fi) => {
                let mut proj = vec![Proj::Deref(Ty::Agg(agg)), Proj::Field(fi)];
                proj.append(&mut p.proj);
                *p = Place {
                    local: Local(0),
                    proj,
                };
            }
            None => p.local = remap[p.local.0 as usize],
        }
    });
    f.locals = locals;
}

fn operand_mut(o: &mut Operand, g: &mut impl FnMut(&mut Place)) {
    if let Operand::Copy(p) = o {
        g(p);
    }
}

/// Visit every place of a function mutably.
fn places_mut(f: &mut Function, g: &mut impl FnMut(&mut Place)) {
    for b in &mut f.blocks {
        for s in &mut b.stmts {
            match s {
                Stmt::Assign(p, rv) => {
                    g(p);
                    match rv {
                        Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Cast(o, _) => {
                            operand_mut(o, g)
                        }
                        Rvalue::Binary(_, a, b) => {
                            operand_mut(a, g);
                            operand_mut(b, g);
                        }
                        Rvalue::AddrOf(p) => g(p),
                        Rvalue::Aggregate(_, ops) => ops.iter_mut().for_each(|o| operand_mut(o, g)),
                    }
                }
                Stmt::MemCopy { dst, src, .. } => {
                    operand_mut(dst, g);
                    operand_mut(src, g);
                }
                Stmt::MemCopyDyn { dst, src, len, .. } => {
                    operand_mut(dst, g);
                    operand_mut(src, g);
                    operand_mut(len, g);
                }
                Stmt::MemSet { dst, byte, len } => {
                    operand_mut(dst, g);
                    operand_mut(byte, g);
                    operand_mut(len, g);
                }
                Stmt::Nop => {}
            }
        }
        match &mut b.term {
            Terminator::Branch { cond: o, .. }
            | Terminator::Switch { value: o, .. }
            | Terminator::Return(o) => operand_mut(o, g),
            Terminator::Call {
                callee, args, dest, ..
            } => {
                if let crate::vir::Callee::Ptr { target, .. } = callee {
                    operand_mut(target, g);
                }
                args.iter_mut().for_each(|a| operand_mut(a, g));
                if let Some(d) = dest {
                    g(d);
                }
            }
            Terminator::Goto(_) | Terminator::Unreachable => {}
        }
    }
}
