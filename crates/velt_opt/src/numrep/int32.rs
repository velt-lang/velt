//! ToInt32 of sums the facts cannot bound: an integer add behind a cheap range check.
//!
//! - **`velt_rt_math_add_int32(a, x)`** (ToInt32 of `a + x`, sema's `(y + i) | 0` with a float
//!   `i`) where `x` converts an `i64` `c`: when |c| <= 2^52 the double sum is exact, so it is
//!   the 32-bit sum `a + (c as i32)`; other values keep the call.
//! - **`velt_rt_math_to_int32(x ± y)`** where `x` and `y` convert `i64`s (or are whole
//!   constants), sema's `(a + b) | 0` of two inferred integers, which JS adds as doubles: when
//!   both are within ±2^52 the double sum is exact, so it is the `i64` sum truncated; other
//!   values add as doubles (an `a` at 2^53 rounds, #561).
//!
//! `narrow` already turned the sums it can bound into integer operations; these are the rest
//! (a counter compared with an unknown bound).

use velt_vir::vir::{
    BasicBlock, BinOp, BlockId, Callee, Const, Function, Local, LocalDecl, Operand, Place, Rvalue,
    Stmt, Terminator, Ty,
};

use super::{Env, ADD_INT32, TO_INT32};
use crate::locals::Usage;
use crate::srclocs::{push_stmt, rewrite_stmts};

pub(super) fn small_int(ty: Ty) -> bool {
    matches!(ty, Ty::I32 | Ty::I16 | Ty::I8 | Ty::U16 | Ty::U8)
}

/// Does `rv` copy or convert a small integer (an `i64` set from it is a narrowing seed)?
pub(super) fn from_small_int(func: &Function, rv: &Rvalue) -> bool {
    match rv {
        Rvalue::Use(op) | Rvalue::Cast(op, _) => small_int(super::flow::operand_ty(func, op)),
        _ => false,
    }
}

/// Guard the int32 sums of `func`; returns whether anything changed.
pub(super) fn guarded_sums(env: &Env, func: &mut Function) -> bool {
    let mut changed = false;
    for bi in 0..func.blocks.len() {
        if let Some(conv) = converted_sum(env, func, bi) {
            let c = converted_operand(func, conv);
            changed |= split_converted_sum(func, bi, c);
        } else if let Some(sum) = double_sum(env, func, bi) {
            split_double_sum(func, bi, sum);
            changed = true;
        }
    }
    changed
}

fn new_temp(func: &mut Function, ty: Ty) -> Local {
    func.locals.push(LocalDecl { ty, name: None });
    Local(func.locals.len() as u32 - 1)
}

/// The extern a block's call terminator calls, when it is `symbol`.
fn calls<'f>(env: &Env, func: &'f Function, bi: usize, symbol: &str) -> Option<&'f [Operand]> {
    match &func.blocks[bi].term {
        Terminator::Call {
            callee: Callee::Extern(id),
            args,
            ..
        } if env.symbol(*id) == symbol => Some(args),
        _ => None,
    }
}

/// The last whole assignment to the place `p` (a plain local) among `stmts[..end]`.
fn last_def<'s>(stmts: &'s [Stmt], p: &Place, end: usize) -> Option<(usize, &'s Rvalue)> {
    if !p.proj.is_empty() {
        return None;
    }
    stmts[..end]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, s)| match s {
            Stmt::Assign(d, rv) if d.local == p.local => d.proj.is_empty().then_some((i, rv)),
            _ => None,
        })
}

fn assigned(stmts: &[Stmt], l: Local) -> bool {
    stmts
        .iter()
        .any(|s| matches!(s, Stmt::Assign(d, _) if d.local == l))
}

fn is_i64(func: &Function, p: &Place) -> bool {
    p.proj.is_empty() && func.locals[p.local.0 as usize].ty == Ty::I64
}

/// The `i64` behind the `f64` operand `x` of the statement at `at` in block `bi`: a whole
/// constant within ±2^52, or a local converted from an `i64` (still unchanged at the end of the
/// block).
fn i64_behind(func: &Function, bi: usize, x: &Operand, at: usize) -> Option<Operand> {
    let stmts = &func.blocks[bi].stmts;
    match x {
        Operand::Const(Const::Float(v), Ty::F64)
            if v.fract() == 0.0 && v.abs() <= 4_503_599_627_370_496.0 =>
        {
            Some(Operand::Const(Const::Int(*v as i128), Ty::I64))
        }
        Operand::Copy(p) => {
            let (j, rv) = last_def(stmts, p, at)?;
            let Rvalue::Cast(c @ Operand::Copy(cp), Ty::F64) = rv else {
                return None;
            };
            (is_i64(func, cp) && !assigned(&stmts[j..], cp.local)).then(|| c.clone())
        }
        _ => None,
    }
}

/// `velt_rt_math_to_int32(t)` ending block `bi`, with `t = x ± y` set in the block from two
/// converted `i64`s (`i64_behind`): the operation and the two `i64`s.
fn double_sum(env: &Env, func: &Function, bi: usize) -> Option<(BinOp, Operand, Operand)> {
    let [Operand::Copy(t)] = calls(env, func, bi, TO_INT32)? else {
        return None;
    };
    let stmts = &func.blocks[bi].stmts;
    let (i, rv) = last_def(stmts, t, stmts.len())?;
    let Rvalue::Binary(op @ (BinOp::Add | BinOp::Sub), x, y) = rv else {
        return None;
    };
    let a = i64_behind(func, bi, x, i)?;
    let b = i64_behind(func, bi, y, i)?;
    // At least one side must vary, or there is nothing to guard.
    matches!((&a, &b), (Operand::Copy(_), _) | (_, Operand::Copy(_))).then(|| (*op, a, b))
}

/// Branch on |a|, |b| <= 2^52 to the truncated `i64` sum, keeping the double sum otherwise.
fn split_double_sum(func: &mut Function, bi: usize, (op, a, b): (BinOp, Operand, Operand)) {
    let Terminator::Call { dest, next, .. } = &func.blocks[bi].term else {
        return;
    };
    let (dest, next) = (dest.clone(), *next);
    let sum = new_temp(func, Ty::I64);
    let mut fast_stmts = vec![Stmt::Assign(
        Place::local(sum),
        Rvalue::Binary(op, a.clone(), b.clone()),
    )];
    if let Some(d) = dest {
        fast_stmts.push(Stmt::Assign(
            d,
            Rvalue::Cast(Operand::Copy(Place::local(sum)), Ty::I32),
        ));
    }
    let call = std::mem::replace(&mut func.blocks[bi].term, Terminator::Unreachable);
    let fast = add_block(func, fast_stmts, Terminator::Goto(next));
    let slow = add_block(func, vec![], call);
    let mut ok = None;
    for v in [a, b] {
        if let Operand::Copy(_) = v {
            let small = push_within_2_52(func, bi, v);
            ok = Some(match ok {
                None => small,
                Some(prev) => both(func, bi, prev, small),
            });
        }
    }
    let ok = ok.expect("ICE: numrep guarded a sum of two constants");
    func.blocks[bi].term = Terminator::Branch {
        cond: Operand::Copy(Place::local(ok)),
        then: fast,
        els: slow,
    };
}

/// Appends to block `bi` the conjunction of the `bool` locals `p` and `q`.
fn both(func: &mut Function, bi: usize, p: Local, q: Local) -> Local {
    let (p8, q8, m, r) = (
        new_temp(func, Ty::U8),
        new_temp(func, Ty::U8),
        new_temp(func, Ty::U8),
        new_temp(func, Ty::Bool),
    );
    let copy = |l: Local| Operand::Copy(Place::local(l));
    let stmts = [
        (p8, Rvalue::Cast(copy(p), Ty::U8)),
        (q8, Rvalue::Cast(copy(q), Ty::U8)),
        (m, Rvalue::Binary(BinOp::BitAnd, copy(p8), copy(q8))),
        (r, Rvalue::Cast(copy(m), Ty::Bool)),
    ];
    for (l, rv) in stmts {
        push_stmt(func, bi, Stmt::Assign(Place::local(l), rv), None);
    }
    r
}

/// Block `bi` ends in the call `velt_rt_math_add_int32(a, x)` with `x` converted from `c`:
/// branch on |c| <= 2^52 to a block with the integer sum, and to one with the call.
fn split_converted_sum(func: &mut Function, bi: usize, c: Operand) -> bool {
    let Terminator::Call {
        args, dest, next, ..
    } = &func.blocks[bi].term
    else {
        return false;
    };
    let (a, dest, next) = (args[0].clone(), dest.clone(), *next);
    let c32 = new_temp(func, Ty::I32);
    let call = std::mem::replace(&mut func.blocks[bi].term, Terminator::Unreachable);
    let mut fast_stmts = vec![Stmt::Assign(
        Place::local(c32),
        Rvalue::Cast(c.clone(), Ty::I32),
    )];
    if let Some(d) = dest {
        fast_stmts.push(Stmt::Assign(
            d,
            Rvalue::Binary(BinOp::Add, a, Operand::Copy(Place::local(c32))),
        ));
    }
    let fast = add_block(func, fast_stmts, Terminator::Goto(next));
    let slow = add_block(func, vec![], call);
    let ok = push_within_2_52(func, bi, c);
    func.blocks[bi].term = Terminator::Branch {
        cond: Operand::Copy(Place::local(ok)),
        then: fast,
        els: slow,
    };
    true
}

/// Appends to block `bi` the test |c| <= 2^52 of the `i64` `c`, as
/// `(c + 2^52) as u64 <= 2^53`; returns the `bool` local holding it.
fn push_within_2_52(func: &mut Function, bi: usize, c: Operand) -> Local {
    let (shifted, u, ok) = (
        new_temp(func, Ty::I64),
        new_temp(func, Ty::U64),
        new_temp(func, Ty::Bool),
    );
    let shift = Rvalue::Binary(BinOp::Add, c, Operand::Const(Const::Int(1 << 52), Ty::I64));
    let to_u64 = Rvalue::Cast(Operand::Copy(Place::local(shifted)), Ty::U64);
    let test = Rvalue::Binary(
        BinOp::Le,
        Operand::Copy(Place::local(u)),
        Operand::Const(Const::Int(1 << 53), Ty::U64),
    );
    for (l, rv) in [(shifted, shift), (u, to_u64), (ok, test)] {
        push_stmt(func, bi, Stmt::Assign(Place::local(l), rv), None);
    }
    ok
}

/// A new block (without source locations) at the end of `func`.
fn add_block(func: &mut Function, stmts: Vec<Stmt>, term: Terminator) -> BlockId {
    if !func.locs.is_empty() {
        func.locs.push(vec![None; stmts.len() + 1]);
    }
    func.blocks.push(BasicBlock { stmts, term });
    BlockId(func.blocks.len() as u32 - 1)
}

/// The `i64` operand `c` when block `bi` ends in `velt_rt_math_add_int32(a, x)` and `x` was last
/// set in the block by converting `c`, which is unchanged since.
fn converted_sum(env: &Env, func: &Function, bi: usize) -> Option<Converted> {
    let [_, Operand::Copy(x)] = calls(env, func, bi, ADD_INT32)? else {
        return None;
    };
    let block = &func.blocks[bi];
    if let Some((j, rv)) = last_def(&block.stmts, x, block.stmts.len()) {
        let Rvalue::Cast(c @ Operand::Copy(cp), Ty::F64) = rv else {
            return None;
        };
        let ok = is_i64(func, cp) && !assigned(&block.stmts[j..], cp.local);
        return ok.then(|| Converted::Here(c.clone()));
    }
    // Set once, elsewhere (a loop counter converted at the loop head): the value converted can be
    // kept next to it.
    let usage = Usage::of(func);
    let single = x.proj.is_empty()
        && x.local.0 as usize >= func.params.len()
        && usage.is_register(x.local)
        && usage.get(x.local).defs == 1;
    if !single {
        return None;
    }
    func.blocks.iter().enumerate().find_map(|(b, blk)| {
        blk.stmts.iter().enumerate().find_map(|(j, st)| match st {
            Stmt::Assign(d, Rvalue::Cast(c @ Operand::Copy(cp), Ty::F64))
                if d.local == x.local && d.proj.is_empty() && is_i64(func, cp) =>
            {
                Some(Converted::At(b, j, c.clone()))
            }
            _ => None,
        })
    })
}

/// Where the `i64` behind a converted operand is found (`converted_sum`).
enum Converted {
    /// This operand, unchanged since the conversion in the same block.
    Here(Operand),
    /// Converted by statement `.1` of block `.0`, the only definition of the converted local.
    At(usize, usize, Operand),
}

/// The `i64` operand behind a `Converted`: for `At`, a new local set right after the conversion
/// (so it always holds the value the converted local was made from).
fn converted_operand(func: &mut Function, conv: Converted) -> Operand {
    match conv {
        Converted::Here(c) => c,
        Converted::At(b, j, c) => {
            let snap = new_temp(func, Ty::I64);
            let mut k = 0;
            rewrite_stmts(func, b, |st, out| {
                out.push(st);
                if k == j {
                    out.push(Stmt::Assign(Place::local(snap), Rvalue::Use(c.clone())));
                }
                k += 1;
            });
            Operand::Copy(Place::local(snap))
        }
    }
}

#[cfg(test)]
#[path = "int32_tests.rs"]
mod tests;
