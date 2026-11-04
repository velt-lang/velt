//! A comparison of an integer converted to a double with a double the facts know is a whole
//! number below 2^53 in magnitude, made in integers:
//!
//! ```text
//! t = cast i as f64        // i: u64 (or any integer type)
//! c = lt t, n              // n whole, never NaN, |n| < 2^53 (an array length, say)
//! ```
//!
//! becomes `c = lt i, n as u64` (`i64` for a signed `i`). It is exact for every `i`: below 2^53
//! both convert exactly; above it, `i as f64` is still at least 2^53 (rounding is monotone), so
//! it compares with `n` as `i` does, and likewise below -2^53. An unsigned `i` against an `n`
//! that may be negative (`len - 15`, `len - len % 64`, whose intervals reach below 0) tests the
//! sign of `n` too: `i < n` is `n > 0 && i < n as u64`, `i > n` is `n < 0 || i > n as u64`.
//!
//! The loop `for (let i: usize = 0; i < n; i++)` over `const n = xs.length` then tests its exit
//! in integers, which keeps its trip count computable (LLVM vectorizes it) and its bounds checks
//! provable.

use velt_vir::vir::{BinOp, Function, Local, LocalDecl, Operand, Place, Rvalue, Stmt, Ty};

use super::flow::{is_comparison, operand_ty, Flow, State};

/// A comparison at statement `si` that `rewrite` makes in integers: which operand is the
/// converted integer (`left`), the integer it was converted from, and the type to compare in.
pub(super) struct Plan {
    si: usize,
    left: bool,
    int: Operand,
    ty: Ty,
    /// An unsigned integer against a double that may be negative: the sign is tested too.
    signed_bound: bool,
}

/// The integer conversions to `f64` made so far in a block: the double local, the integer
/// operand it was converted from. A write to either ends the entry.
#[derive(Default)]
pub(super) struct Conversions {
    seen: Vec<(Local, Operand)>,
}

impl Conversions {
    /// Note statement `s` (after `plan` looked at it).
    pub(super) fn note(&mut self, func: &Function, s: &Stmt) {
        match s {
            Stmt::Assign(p, rv) => {
                if p.proj.is_empty() {
                    let d = p.local;
                    self.seen.retain(|(t, src)| *t != d && !reads(src, d));
                    if let Rvalue::Cast(src @ Operand::Copy(sp), Ty::F64) = rv {
                        let from = operand_ty(func, src);
                        if sp.proj.is_empty()
                            && from.is_int()
                            && func.locals[d.0 as usize].ty == Ty::F64
                        {
                            self.seen.push((d, src.clone()));
                        }
                    }
                } else {
                    // A store through a pointer may reach an address-taken local.
                    self.seen.clear();
                }
            }
            Stmt::MemCopy { .. } | Stmt::MemCopyDyn { .. } | Stmt::MemSet { .. } => {
                self.seen.clear()
            }
            Stmt::Nop => {}
        }
    }

    /// The integer `op` (a double local) was converted from in this block, if any.
    fn source(&self, op: &Operand) -> Option<&Operand> {
        let Operand::Copy(p) = op else { return None };
        if !p.proj.is_empty() {
            return None;
        }
        self.seen
            .iter()
            .find(|(t, _)| *t == p.local)
            .map(|(_, src)| src)
    }
}

fn reads(op: &Operand, l: Local) -> bool {
    matches!(op, Operand::Copy(p) if p.local == l)
}

/// Whether statement `si` (`s`) is such a comparison, given the facts `st` before it.
pub(super) fn plan(
    flow: &Flow,
    func: &Function,
    st: &State,
    conv: &Conversions,
    si: usize,
    s: &Stmt,
) -> Option<Plan> {
    let Stmt::Assign(_, Rvalue::Binary(op, a, b)) = s else {
        return None;
    };
    if !is_comparison(*op) || operand_ty(func, a) != Ty::F64 || operand_ty(func, b) != Ty::F64 {
        return None;
    }
    // Either operand may be the converted integer, and the other one the bounded double.
    let bounded = |other: &Operand| {
        flow.operand(st, func, other)
            .filter(|y| y.exact_int() && y.lo <= y.hi)
    };
    let (left, int, y) = [(true, a, b), (false, b, a)]
        .into_iter()
        .find_map(|(left, x, other)| Some((left, conv.source(x)?, bounded(other)?)))?;
    let from = operand_ty(func, int);
    let ty = if from.is_signed() { Ty::I64 } else { Ty::U64 };
    Some(Plan {
        si,
        left,
        int: int.clone(),
        ty,
        signed_bound: !from.is_signed() && y.lo < 0.0,
    })
}

/// Rewrite the comparisons `plans` (in statement order) of block `bi`.
pub(super) fn rewrite(func: &mut Function, bi: usize, plans: Vec<Plan>) {
    let fresh = |func: &mut Function, ty| {
        func.locals.push(LocalDecl::new(ty, None));
        Local(func.locals.len() as u32 - 1)
    };
    let mut temps = vec![];
    for p in &plans {
        let from = operand_ty(func, &p.int);
        let x = (from != p.ty).then(|| fresh(func, p.ty));
        let y = fresh(func, if p.signed_bound { Ty::I64 } else { p.ty });
        let signed = p.signed_bound.then(|| {
            [
                fresh(func, Ty::U64),
                fresh(func, Ty::Bool),
                fresh(func, Ty::Bool),
            ]
        });
        temps.push((x, y, signed));
    }
    let mut si = 0;
    let mut next = 0;
    crate::srclocs::rewrite_stmts(func, bi, |s, out| {
        let i = si;
        si += 1;
        let Some(p) = plans.get(next).filter(|p| p.si == i) else {
            out.push(s);
            return;
        };
        let (x_t, y_t, signed) = temps[next];
        next += 1;
        // The facts may have decided it meanwhile (`simplify::decided`): a constant is better.
        let Stmt::Assign(d, Rvalue::Binary(op, a, b)) = s else {
            out.push(s);
            return;
        };
        let (op, other) = match p.left {
            true => (op, b),
            false => (flipped(op), a),
        };
        let x = match x_t {
            Some(t) => {
                out.push(Stmt::Assign(
                    Place::local(t),
                    Rvalue::Cast(p.int.clone(), p.ty),
                ));
                Operand::Copy(Place::local(t))
            }
            None => p.int.clone(),
        };
        let y_ty = if p.signed_bound { Ty::I64 } else { p.ty };
        out.push(Stmt::Assign(Place::local(y_t), Rvalue::Cast(other, y_ty)));
        let y = Operand::Copy(Place::local(y_t));
        let Some([yu, cmp, sign]) = signed else {
            out.push(Stmt::Assign(d, Rvalue::Binary(op, x, y)));
            return;
        };
        // `x op y` for an unsigned `x` and a signed `y`: `y` past 0 on the side that decides,
        // then `x op (y as u64)`.
        let at = |l| Operand::Copy(Place::local(l));
        out.push(Stmt::Assign(
            Place::local(yu),
            Rvalue::Cast(y.clone(), Ty::U64),
        ));
        out.push(Stmt::Assign(
            Place::local(cmp),
            Rvalue::Binary(op, x, at(yu)),
        ));
        let zero = Operand::Const(velt_vir::vir::Const::Int(0), Ty::I64);
        let (test, join) = match op {
            BinOp::Lt => (BinOp::Gt, BinOp::BitAnd),
            BinOp::Le | BinOp::Eq => (BinOp::Ge, BinOp::BitAnd),
            _ => (BinOp::Lt, BinOp::BitOr), // Gt, Ge, Ne
        };
        out.push(Stmt::Assign(
            Place::local(sign),
            Rvalue::Binary(test, y, zero),
        ));
        out.push(Stmt::Assign(d, Rvalue::Binary(join, at(sign), at(cmp))));
    });
}

/// `op` with its operands swapped: `a < b` is `b > a`.
fn flipped(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::Le => BinOp::Ge,
        BinOp::Gt => BinOp::Lt,
        BinOp::Ge => BinOp::Le,
        op => op,
    }
}
