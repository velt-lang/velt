//! Range checks that a dominating check already proves, decided at compile time.
//!
//! A check `at > len - 2` before a vector load, or `i < len` before an element access, is
//! redundant when an earlier branch established `base + K <= len` and `at` is `base` plus at most
//! `K - 2`: the pattern of a structure that checks a node's whole range once and then reads
//! words at constant offsets in it (a B-tree in one arena, bbtree's nodes read through raw
//! pointers in Rust). LLVM cannot prove it: Velt's integer arithmetic wraps, so `base + 6` is
//! not known to be at most `base + 128` without knowing that neither wraps.
//!
//! An abstract interpretation over the CFG of the unsigned integer locals:
//! - **Values:** a local is `b + k` for an offset `k` in `[lo, hi]` (equal modulo 2^64), where `b` is
//!   a local whose value is unknown, or is in `[lo, hi]` exactly (no base). A local that nothing
//!   is known about is itself plus 0.
//! - **Facts:** `x + K <= y`, mathematical (not modulo), for locals `x` (or 0) and `y`, from the
//!   edges of branches on comparisons, and copied along with a copy of either local.
//! - **Kills:** assigning a local forgets its value, every value based on it and every fact naming
//!   it, so facts stay true although VIR locals are assigned more than once (loops).
//! - **Joins** keep what all predecessors agree on, and widen the intervals of a join reached
//!   repeatedly; a loop's own test then bounds its counter again on the edge into the body.
//!
//! **The no-wrap argument.** A proof that `b + hi <= y - d` with `x + K <= y` facts uses only
//! non-negative offsets, so every partial sum along the way was at most `y`, below 2^64: none of
//! the wrapping additions that computed the value wrapped, and the compared values are the
//! mathematical ones. A comparison so decided becomes a constant, and `simplify_cfg` drops the
//! dead panic path.

use velt_vir::vir::{AggLayout, BinOp, Const, Function, Operand, Rvalue, Stmt, Terminator, Ty};

use crate::visit::successors;

mod domain;
mod transfer;

use domain::{meet, unsigned, widen, State, WIDEN_AFTER};
use transfer::{decide, edge_states, frozen_fields, transfer_block, Cx};

/// Decide the comparisons of `func`'s branches that earlier checks prove; returns whether any
/// was decided.
pub(crate) fn run(aggs: &[AggLayout], func: &mut Function) -> bool {
    let n = func.blocks.len();
    if n == 0 {
        return false;
    }
    if !has_range_check(func) {
        return false;
    }
    let threaded = thread_bool_copies(func);
    let decisions = analyse(aggs, func);
    let changed = threaded || !decisions.is_empty();
    for (bi, v) in decisions {
        if let Terminator::Branch { cond, .. } = &mut func.blocks[bi].term {
            *cond = Operand::Const(Const::Bool(v), Ty::Bool);
        }
    }
    changed
}

/// Whether `func` compares something with `y - k` for a constant `k`: the form of a range
/// check (`b > len - 8`, velt:simd's `at > length - 2`) from which the facts that decide other
/// checks come. Without one nothing can be decided, and the analysis is skipped.
fn has_range_check(func: &Function) -> bool {
    let mut subs: Vec<u32> = vec![];
    for b in &func.blocks {
        for s in &b.stmts {
            if let Stmt::Assign(pl, Rvalue::Binary(BinOp::Sub, _, Operand::Const(..))) = s {
                if pl.proj.is_empty() {
                    subs.push(pl.local.0);
                }
            }
        }
    }
    if subs.is_empty() {
        return false;
    }
    let is_sub = |op: &Operand| matches!(op, Operand::Copy(p) if p.proj.is_empty() && subs.contains(&p.local.0));
    func.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(s, Stmt::Assign(_, Rvalue::Binary(BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, a, b)) if is_sub(a) || is_sub(b))
    })
}

/// Thread branches on a bool that a predecessor just copied from another: `a || b` lowered as
/// `x = c1; branch c1 -> B / R; R: x = c2; goto B; B: branch x -> T / E` becomes `branch c1 ->
/// T / R; R: x = c2; branch c2 -> T / E`, so each comparison guards its own edges, the shape
/// the analysis reads facts from. `B` (no statements) is left for `simplify_cfg` to drop; `x`
/// keeps its assignments for any other use. Returns whether anything was threaded.
fn thread_bool_copies(func: &mut Function) -> bool {
    let mut changed = false;
    for bi in 0..func.blocks.len() {
        // The block `bi` jumps into, when it is an empty branch on a bool local.
        let targets = successors(&func.blocks[bi].term);
        for b in targets {
            let b = b.0 as usize;
            if b == bi || !func.blocks[b].stmts.is_empty() {
                continue;
            }
            let Terminator::Branch {
                cond: Operand::Copy(x),
                then,
                els,
            } = func.blocks[b].term.clone()
            else {
                continue;
            };
            if !x.proj.is_empty() || func.locals[x.local.0 as usize].ty != Ty::Bool {
                continue;
            }
            // The last assignment of `x` in `bi`, a copy of `y` that nothing after changes.
            let stmts = &func.blocks[bi].stmts;
            let Some(at) = stmts.iter().rposition(
                |s| matches!(s, Stmt::Assign(pl, _) if pl.local == x.local && pl.proj.is_empty()),
            ) else {
                continue;
            };
            let Stmt::Assign(_, Rvalue::Use(Operand::Copy(y))) = &stmts[at] else {
                continue;
            };
            if !y.proj.is_empty() || y.local == x.local {
                continue;
            }
            let y = y.local;
            let later_writes = stmts[at + 1..].iter().any(|s| {
                matches!(s, Stmt::Assign(pl, _) if pl.local == y || pl.local == x.local)
                    || matches!(s, Stmt::Assign(_, Rvalue::AddrOf(pl)) if pl.local == y || pl.local == x.local)
            });
            if later_writes {
                continue;
            }
            match func.blocks[bi].term.clone() {
                Terminator::Goto(t) if t.0 as usize == b => {
                    func.blocks[bi].term = Terminator::Branch {
                        cond: Operand::Copy(velt_vir::vir::Place::local(y)),
                        then,
                        els,
                    };
                    changed = true;
                }
                Terminator::Branch {
                    cond: Operand::Copy(c),
                    then: t1,
                    els: e1,
                } if c.proj.is_empty() && c.local == y => {
                    // On `bi`'s own edges `y` (so `x`) is known.
                    let t2 = if t1.0 as usize == b { then } else { t1 };
                    let e2 = if e1.0 as usize == b { els } else { e1 };
                    if t2 != t1 || e2 != e1 {
                        func.blocks[bi].term = Terminator::Branch {
                            cond: Operand::Copy(c),
                            then: t2,
                            els: e2,
                        };
                        changed = true;
                    }
                }
                _ => {}
            }
        }
    }
    changed
}

/// The branches whose comparison is decided, and to what.
fn analyse(aggs: &[AggLayout], func: &Function) -> Vec<(usize, bool)> {
    let n = func.blocks.len();
    let cx = Cx {
        func,
        frozen: frozen_fields(aggs, func),
    };
    let tracked = tracked_locals(func);
    let preds = predecessors(func);
    let mut ins: Vec<Option<State>> = vec![None; n];
    let mut arrivals = vec![0u32; n];
    ins[0] = Some(State::default());
    // Blocks in reverse postorder, the earliest pending one first: a block's predecessors
    // outside loops are done before it, so most blocks are visited once or twice.
    let rank = rpo_rank(func);
    let mut work = std::collections::BTreeSet::new();
    work.insert((rank[0], 0usize));
    let mut budget = n * 16;
    while let Some((_, bi)) = work.pop_first() {
        if budget == 0 {
            return vec![];
        }
        budget -= 1;
        let Some(state) = ins[bi].clone() else {
            continue;
        };
        let out = transfer_block(&cx, &tracked, bi, state);
        for (succ, edge) in edge_states(&cx, &tracked, bi, out) {
            let joined = match &ins[succ] {
                None => edge,
                Some(old) => {
                    let mut m = meet(old, &edge);
                    if preds[succ] > 1 {
                        arrivals[succ] += 1;
                        if arrivals[succ] > WIDEN_AFTER {
                            m = widen(old, &m, func);
                        }
                    }
                    m
                }
            };
            if ins[succ].as_ref() != Some(&joined) {
                ins[succ] = Some(joined);
                work.insert((rank[succ], succ));
            }
        }
    }
    let mut decisions = vec![];
    for (bi, state) in ins.into_iter().enumerate() {
        let Some(state) = state else {
            continue;
        };
        let out = transfer_block(&cx, &tracked, bi, state);
        let Terminator::Branch {
            cond: Operand::Copy(c),
            ..
        } = &func.blocks[bi].term
        else {
            continue;
        };
        if !c.proj.is_empty() {
            continue;
        }
        let Some(cond) = out.conds.iter().find(|k| k.c == c.local.0).cloned() else {
            continue;
        };
        if let Some(v) = decide(&out, &cond) {
            decisions.push((bi, v));
        }
    }
    decisions
}

/// Locals this pass reasons about: integers and bools never given away by address (signed ones
/// only as casts of unsigned values).
fn tracked_locals(func: &Function) -> Vec<bool> {
    let mut t: Vec<bool> = func
        .locals
        .iter()
        .map(|l| unsigned(l.ty) || l.ty.is_signed() || l.ty == Ty::Bool)
        .collect();
    for b in &func.blocks {
        for s in &b.stmts {
            if let Stmt::Assign(_, Rvalue::AddrOf(p)) = s {
                if let Some(x) = t.get_mut(p.local.0 as usize) {
                    *x = false;
                }
            }
        }
    }
    // Only the backward slice of the comparisons and branch conditions: the states stay small
    // in large functions, where most locals never reach a comparison.
    let mut rel = vec![false; t.len()];
    // Marks an operand's local; whether it was new.
    let mark = |rel: &mut Vec<bool>, op: &Operand| -> bool {
        if let Operand::Copy(p) = op {
            if p.proj.is_empty() {
                if let Some(x) = rel.get_mut(p.local.0 as usize) {
                    let new = !*x;
                    *x = true;
                    return new;
                }
            }
        }
        false
    };
    for b in &func.blocks {
        for s in &b.stmts {
            if let Stmt::Assign(
                pl,
                Rvalue::Binary(BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, x, y),
            ) = s
            {
                if pl.proj.is_empty() {
                    rel[pl.local.0 as usize] = true;
                }
                mark(&mut rel, x);
                mark(&mut rel, y);
            }
        }
        if let Terminator::Branch { cond, .. } = &b.term {
            mark(&mut rel, cond);
        }
    }
    loop {
        let mut changed = false;
        for b in &func.blocks {
            for s in &b.stmts {
                let Stmt::Assign(pl, rv) = s else { continue };
                if !pl.proj.is_empty() || !rel[pl.local.0 as usize] {
                    continue;
                }
                match rv {
                    Rvalue::Use(op) | Rvalue::Cast(op, _) | Rvalue::Unary(_, op) => {
                        changed |= mark(&mut rel, op);
                    }
                    Rvalue::Binary(_, x, y) => {
                        changed |= mark(&mut rel, x);
                        changed |= mark(&mut rel, y);
                    }
                    _ => {}
                }
            }
        }
        if !changed {
            break;
        }
    }
    for (x, r) in t.iter_mut().zip(&rel) {
        *x &= *r;
    }
    t
}

/// Each block's position in a reverse postorder of the CFG from the entry (unreachable
/// blocks last).
fn rpo_rank(func: &Function) -> Vec<usize> {
    let n = func.blocks.len();
    let mut post = Vec::with_capacity(n);
    let mut seen = vec![false; n];
    // Iterative DFS: (block, index of the next successor to visit).
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    seen[0] = true;
    while let Some(&mut (b, ref mut i)) = stack.last_mut() {
        let succ = successors(&func.blocks[b].term);
        if let Some(s) = succ.get(*i) {
            *i += 1;
            let s = s.0 as usize;
            if s < n && !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            post.push(b);
            stack.pop();
        }
    }
    let mut rank = vec![usize::MAX; n];
    for (r, b) in post.iter().rev().enumerate() {
        rank[*b] = r;
    }
    rank
}

fn predecessors(func: &Function) -> Vec<u32> {
    let mut p = vec![0u32; func.blocks.len()];
    for b in &func.blocks {
        for s in successors(&b.term) {
            if let Some(x) = p.get_mut(s.0 as usize) {
                *x += 1;
            }
        }
    }
    p
}

#[cfg(test)]
mod tests;
