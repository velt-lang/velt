//! ToInt32 of sums the facts cannot bound: an integer add behind a cheap range check.
//!
//! **`math.to_int32(x ± y)`** where `x` and `y` convert `i64`s (or are whole constants),
//! sema's `(a + b) | 0` of two numbers held as integers, which JS adds as doubles: when both are
//! within ±2^52 the double sum is exact, so it is the `i64` sum truncated; other values add as
//! doubles (an `a` at 2^53 rounds, #561).
//!
//! `narrow` already turned the sums it can bound into integer operations; these are the rest
//! (a counter compared with an unknown bound).

use velt_vir::vir::{
    BasicBlock, BinOp, BlockId, Const, Function, Local, LocalDecl, Operand, Place, Rvalue, Stmt,
    Terminator, Ty,
};

use crate::srclocs::{push_stmt, split_after};
use velt_vir::math::MathOp;

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
pub(super) fn guarded_sums(func: &mut Function) -> bool {
    let mut changed = false;
    // Blocks split off below are visited too: a block may hold several such sums.
    let mut bi = 0;
    while bi < func.blocks.len() {
        if let Some((si, sum)) = statement_sum(func, bi) {
            // `d = math.to_int32(t)` at the end of its block, so that the slow side computes it
            // and both sides continue with the statements after it.
            let rest = split_after(func, bi, si);
            let s = func.blocks[bi]
                .stmts
                .pop()
                .expect("ICE: numrep split lost its statement");
            if let Some(locs) = func.locs.get_mut(bi) {
                locs.remove(si);
            }
            let Stmt::Assign(d, _) = &s else {
                unreachable!("ICE: numrep sum is not an assignment")
            };
            split_double_sum(
                func,
                bi,
                sum,
                Some(d.clone()),
                rest,
                (vec![s], Terminator::Goto(rest)),
            );
            changed = true;
        }
        bi += 1;
    }
    changed
}

/// The first `d = math.to_int32(t)` of block `bi` (`Rvalue::Intrinsic`) with `t = x ± y` set
/// before it in the block from two converted `i64`s: its index, the operation and the `i64`s.
fn statement_sum(func: &Function, bi: usize) -> Option<(usize, (BinOp, Operand, Operand))> {
    let stmts = &func.blocks[bi].stmts;
    stmts.iter().enumerate().find_map(|(si, s)| {
        let Stmt::Assign(d, rv) = s else { return None };
        let (MathOp::ToInt32, [Operand::Copy(t)]) = super::math_rvalue(rv)? else {
            return None;
        };
        if !d.proj.is_empty() {
            return None;
        }
        sum_before(func, bi, t, si).map(|sum| (si, sum))
    })
}

fn new_temp(func: &mut Function, ty: Ty) -> Local {
    func.locals.push(LocalDecl::new(ty, None));
    Local(func.locals.len() as u32 - 1)
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

/// `t = x ± y`, the last assignment to `t` among the first `end` statements of block `bi`,
/// from two converted `i64`s (`i64_behind`): the operation and the two `i64`s.
fn sum_before(
    func: &Function,
    bi: usize,
    t: &Place,
    end: usize,
) -> Option<(BinOp, Operand, Operand)> {
    let stmts = &func.blocks[bi].stmts;
    let (i, rv) = last_def(stmts, t, end)?;
    let Rvalue::Binary(op @ (BinOp::Add | BinOp::Sub), x, y) = rv else {
        return None;
    };
    let a = i64_behind(func, bi, x, i)?;
    let b = i64_behind(func, bi, y, i)?;
    // At least one side must vary, or there is nothing to guard.
    matches!((&a, &b), (Operand::Copy(_), _) | (_, Operand::Copy(_))).then(|| (*op, a, b))
}

/// End block `bi` with a branch on |a|, |b| <= 2^52 to the truncated `i64` sum in `dest`, then
/// `next`; otherwise to a block of `slow` (statements and terminator), which computes the
/// double sum's ToInt32 into `dest` and continues to `next` itself.
fn split_double_sum(
    func: &mut Function,
    bi: usize,
    (op, a, b): (BinOp, Operand, Operand),
    dest: Option<Place>,
    next: BlockId,
    slow: (Vec<Stmt>, Terminator),
) {
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
    let fast = add_block(func, fast_stmts, Terminator::Goto(next));
    let slow = add_block(func, slow.0, slow.1);
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

#[cfg(test)]
#[path = "int32_tests.rs"]
pub(super) mod tests;
