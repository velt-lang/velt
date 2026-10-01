//! Constant folding and propagation: known values of register-like locals replace their
//! reads, operations on constants are evaluated, integer identities (`x + 0`, `x * 1`,
//! `x & 0`, …) are simplified, branches/switches on constants become jumps, and indirect
//! calls through a constant function address become direct calls (so they can be inlined).

mod dataflow;
mod liveness;
mod value;

use velt_vir::vir::{BinOp, Callee, Const, Function, Operand, Rvalue, Stmt, Terminator, Ty, UnOp};

use crate::callgraph::Signature;
use crate::visit::{rvalue_operands_mut, stmt_operands_mut, term_operands_mut};
use dataflow::{Facts, Lat, State};
pub(crate) use value::normalize;
use value::Value;

/// Fold constants in `func`; `signatures` are all functions' signatures (for
/// devirtualization). Returns whether anything changed.
pub(crate) fn run(signatures: &[Signature], func: &mut Function) -> bool {
    if func.blocks.is_empty() {
        return false;
    }
    let facts = Facts::of(func);
    let solved = dataflow::solve(func, &facts);
    let mut changed = false;
    let mut st = facts.unknown();
    for (bi, block) in func.blocks.iter_mut().enumerate() {
        if let Some((liveness, entries)) = &solved {
            let Some(entry) = &entries[bi] else { continue };
            dataflow::load(&mut st, &liveness.live_in[bi], entry);
        }
        for s in &mut block.stmts {
            changed |= rewrite_stmt(&facts, &st, s);
            facts.transfer(&mut st, s);
        }
        changed |= rewrite_term(&facts, &st, signatures, &mut block.term);
        match &solved {
            Some((liveness, _)) => dataflow::reset(&mut st, liveness, bi),
            // Without global facts every block starts from nothing known.
            None => forget_assigned(&facts, &mut st, &block.stmts),
        }
    }
    changed
}

/// Return the slots the statements assign to `Varying` (call results never leave it).
fn forget_assigned(facts: &Facts, st: &mut State, stmts: &[Stmt]) {
    for s in stmts {
        if let Stmt::Assign(dst, _) = s {
            if let Some(slot) = facts.slot(dst) {
                st[slot] = Lat::Varying;
            }
        }
    }
}

/// Replace reads of known locals by constants in an operand.
fn substitute(facts: &Facts, st: &State, op: &mut Operand) -> bool {
    let Operand::Copy(p) = op else { return false };
    let Some(slot) = facts.slot(p) else {
        return false;
    };
    let Lat::Known(v) = st[slot] else {
        return false;
    };
    let ty = facts.tys[p.local.0 as usize];
    *op = Operand::Const(v.to_const(ty), ty);
    true
}

fn rewrite_stmt(facts: &Facts, st: &State, s: &mut Stmt) -> bool {
    let mut changed = false;
    match s {
        Stmt::Assign(_, rv) => {
            rvalue_operands_mut(rv, &mut |op| changed |= substitute(facts, st, op));
            changed |= fold_rvalue(facts, st, rv);
        }
        Stmt::MemCopy { .. } | Stmt::MemCopyDyn { .. } | Stmt::MemSet { .. } => {
            stmt_operands_mut(s, &mut |op| changed |= substitute(facts, st, op));
        }
        Stmt::Nop => {}
    }
    changed
}

/// Evaluate a fully constant rvalue, or apply an algebraic identity.
fn fold_rvalue(facts: &Facts, st: &State, rv: &mut Rvalue) -> bool {
    if matches!(
        rv,
        Rvalue::Use(_) | Rvalue::AddrOf(_) | Rvalue::Aggregate(..)
    ) {
        return false;
    }
    let Some(ty) = result_ty(facts, rv) else {
        return false;
    };
    if let Some(v) = facts.eval(st, rv) {
        *rv = Rvalue::Use(Operand::Const(v.to_const(ty), ty));
        return true;
    }
    match identity(facts, rv, ty) {
        Some(new) => {
            *rv = new;
            true
        }
        None => false,
    }
}

/// Result type of an rvalue, when it can be told without aggregate layouts.
fn result_ty(facts: &Facts, rv: &Rvalue) -> Option<Ty> {
    Some(match rv {
        Rvalue::Cast(_, to) => *to,
        Rvalue::Unary(UnOp::Not, _) => Ty::Bool,
        Rvalue::Binary(
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge,
            ..,
        ) => Ty::Bool,
        Rvalue::Binary(BinOp::PtrAdd, ..) | Rvalue::AddrOf(_) => Ty::Ptr,
        Rvalue::Binary(BinOp::Shl | BinOp::Shr | BinOp::UShr, a, _) => facts.known_ty(a)?,
        Rvalue::Binary(_, a, b) => facts.known_ty(a).or_else(|| facts.known_ty(b))?,
        Rvalue::Unary(_, a) | Rvalue::Use(a) => facts.known_ty(a)?,
        Rvalue::Aggregate(id, _) => Ty::Agg(*id),
    })
}

/// Integer identities with one constant operand. Floats are left alone (`x + 0.0` is not
/// `x` for `x = -0.0`, and NaN breaks the multiplicative ones).
fn identity(facts: &Facts, rv: &Rvalue, ty: Ty) -> Option<Rvalue> {
    let Rvalue::Binary(op, a, b) = rv else {
        return None;
    };
    let operand_ty = match op {
        BinOp::Shl | BinOp::Shr | BinOp::UShr => facts.known_ty(a)?,
        _ => facts.known_ty(a).or_else(|| facts.known_ty(b))?,
    };
    if !operand_ty.is_int() || ty != operand_ty {
        return None;
    }
    let konst = |op: &Operand| match op {
        Operand::Const(c, t) => match Value::from_const(c, *t) {
            Some(Value::Int(v)) => Some(v),
            _ => None,
        },
        Operand::Copy(_) => None,
    };
    let zero = || Some(Rvalue::Use(Operand::Const(Const::Int(0), ty)));
    let keep = |x: &Operand| Some(Rvalue::Use(x.clone()));
    let all_ones = value::normalize(-1, ty);
    match (op, konst(a), konst(b)) {
        (BinOp::Add | BinOp::BitOr | BinOp::BitXor, Some(0), _) => keep(b),
        (BinOp::Mul, Some(1), _) => keep(b),
        (BinOp::BitAnd, Some(m), _) if m == all_ones => keep(b),
        (BinOp::Mul | BinOp::BitAnd, Some(0), _) => zero(),
        (
            BinOp::Add
            | BinOp::Sub
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::Shl
            | BinOp::Shr
            | BinOp::UShr,
            _,
            Some(0),
        ) => keep(a),
        (BinOp::Mul | BinOp::Div, _, Some(1)) => keep(a),
        (BinOp::BitAnd, _, Some(m)) if m == all_ones => keep(a),
        (BinOp::Mul | BinOp::BitAnd, _, Some(0)) => zero(),
        _ => None,
    }
}

fn rewrite_term(facts: &Facts, st: &State, signatures: &[Signature], t: &mut Terminator) -> bool {
    let mut changed = false;
    term_operands_mut(t, &mut |op| changed |= substitute(facts, st, op));
    let jump = match t {
        Terminator::Branch { .. } | Terminator::Switch { .. } => match facts.feasible(st, t)[..] {
            [only] => Some(only),
            _ => None,
        },
        _ => None,
    };
    if let Some(target) = jump {
        *t = Terminator::Goto(target);
        return true;
    }
    changed | devirtualize(signatures, t)
}

/// `call (const fn#N)(…)` → `call fn#N(…)` when the signatures agree.
fn devirtualize(signatures: &[Signature], t: &mut Terminator) -> bool {
    let Terminator::Call { callee, .. } = t else {
        return false;
    };
    let Callee::Ptr {
        target: Operand::Const(Const::Func(id), _),
        params,
        ret,
    } = callee
    else {
        return false;
    };
    match signatures.get(id.0 as usize) {
        Some((p, r)) if p == params && r == ret => {
            *callee = Callee::Func(*id);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;
