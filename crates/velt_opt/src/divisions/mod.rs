//! Cheaper signed divisions and remainders by constants (FINDINGS 8.8).
//!
//! Integers wrap, so LLVM rarely knows that a signed dividend is non-negative, and `x / 2` on
//! `i64` becomes a shift plus a sign fix-up (Cranelift: a hardware divide). This pass rewrites
//! `x / c` and `x % c` on signed integers, `c > 1` a constant, when it can prove:
//! - **`x` is a multiple of `c = 2^k`** (`parity`: `(i + j) * (i + j + 1)` is even): the
//!   division is exact, so `x / c` is the arithmetic shift `x >> k` for every sign, and
//!   `x % c` is 0;
//! - **`x >= 0`** (`range`: counters that start at 0 and grow under a `<` test, sums and
//!   products of such values that cannot wrap, remainders, masks): `x / 2^k` is `x >> k`,
//!   `x % 2^k` is `x & (2^k - 1)`, and other constants divide unsigned (`cast`, `udiv`/`urem`,
//!   `cast` back), which is cheaper than a signed division by a constant.

mod parity;
mod range;
mod slice;

use velt_vir::vir::{BinOp, Const, Function, Local, LocalDecl, Operand, Place, Rvalue, Stmt, Ty};

use crate::locals::Usage;
use crate::srclocs::rewrite_stmts;
use parity::Parity;
use range::Ranges;

/// A division this pass can make cheaper.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Proof {
    /// The dividend is a multiple of the (power-of-two) divisor.
    Exact,
    /// The dividend is non-negative.
    NonNegative,
}

/// Rewrite the provably cheaper divisions of `func`; returns whether anything changed.
pub(crate) fn run(func: &mut Function) -> bool {
    let seeds: Vec<Local> = func
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter_map(candidate)
        .filter_map(|(_, x, _, _)| match x {
            Operand::Copy(p) if p.proj.is_empty() => Some(p.local),
            _ => None,
        })
        .collect();
    if seeds.is_empty() {
        return false;
    }
    let proofs = prove(func, &seeds);
    if proofs.is_empty() {
        return false;
    }
    apply(func, proofs);
    true
}

/// `dst = x op c` with `op` a signed `Div`/`Rem` and `c > 1` a constant: (op, x, c, type).
fn candidate(s: &Stmt) -> Option<(BinOp, &Operand, i128, Ty)> {
    match s {
        Stmt::Assign(_, Rvalue::Binary(op @ (BinOp::Div | BinOp::Rem), x, c)) => match c {
            Operand::Const(Const::Int(c), ty) if ty.is_signed() && *c > 1 => {
                Some((*op, x, *c, *ty))
            }
            _ => None,
        },
        _ => None,
    }
}

/// `(block, statement, proof)` for every candidate the analyses prove cheaper.
fn prove(func: &Function, seeds: &[Local]) -> Vec<(usize, usize, Proof)> {
    let usage = Usage::of(func);
    let ranges = Ranges::compute(func, seeds);
    let mut out = vec![];
    for (bi, block) in func.blocks.iter().enumerate() {
        if !block.stmts.iter().any(|s| candidate(s).is_some()) {
            continue;
        }
        let mut parity = Parity::new(&usage);
        let mut state = ranges.as_ref().and_then(|r| r.entry(bi));
        for (si, s) in block.stmts.iter().enumerate() {
            if let Some((_, x, c, ty)) = candidate(s) {
                let exact = c.count_ones() == 1 && parity.zeros(x, ty) >= c.trailing_zeros();
                let non_negative = match (&ranges, &state) {
                    (Some(r), Some(st)) => r.operand(st, func, x).is_some_and(|i| i.lo >= 0),
                    _ => false,
                };
                if exact {
                    out.push((bi, si, Proof::Exact));
                } else if non_negative {
                    out.push((bi, si, Proof::NonNegative));
                }
            }
            parity.step(s, |l| func.locals[l.0 as usize].ty);
            if let (Some(r), Some(st)) = (&ranges, &mut state) {
                r.transfer(st, func, s);
            }
        }
    }
    out
}

/// Rewrite the proven statements.
fn apply(func: &mut Function, proofs: Vec<(usize, usize, Proof)>) {
    // Divisions by other constants than powers of two need an unsigned temporary each.
    let mut temps = Vec::with_capacity(proofs.len());
    for &(bi, si, _) in &proofs {
        let temp = match candidate(&func.blocks[bi].stmts[si]) {
            Some((_, _, c, ty)) if c.count_ones() != 1 => {
                func.locals.push(LocalDecl {
                    ty: unsigned(ty),
                    name: None,
                });
                Some(Local(func.locals.len() as u32 - 1))
            }
            _ => None,
        };
        temps.push(temp);
    }
    let mut blocks: Vec<usize> = proofs.iter().map(|p| p.0).collect();
    blocks.dedup();
    for bi in blocks {
        let mut index = 0;
        rewrite_stmts(func, bi, |s, out| {
            let hit = proofs.iter().position(|p| p.0 == bi && p.1 == index);
            index += 1;
            match hit {
                Some(i) => out.extend(cheaper(s, proofs[i].2, temps[i])),
                None => out.push(s),
            }
        });
    }
}

/// The cheaper form of candidate `s` (`temp`: an unsigned local, for non-power-of-two
/// divisors).
fn cheaper(s: Stmt, proof: Proof, temp: Option<Local>) -> Vec<Stmt> {
    let Stmt::Assign(dst, Rvalue::Binary(op, x, Operand::Const(Const::Int(c), ty))) = s else {
        return vec![s];
    };
    let int = |v: i128| Operand::Const(Const::Int(v), ty);
    let k = i128::from(c.trailing_zeros());
    let rv = match (op, proof, temp) {
        (BinOp::Div, _, None) => Rvalue::Binary(BinOp::Shr, x, int(k)),
        (BinOp::Rem, Proof::Exact, None) => Rvalue::Use(int(0)),
        (BinOp::Rem, Proof::NonNegative, None) => Rvalue::Binary(BinOp::BitAnd, x, int(c - 1)),
        (_, _, Some(t)) => {
            let u = unsigned(ty);
            let t = Place::local(t);
            let divisor = Operand::Const(Const::Int(c), u);
            let quotient = Rvalue::Binary(op, Operand::Copy(t.clone()), divisor);
            return vec![
                Stmt::Assign(t.clone(), Rvalue::Cast(x, u)),
                Stmt::Assign(t.clone(), quotient),
                Stmt::Assign(dst, Rvalue::Cast(Operand::Copy(t), ty)),
            ];
        }
        _ => return vec![Stmt::Assign(dst, Rvalue::Binary(op, x, int(c)))],
    };
    vec![Stmt::Assign(dst, rv)]
}

/// The unsigned type of the same width.
fn unsigned(ty: Ty) -> Ty {
    match ty {
        Ty::I8 => Ty::U8,
        Ty::I16 => Ty::U16,
        Ty::I32 => Ty::U32,
        _ => Ty::U64,
    }
}

#[cfg(test)]
mod tests;
