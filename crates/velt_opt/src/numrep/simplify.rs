//! What the facts decide outright, before narrowing:
//! - a comparison whose outcome is known becomes a constant, and a branch whose condition is
//!   known (or one of whose edges the refinement rules out) becomes a jump;
//! - `trunc`/`floor`/`ceil`/`round` of a whole value is the value (also for ±∞ and NaN);
//! - `abs` of a value that is never negative or `-0` is the value;
//! - `__floatIndex(x)` of a whole `x` in `[0, 2^64)`, never NaN, is `x as u64` (the path the
//!   helper takes for it);
//! - ToInt32 of a constant is that constant, and of a whole value in the int32 range a
//!   conversion.

use velt_vir::vir::{Callee, Const, Function, Operand, Rvalue, Stmt, Terminator, Ty};

use super::fact::{self, Fact};
use super::flow::{is_comparison, operand_ty, Flow, State};
use super::refine;
use super::Env;
use crate::srclocs::push_stmt;

/// Apply the simplifications to `func`; returns whether anything changed.
pub(super) fn run(env: &Env, flow: &Flow, func: &mut Function) -> bool {
    let mut changed = false;
    for bi in 0..func.blocks.len() {
        let Some(entry) = flow.entry(bi) else {
            continue;
        };
        let mut st: State = entry.clone();
        for si in 0..func.blocks[bi].stmts.len() {
            changed |= constant_params(flow, func, &st, bi, si);
            if let Some(v) = decided(flow, func, &st, &func.blocks[bi].stmts[si]) {
                if let Stmt::Assign(_, rv) = &mut func.blocks[bi].stmts[si] {
                    *rv = Rvalue::Use(Operand::Const(Const::Bool(v), Ty::Bool));
                    changed = true;
                }
            }
            flow.transfer(&mut st, func, &func.blocks[bi].stmts[si]);
        }
        changed |= terminator(env, flow, func, bi, &st);
    }
    changed
}

/// A parameter that every call sets to the same constant (`params`) is that constant where it
/// is read in statement `si` of block `bi` (unless the body changed it).
fn constant_params(flow: &Flow, func: &mut Function, st: &State, bi: usize, si: usize) -> bool {
    let n = func.params.len();
    let mut known = vec![];
    crate::visit::stmt_operands(&func.blocks[bi].stmts[si], &mut |op| {
        if let Operand::Copy(p) = op {
            let i = p.local.0 as usize;
            if p.proj.is_empty() && i < n {
                if let Some(c) = flow
                    .operand(st, func, op)
                    .and_then(|f| constant(f, func.locals[i].ty))
                {
                    known.push((p.local, c));
                }
            }
        }
    });
    if known.is_empty() {
        return false;
    }
    crate::visit::stmt_operands_mut(&mut func.blocks[bi].stmts[si], &mut |op| {
        if let Operand::Copy(p) = op {
            if let Some((_, c)) = known
                .iter()
                .find(|(l, _)| p.proj.is_empty() && *l == p.local)
            {
                *op = c.clone();
            }
        }
    });
    true
}

/// The constant a fact pins down: one value, not NaN, and not a `-0` that 0 would stand for.
fn constant(f: Fact, ty: Ty) -> Option<Operand> {
    if f.lo != f.hi || f.nan || f.neg_zero || f.lo.is_infinite() {
        return None;
    }
    match ty {
        Ty::F64 => Some(Operand::Const(Const::Float(f.lo), Ty::F64)),
        t if t.is_int() => Some(Operand::Const(Const::Int(f.lo as i128), t)),
        _ => None,
    }
}

/// The known outcome of the comparison `s` assigns, if any.
fn decided(flow: &Flow, func: &Function, st: &State, s: &Stmt) -> Option<bool> {
    let Stmt::Assign(_, Rvalue::Binary(op, a, b)) = s else {
        return None;
    };
    if !is_comparison(*op) || matches!((a, b), (Operand::Const(..), Operand::Const(..))) {
        return None;
    }
    let ty = operand_ty(func, a);
    if !(ty.is_int() || ty == Ty::F64) {
        return None;
    }
    fact::decide(*op, flow.operand(st, func, a)?, flow.operand(st, func, b)?)
}

/// Simplify the terminator of block `bi`, reached with `st`.
fn terminator(env: &Env, flow: &Flow, func: &mut Function, bi: usize, st: &State) -> bool {
    if let Terminator::Call { args, .. } = &func.blocks[bi].term {
        let printed = args.get(1).and_then(|a| flow.operand(st, func, a));
        if printed.is_some_and(|f| super::print::as_integer(env, func, bi, f)) {
            return true;
        }
    }
    let replacement = match &func.blocks[bi].term {
        Terminator::Branch { cond, then, els } => {
            branch_target(flow, func, bi, st, cond, *then, *els).map(|t| (None, t))
        }
        Terminator::Call {
            callee,
            args,
            dest,
            next,
        } => {
            let arg = args
                .first()
                .and_then(|a| Some((a, flow.operand(st, func, a)?)));
            arg.and_then(|(a, f)| folded_call(env, callee, a, f))
                .map(|rv| (dest.clone().map(|d| Stmt::Assign(d, rv)), *next))
        }
        _ => None,
    };
    let Some((stmt, target)) = replacement else {
        return false;
    };
    if let Some(s) = stmt {
        let at = func.term_loc(bi);
        push_stmt(func, bi, s, at);
    }
    func.blocks[bi].term = Terminator::Goto(target);
    true
}

/// Where a branch always goes, when the facts know.
fn branch_target(
    flow: &Flow,
    func: &Function,
    bi: usize,
    st: &State,
    cond: &Operand,
    then: velt_vir::vir::BlockId,
    els: velt_vir::vir::BlockId,
) -> Option<velt_vir::vir::BlockId> {
    let c = flow.operand(st, func, cond)?;
    if c.lo == c.hi {
        return Some(if c.lo != 0.0 { then } else { els });
    }
    let cmp = refine::condition(func, bi)?;
    let x = flow.operand(st, func, &cmp.lhs)?;
    let y = flow.operand(st, func, &cmp.rhs)?;
    let feasible = |taken: bool| refine::refine(cmp.op, taken != cmp.negated, x, y).is_some();
    match (feasible(true), feasible(false)) {
        (true, false) => Some(then),
        (false, true) => Some(els),
        _ => None,
    }
}

/// The value of a call the facts about its argument `a` make unnecessary.
fn folded_call(env: &Env, callee: &Callee, a: &Operand, f: Fact) -> Option<Rvalue> {
    match callee {
        Callee::Extern(id) if env.is_rounding(*id) && f.integral => Some(Rvalue::Use(a.clone())),
        Callee::Extern(id) if env.is_abs(*id) && f.lo >= 0.0 && !f.neg_zero && !f.nan => {
            Some(Rvalue::Use(a.clone()))
        }
        c if env.is_float_index(c)
            && f.integral
            && !f.nan
            && f.lo >= 0.0
            && f.hi < 18_446_744_073_709_551_616.0 =>
        {
            Some(Rvalue::Cast(a.clone(), Ty::U64))
        }
        // ToInt32 of a constant (`x | 0`'s `0`), or of a whole value within the int32 range,
        // which converts exactly.
        c if env.is_to_int32(c) && !f.nan && f.lo == f.hi => {
            let v = js_to_int32(f.lo);
            Some(Rvalue::Use(Operand::Const(Const::Int(v.into()), Ty::I32)))
        }
        c if env.is_to_int32(c)
            && f.integral
            && !f.nan
            && f.lo >= -2_147_483_648.0
            && f.hi <= 2_147_483_647.0 =>
        {
            Some(Rvalue::Cast(a.clone(), Ty::I32))
        }
        _ => None,
    }
}

/// JS's ToInt32 of `x`: truncated, modulo 2^32, in the signed 32-bit range (NaN and ±∞ are 0).
fn js_to_int32(x: f64) -> i32 {
    if !x.is_finite() {
        return 0;
    }
    let m = x.trunc().rem_euclid(4_294_967_296.0);
    m as u32 as i32
}

/// Integer comparisons of a local with itself (`x == x` once a NaN test like `__floatIndex`'s
/// `trunc(x) == x` compares integers) become constants, and branches on them jumps.
pub(crate) fn self_comparisons(func: &mut Function) -> bool {
    let mut changed = false;
    for bi in 0..func.blocks.len() {
        for si in 0..func.blocks[bi].stmts.len() {
            let Stmt::Assign(d, Rvalue::Binary(op, Operand::Copy(a), Operand::Copy(b))) =
                &func.blocks[bi].stmts[si]
            else {
                continue;
            };
            let int = a.proj.is_empty() && func.locals[a.local.0 as usize].ty.is_int();
            if !(int && a == b && is_comparison(*op)) {
                continue;
            }
            let v = matches!(
                op,
                velt_vir::vir::BinOp::Eq | velt_vir::vir::BinOp::Le | velt_vir::vir::BinOp::Ge
            );
            let d = d.clone();
            func.blocks[bi].stmts[si] =
                Stmt::Assign(d, Rvalue::Use(Operand::Const(Const::Bool(v), Ty::Bool)));
            changed = true;
        }
        changed |= constant_branch(func, bi);
    }
    changed
}

/// A branch on a local last set to a constant in its block becomes a jump.
fn constant_branch(func: &mut Function, bi: usize) -> bool {
    let block = &func.blocks[bi];
    let Terminator::Branch {
        cond: Operand::Copy(c),
        then,
        els,
    } = &block.term
    else {
        return false;
    };
    let last = block.stmts.iter().rev().find_map(|s| match s {
        Stmt::Assign(d, rv) if d.local == c.local => Some((d.proj.is_empty(), rv)),
        _ => None,
    });
    let Some((true, Rvalue::Use(Operand::Const(Const::Bool(v), _)))) = last else {
        return false;
    };
    let target = if *v { *then } else { *els };
    func.blocks[bi].term = Terminator::Goto(target);
    true
}
