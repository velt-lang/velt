//! Which locals `numrep` narrows, and to what (`Plan`).
//!
//! - An `f64` local is narrowed when every definition is an integer-computable form
//!   (`Use`, a conversion of an integer, `neg`, `+ - * %`) whose operands and result are whole,
//!   never NaN and strictly within ±2^53: integer arithmetic gives exactly the double's value
//!   there. It becomes an `i32` when all its values fit, an `i64` otherwise.
//! - An `i64` local is narrowed to `i32` when every definition copies or converts a 32-bit
//!   value (step 1): a constant, a small integer, or another local narrowed to `i32`.
//! - `-0` is the one double an integer cannot hold. A local that may be `-0` is narrowed only
//!   when no read can tell `-0` from 0: comparisons, conversions to integers, ToInt32,
//!   `__floatIndex`, the divisor of `%`, adding a value that is never `-0`, subtracting a
//!   non-zero one, and the operands of other narrowed definitions (whose own facts account for
//!   the `-0`). That last rule makes this a greatest fixpoint.

use std::collections::HashMap;

use velt_vir::vir::{BinOp, Function, Local, Operand, Rvalue, Stmt, Terminator, Ty, UnOp};

use super::fact::Fact;
use super::flow::{is_comparison, operand_ty, Flow, State};
use super::Env;
use crate::locals::Usage;

/// Why a local stays as it is (for `--report numbers`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Reason {
    /// A definition may not be a whole number.
    NotWhole,
    /// A definition may be NaN.
    MayBeNan,
    /// A definition may reach ±2^53 (its interval).
    Unbounded(f64, f64),
    /// A definition is not an integer-computable operation (a call, a load, a division).
    Form,
    /// The value may be `-0`, and a read can tell (block, statement or terminator index).
    NegZero(usize, usize),
    /// An `i64` local set from a value that is not a 32-bit integer.
    Wide,
}

/// The narrowing decisions for one function.
pub(super) struct Plan {
    /// Per local: its new type, when narrowed.
    pub to: Vec<Option<Ty>>,
    /// Per local: the join of the values it is assigned.
    pub range: Vec<Fact>,
    /// Facts about the operands of each narrowed definition, by (block, statement).
    pub operands: HashMap<(usize, usize), Vec<Fact>>,
    /// Per candidate that stays: why.
    pub reasons: Vec<Option<Reason>>,
}

/// Where a candidate is read, and whether that read can see `-0`.
struct Read {
    local: Local,
    at: (usize, usize),
    /// The read cannot tell `-0` from 0 whatever happens.
    blind: bool,
    /// ... or it cannot when this local is narrowed (the read is an operand of its definition).
    into: Option<Local>,
}

/// The locals `numrep` may narrow: non-param register `f64` locals, and `i64` ones assigned a
/// small integer somewhere, that are only ever assigned as a whole by statements.
fn candidates(func: &Function) -> Vec<bool> {
    let usage = Usage::of(func);
    let mut keep: Vec<bool> = (0..func.locals.len())
        .map(|i| {
            let l = Local(i as u32);
            let u = usage.get(l);
            i >= func.params.len()
                && matches!(func.locals[i].ty, Ty::F64 | Ty::I64)
                && usage.is_register(l)
                && u.defs > 0
                && u.partial_defs == 0
        })
        .collect();
    for b in &func.blocks {
        if let Terminator::Call { dest: Some(d), .. } = &b.term {
            keep[d.local.0 as usize] = false;
        }
    }
    keep
}

/// Decide what to narrow in `func`.
pub(super) fn plan(env: &Env, flow: &Flow, func: &Function) -> Plan {
    let n = func.locals.len();
    let cand = candidates(func);
    let mut p = Plan {
        to: vec![None; n],
        range: vec![Fact::empty(); n],
        operands: HashMap::new(),
        reasons: vec![None; n],
    };
    let mut may_neg_zero = vec![false; n];
    let mut sources: Vec<(Local, Operand)> = vec![];
    let mut reads: Vec<Read> = vec![];
    for (bi, block) in func.blocks.iter().enumerate() {
        let Some(entry) = flow.entry(bi) else {
            continue;
        };
        let mut st: State = entry.clone();
        for (si, s) in block.stmts.iter().enumerate() {
            if let Stmt::Assign(d, rv) = s {
                collect_reads(flow, func, &st, &cand, rv, d, (bi, si), &mut reads);
                if d.proj.is_empty() && cand[d.local.0 as usize] {
                    let l = d.local;
                    let r = flow.rvalue(&st, func, rv, func.locals[l.0 as usize].ty);
                    p.range[l.0 as usize] = p.range[l.0 as usize].join(r);
                    may_neg_zero[l.0 as usize] |= r.neg_zero;
                    let ops = operand_facts(flow, func, &st, rv);
                    if func.locals[l.0 as usize].ty == Ty::F64 {
                        if let Err(why) = f64_def(rv, r, &ops, func) {
                            p.reasons[l.0 as usize].get_or_insert(why);
                        }
                    } else {
                        match i64_source(func, rv, &ops) {
                            Ok(Some(src)) => sources.push((l, src)),
                            Ok(None) => {}
                            Err(why) => {
                                p.reasons[l.0 as usize].get_or_insert(why);
                            }
                        }
                    }
                    p.operands.insert((bi, si), ops);
                }
            } else {
                collect_stmt_reads(&cand, s, (bi, si), &mut reads);
            }
            flow.transfer(&mut st, func, s);
        }
        collect_term_reads(env, &cand, &block.term, (bi, block.stmts.len()), &mut reads);
    }
    for l in 0..n {
        if cand[l] && p.reasons[l].is_none() && p.range[l].lo <= p.range[l].hi {
            let i32_ = p.range[l].fits(Ty::I32) || func.locals[l].ty == Ty::I64;
            p.to[l] = Some(if i32_ { Ty::I32 } else { Ty::I64 });
        } else if cand[l] && p.reasons[l].is_none() {
            // Never assigned on a reachable path: nothing to gain.
            p.reasons[l] = Some(Reason::Form);
        }
    }
    settle(&mut p, &may_neg_zero, &sources, &reads);
    p
}

/// Drop narrowed locals until every rule holds (greatest fixpoint).
fn settle(p: &mut Plan, may_neg_zero: &[bool], sources: &[(Local, Operand)], reads: &[Read]) {
    loop {
        let mut changed = false;
        for (l, src) in sources {
            let ok = match src {
                Operand::Copy(q) => p.to[q.local.0 as usize] == Some(Ty::I32),
                Operand::Const(..) => true,
            };
            if !ok && p.to[l.0 as usize].is_some() {
                p.to[l.0 as usize] = None;
                p.reasons[l.0 as usize] = Some(Reason::Wide);
                changed = true;
            }
        }
        for r in reads {
            let i = r.local.0 as usize;
            if p.to[i].is_none() || !may_neg_zero[i] || r.blind {
                continue;
            }
            if r.into.is_some_and(|d| p.to[d.0 as usize].is_some()) {
                continue;
            }
            p.to[i] = None;
            p.reasons[i] = Some(Reason::NegZero(r.at.0, r.at.1));
            changed = true;
        }
        if !changed {
            return;
        }
    }
}

fn operand_facts(flow: &Flow, func: &Function, st: &State, rv: &Rvalue) -> Vec<Fact> {
    let mut ops = vec![];
    crate::visit::rvalue_operands(rv, &mut |op| {
        ops.push(flow.operand(st, func, op).unwrap_or(Fact::top(Ty::F64)))
    });
    ops
}

/// Can this definition of an `f64` local (result `r`, operands `ops`) be computed exactly with
/// integers?
fn f64_def(rv: &Rvalue, r: Fact, ops: &[Fact], func: &Function) -> Result<(), Reason> {
    if !r.integral {
        return Err(Reason::NotWhole);
    }
    if r.nan {
        return Err(Reason::MayBeNan);
    }
    if !r.exact_int() {
        return Err(Reason::Unbounded(r.lo, r.hi));
    }
    let form = match rv {
        Rvalue::Use(_) | Rvalue::Unary(UnOp::Neg, _) => true,
        Rvalue::Cast(op, Ty::F64) => {
            let t = operand_ty(func, op);
            t.is_int() || t == Ty::Bool
        }
        Rvalue::Binary(BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Rem, ..) => true,
        _ => false,
    };
    if form && ops.iter().all(Fact::exact_int) {
        Ok(())
    } else {
        Err(Reason::Form)
    }
}

/// The 32-bit value an `i64` definition copies or converts: `Ok(None)` for a constant or a
/// small integer, `Ok(Some(local))` for a local that must itself become `i32`.
fn i64_source(func: &Function, rv: &Rvalue, ops: &[Fact]) -> Result<Option<Operand>, Reason> {
    let (Rvalue::Use(op) | Rvalue::Cast(op, _)) = rv else {
        return Err(Reason::Wide);
    };
    if !ops.first().is_some_and(|f| f.fits(Ty::I32)) {
        return Err(Reason::Wide);
    }
    match op {
        Operand::Copy(p)
            if matches!(operand_ty(func, op), Ty::F64 | Ty::I64) && p.proj.is_empty() =>
        {
            Ok(Some(op.clone()))
        }
        Operand::Copy(_) if super::int32::small_int(operand_ty(func, op)) => Ok(None),
        Operand::Copy(_) => Err(Reason::Wide),
        Operand::Const(..) => Ok(None),
    }
}

/// Record the reads of candidates in `rv`, assigned to `d`.
#[allow(clippy::too_many_arguments)] // one call site; the facts at the point are all needed
fn collect_reads(
    flow: &Flow,
    func: &Function,
    st: &State,
    cand: &[bool],
    rv: &Rvalue,
    d: &velt_vir::vir::Place,
    at: (usize, usize),
    reads: &mut Vec<Read>,
) {
    let fact = |op: &Operand| flow.operand(st, func, op).unwrap_or(Fact::top(Ty::F64));
    let into = (d.proj.is_empty() && cand[d.local.0 as usize]).then_some(d.local);
    let mut push = |op: &Operand, blind: bool| {
        if let Operand::Copy(p) = op {
            if p.proj.is_empty() && cand[p.local.0 as usize] {
                reads.push(Read {
                    local: p.local,
                    at,
                    blind,
                    into,
                });
            }
        }
    };
    match rv {
        Rvalue::Binary(op, a, b) if is_comparison(*op) => {
            push(a, true);
            push(b, true);
        }
        Rvalue::Cast(a, to) if to.is_int() => push(a, true),
        Rvalue::Binary(BinOp::Rem, a, b) => {
            push(a, false);
            push(b, true);
        }
        Rvalue::Binary(BinOp::Add, a, b) => {
            push(a, !fact(b).neg_zero);
            push(b, !fact(a).neg_zero);
        }
        Rvalue::Binary(BinOp::Sub, a, b) => {
            push(a, !fact(b).may_be_zero());
            push(b, !fact(a).neg_zero);
        }
        _ => crate::visit::rvalue_operands(rv, &mut |op| push(op, false)),
    }
}

/// Reads in a statement other than an assignment can all see `-0` (`MemSet`, copies).
fn collect_stmt_reads(cand: &[bool], s: &Stmt, at: (usize, usize), reads: &mut Vec<Read>) {
    crate::visit::stmt_operands(s, &mut |op| {
        if let Operand::Copy(p) = op {
            if p.proj.is_empty() && cand[p.local.0 as usize] {
                reads.push(Read {
                    local: p.local,
                    at,
                    blind: false,
                    into: None,
                });
            }
        }
    });
}

/// Reads in a terminator: ToInt32 and `__floatIndex` cannot see `-0`; calls and returns can.
fn collect_term_reads(
    env: &Env,
    cand: &[bool],
    t: &Terminator,
    at: (usize, usize),
    reads: &mut Vec<Read>,
) {
    let blind = match t {
        Terminator::Call { callee, .. } => env.is_to_int32(callee) || env.is_float_index(callee),
        _ => false,
    };
    crate::visit::term_operands(t, &mut |op| {
        if let Operand::Copy(p) = op {
            if p.proj.is_empty() && cand[p.local.0 as usize] {
                reads.push(Read {
                    local: p.local,
                    at,
                    blind,
                    into: None,
                });
            }
        }
    });
}
