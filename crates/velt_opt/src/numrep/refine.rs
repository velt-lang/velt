//! Branch refinement for `numrep`: which comparison decides a branch, and what it proves about
//! the compared values on each edge.

use velt_vir::vir::{BinOp, Function, Local, Operand, Rvalue, Stmt, Terminator, Ty, UnOp};

use super::fact::Fact;
use super::flow::is_comparison;

/// The comparison `lhs op rhs` deciding a branch; `negated` when the branch tests its negation.
pub(super) struct Condition {
    pub op: BinOp,
    pub lhs: Operand,
    pub rhs: Operand,
    pub negated: bool,
}

/// The last whole assignment to `l` among `stmts[..end]`.
fn last_def(stmts: &[Stmt], l: Local, end: usize) -> Option<(usize, &Rvalue)> {
    stmts[..end]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, s)| match s {
            Stmt::Assign(d, rv) if d.local == l => d.proj.is_empty().then_some((i, rv)),
            _ => None,
        })
}

fn assigned_in(stmts: &[Stmt], op: &Operand) -> bool {
    let Operand::Copy(p) = op else { return false };
    stmts
        .iter()
        .any(|s| matches!(s, Stmt::Assign(d, _) if d.local == p.local))
}

/// The comparison deciding block `b`'s branch, defined in the block (through copies and `!`),
/// with neither operand reassigned after it.
pub(super) fn condition(func: &Function, b: usize) -> Option<Condition> {
    let block = &func.blocks[b];
    let Terminator::Branch {
        cond: Operand::Copy(c),
        ..
    } = &block.term
    else {
        return None;
    };
    if !c.proj.is_empty() {
        return None;
    }
    let (mut local, mut end, mut negated) = (c.local, block.stmts.len(), false);
    for _ in 0..4 {
        let (at, rv) = last_def(&block.stmts, local, end)?;
        match rv {
            Rvalue::Use(Operand::Copy(p)) if p.proj.is_empty() => local = p.local,
            Rvalue::Unary(UnOp::Not, Operand::Copy(p)) if p.proj.is_empty() => {
                local = p.local;
                negated = !negated;
            }
            Rvalue::Binary(op, x, y) if is_comparison(*op) => {
                let later = &block.stmts[at + 1..];
                if assigned_in(later, x) || assigned_in(later, y) {
                    return None;
                }
                return Some(Condition {
                    op: *op,
                    lhs: x.clone(),
                    rhs: y.clone(),
                    negated,
                });
            }
            _ => return None,
        }
        // The copied local must still hold its value where the condition reads it.
        if block.stmts[at..end]
            .iter()
            .any(|s| matches!(s, Stmt::Assign(d, _) if d.local == local))
        {
            return None;
        }
        end = at;
    }
    None
}

/// The local `op` copies or converts from an integer in block `b` (unchanged since, as is
/// `op`): refining `op` refines it too. The flag says the conversion is int → `f64`, which is
/// exact only within ±2^53.
pub(super) fn converted_from(func: &Function, b: usize, op: &Operand) -> Option<(Operand, bool)> {
    let Operand::Copy(p) = op else { return None };
    if !p.proj.is_empty() {
        return None;
    }
    let stmts = &func.blocks[b].stmts;
    let (at, rv) = last_def(stmts, p.local, stmts.len())?;
    let (src, cast) = match rv {
        Rvalue::Use(src @ Operand::Copy(z)) if z.proj.is_empty() => (src, false),
        Rvalue::Cast(src @ Operand::Copy(z), Ty::F64)
            if z.proj.is_empty() && func.locals[z.local.0 as usize].ty.is_int() =>
        {
            (src, true)
        }
        _ => return None,
    };
    (!assigned_in(&stmts[at..], src)).then(|| (src.clone(), cast))
}

fn negate(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Ge,
        BinOp::Le => BinOp::Gt,
        BinOp::Gt => BinOp::Le,
        BinOp::Ge => BinOp::Lt,
        BinOp::Eq => BinOp::Ne,
        _ => BinOp::Eq,
    }
}

/// The facts about `x` and `y` given that `x op y` is `holds`; `None` if that is impossible.
pub(super) fn refine(op: BinOp, holds: bool, x: Fact, y: Fact) -> Option<(Fact, Fact)> {
    let rel = if holds {
        op
    } else if x.nan || y.nan {
        // A false comparison may just mean NaN.
        return Some((x, y));
    } else {
        negate(op)
    };
    if rel == BinOp::Ne {
        return Some((x, y));
    }
    // Every other relation that holds rules NaN out.
    let (mut x, mut y) = (Fact { nan: false, ..x }, Fact { nan: false, ..y });
    match rel {
        BinOp::Lt => {
            x.hi = x.hi.min(y.hi.next_down());
            y.lo = y.lo.max(x.lo.next_up());
        }
        BinOp::Le => {
            x.hi = x.hi.min(y.hi);
            y.lo = y.lo.max(x.lo);
        }
        BinOp::Gt | BinOp::Ge => {
            let flipped = if rel == BinOp::Gt {
                BinOp::Lt
            } else {
                BinOp::Le
            };
            let (ny, nx) = refine(flipped, true, y, x)?;
            return Some((nx, ny));
        }
        _ => {
            let (lo, hi) = (x.lo.max(y.lo), x.hi.min(y.hi));
            let integral = x.integral || y.integral;
            (x.lo, x.hi, x.integral) = (lo, hi, integral);
            (y.lo, y.hi, y.integral) = (lo, hi, integral);
        }
    }
    let (x, y) = (normalized(x), normalized(y));
    (!x.is_empty() && !y.is_empty()).then_some((x, y))
}

/// Whole bounds for whole values, and no `-0` once 0 is ruled out.
fn normalized(mut f: Fact) -> Fact {
    if f.integral {
        f.lo = f.lo.ceil();
        f.hi = f.hi.floor();
    }
    if !f.may_be_zero() {
        f.neg_zero = false;
    }
    f
}
