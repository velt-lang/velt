//! How statements, branches and the frozen params' fields change a state of `bounds`.

use velt_vir::vir::{
    AggLayout, BinOp, Const, Function, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty,
};

use super::domain::*;
use crate::visit::successors;

/// The function, and the scalar fields of its `frozen` params read in it: each is one value for
/// the whole call (vir.rs invariant 9), a term numbered after the locals, so the length of a
/// borrowed array read twice is one term.
pub(super) struct Cx<'a> {
    pub(super) func: &'a Function,
    pub(super) frozen: Vec<(u32, Vec<Proj>)>,
}

impl Cx<'_> {
    /// The term of a read of a frozen param's scalar field.
    pub(super) fn frozen_term(&self, pl: &Place) -> Option<u32> {
        let i = self
            .frozen
            .iter()
            .position(|(l, proj)| *l == pl.local.0 && *proj == pl.proj)?;
        Some(self.func.locals.len() as u32 + i as u32)
    }
}

/// The frozen params' scalar unsigned fields that `func` reads, for [`Cx`]. A param the
/// function assigns or takes the address of is left out.
pub(super) fn frozen_fields(aggs: &[AggLayout], func: &Function) -> Vec<(u32, Vec<Proj>)> {
    let mut out: Vec<(u32, Vec<Proj>)> = vec![];
    let frozen: Vec<bool> = (0..func.params.len())
        .map(|i| func.params[i] == Ty::Ptr && func.param_attr(i).frozen)
        .collect();
    if !frozen.contains(&true) {
        return out;
    }
    let mut spoiled = vec![false; frozen.len()];
    let mut reads: Vec<Place> = vec![];
    for b in &func.blocks {
        for s in &b.stmts {
            crate::visit::stmt_places(s, &mut |pl, u| {
                let l = pl.local.0 as usize;
                if l >= frozen.len() || !frozen[l] {
                    return;
                }
                match u {
                    crate::visit::PlaceUse::Read => {}
                    _ if pl.proj.is_empty() => spoiled[l] = true,
                    _ => {}
                }
            });
            crate::visit::stmt_operands(s, &mut |op| {
                if let Operand::Copy(pl) = op {
                    reads.push(pl.clone());
                }
            });
        }
        if let Terminator::Call { dest: Some(d), .. } = &b.term {
            if (d.local.0 as usize) < frozen.len() {
                spoiled[d.local.0 as usize] = true;
            }
        }
        crate::visit::term_operands(&b.term, &mut |op| {
            if let Operand::Copy(pl) = op {
                reads.push(pl.clone());
            }
        });
    }
    for pl in &reads {
        let l = pl.local.0 as usize;
        if l >= frozen.len() || !frozen[l] || spoiled[l] {
            continue;
        }
        let Some(Proj::Deref(mut ty)) = pl.proj.first().cloned() else {
            continue;
        };
        let mut ok = true;
        for x in &pl.proj[1..] {
            match (x, ty) {
                (Proj::Field(f), Ty::Agg(a)) => match aggs
                    .get(a.0 as usize)
                    .and_then(|g| g.fields.get(*f as usize))
                {
                    Some(&(t, _)) => ty = t,
                    None => ok = false,
                },
                _ => ok = false,
            }
        }
        if ok && unsigned(ty) && !out.iter().any(|(m, p)| *m == pl.local.0 && *p == pl.proj) {
            out.push((pl.local.0, pl.proj.clone()));
        }
    }
    out
}

pub(super) fn transfer_block(cx: &Cx, tracked: &[bool], bi: usize, mut st: State) -> State {
    for s in &cx.func.blocks[bi].stmts {
        transfer_stmt(cx, tracked, &mut st, s);
    }
    if let Terminator::Call { dest: Some(d), .. } = &cx.func.blocks[bi].term {
        if tracked.get(d.local.0 as usize).copied().unwrap_or(false) {
            kill(&mut st, d.local.0);
        }
    }
    st
}

pub(super) fn transfer_stmt(cx: &Cx, tracked: &[bool], st: &mut State, s: &Stmt) {
    let Stmt::Assign(place, rv) = s else { return };
    let v = place.local.0;
    if !place.proj.is_empty() {
        // A write into a local's memory (an aggregate): no integer local changes.
        return;
    }
    if !tracked.get(v as usize).copied().unwrap_or(false) {
        // Nothing in a state names an untracked local: values are based on tracked locals,
        // and facts and conditions name only them (and frozen fields).
        return;
    }
    let ty = cx.func.locals[v as usize].ty;
    if ty.is_signed() {
        let from = match rv {
            Rvalue::Cast(Operand::Copy(src), _) if src.proj.is_empty() => Some(src.local.0),
            _ => None,
        };
        kill(st, v);
        if let Some(u) = from.filter(|u| {
            u != &v
                && tracked.get(*u as usize).copied().unwrap_or(false)
                && unsigned(cx.func.locals[*u as usize].ty)
        }) {
            st.casts.push((v, u));
        }
        return;
    }
    if ty == Ty::Bool {
        let cond = match rv {
            Rvalue::Binary(op @ (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge), a, b)
                if operand_unsigned(cx, a) && operand_unsigned(cx, b) =>
            {
                term_of(cx, a).zip(term_of(cx, b)).map(|(a, b)| Cond {
                    c: v,
                    op: *op,
                    a,
                    b,
                    signed: false,
                })
            }
            Rvalue::Binary(op @ (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge), a, b) => {
                // A signed comparison of a cast unsigned value with a constant 0 or more.
                let view = |op: &Operand| match op {
                    Operand::Const(Const::Int(c), t)
                        if t.is_signed() && crate::constfold::normalize(*c, *t) >= 0 =>
                    {
                        Some(Term::Const(crate::constfold::normalize(*c, *t)))
                    }
                    Operand::Copy(pl) if pl.proj.is_empty() => st
                        .casts
                        .iter()
                        .find(|(s, _)| *s == pl.local.0)
                        .map(|(_, u)| Term::Local(*u)),
                    _ => None,
                };
                match (view(a), view(b)) {
                    (Some(a), Some(b)) => Some(Cond {
                        c: v,
                        op: *op,
                        a,
                        b,
                        signed: true,
                    }),
                    _ => None,
                }
            }
            // A copy of a condition is that condition.
            Rvalue::Use(Operand::Copy(y)) if y.proj.is_empty() && y.local.0 != v => st
                .conds
                .iter()
                .find(|k| k.c == y.local.0)
                .map(|k| Cond { c: v, ..k.clone() }),
            _ => None,
        };
        kill(st, v);
        if let Some(c) = cond {
            if !mentions(&c.a, v) && !mentions(&c.b, v) {
                st.conds.push(c);
            }
        }
        return;
    }
    // A copy carries the facts of its source.
    if let Rvalue::Use(Operand::Copy(src)) = rv {
        if src.proj.is_empty() && src.local.0 != v && tracked[src.local.0 as usize] {
            let s = src.local.0;
            let sv = val_of_local(st, s);
            kill(st, v);
            let copied: Vec<Fact> = st
                .facts
                .iter()
                .filter_map(|f| {
                    if f.x == Some(s) {
                        Some(Fact { x: Some(v), ..*f })
                    } else if f.y == s {
                        Some(Fact { y: v, ..*f })
                    } else {
                        None
                    }
                })
                .collect();
            for f in copied {
                add_fact(st, f);
            }
            if sv.base != Some(s) {
                st.vals.insert(v, sv);
            }
            return;
        }
    }
    let new = eval(cx, st, rv, ty);
    kill(st, v);
    if let Some(n) = new {
        if n.base != Some(v) {
            st.vals.insert(v, n);
        }
    }
}

pub(super) fn operand_unsigned(cx: &Cx, op: &Operand) -> bool {
    match op {
        Operand::Const(_, ty) => unsigned(*ty),
        Operand::Copy(p) if !p.proj.is_empty() => cx.frozen_term(p).is_some(),
        Operand::Copy(p) => cx
            .func
            .locals
            .get(p.local.0 as usize)
            .is_some_and(|l| unsigned(l.ty)),
    }
}

/// The term of an unsigned operand.
pub(super) fn term_of(cx: &Cx, op: &Operand) -> Option<Term> {
    match op {
        Operand::Const(Const::Int(c), ty) if unsigned(*ty) => {
            Some(Term::Const(crate::constfold::normalize(*c, *ty)))
        }
        Operand::Copy(p) if p.proj.is_empty() => {
            let l = cx.func.locals.get(p.local.0 as usize)?;
            unsigned(l.ty).then_some(Term::Local(p.local.0))
        }
        Operand::Copy(p) => cx.frozen_term(p).map(Term::Local),
        _ => None,
    }
}

pub(super) fn val_of(cx: &Cx, st: &State, op: &Operand) -> Option<Val> {
    match op {
        // A constant is its value in its type (a VIR constant may be written out of range).
        Operand::Const(Const::Int(c), ty) if unsigned(*ty) => {
            let c = crate::constfold::normalize(*c, *ty);
            Some(Val {
                base: None,
                lo: c,
                hi: c,
            })
        }
        Operand::Copy(p) if p.proj.is_empty() => {
            let l = cx.func.locals.get(p.local.0 as usize)?;
            unsigned(l.ty).then(|| val_of_local(st, p.local.0))
        }
        Operand::Copy(p) => cx.frozen_term(p).map(|t| Val {
            base: Some(t),
            lo: 0,
            hi: 0,
        }),
        _ => None,
    }
}

pub(super) fn exact(lo: i128, hi: i128, ty: Ty) -> Option<Val> {
    (lo >= 0 && hi <= max_of(ty) && lo <= hi).then_some(Val { base: None, lo, hi })
}

/// The value an rvalue computes, or `None` (nothing known: the local becomes itself).
pub(super) fn eval(cx: &Cx, st: &State, rv: &Rvalue, ty: Ty) -> Option<Val> {
    if !unsigned(ty) {
        return None;
    }
    match rv {
        Rvalue::Use(op) => val_of(cx, st, op),
        Rvalue::Cast(op, _) => {
            // A widening of an unsigned value keeps it; anything else is unknown.
            let v = val_of(cx, st, op)?;
            let from = match op {
                Operand::Const(_, t) => *t,
                Operand::Copy(p) => cx.func.locals.get(p.local.0 as usize)?.ty,
            };
            // A cast to the same width changes nothing (`length as u64`); a widening keeps an
            // exact interval only (a base is a value of the narrower type).
            let same = unsigned(from) && max_of(from) == max_of(ty);
            let widens = unsigned(from) && max_of(from) < max_of(ty) && v.base.is_none();
            (same || widens).then_some(v)
        }
        Rvalue::Binary(op, a, b) => {
            let x = val_of(cx, st, a)?;
            let y = val_of(cx, st, b)?;
            match op {
                BinOp::Add => match (x.base, y.base) {
                    (None, None) => exact(x.lo + y.lo, x.hi + y.hi, ty),
                    (Some(_), None) if y.lo >= 0 && ty == Ty::U64 => Some(Val {
                        base: x.base,
                        lo: x.lo + y.lo,
                        hi: x.hi + y.hi,
                    }),
                    (None, Some(_)) if x.lo >= 0 && ty == Ty::U64 => Some(Val {
                        base: y.base,
                        lo: x.lo + y.lo,
                        hi: x.hi + y.hi,
                    }),
                    _ => None,
                },
                BinOp::Sub => match (x.base, y.base) {
                    (_, None) if y.lo == y.hi && ty == Ty::U64 && x.base.is_some() => Some(Val {
                        base: x.base,
                        lo: x.lo - y.lo,
                        hi: x.hi - y.lo,
                    }),
                    (None, None) if x.lo >= y.hi => exact(x.lo - y.hi, x.hi - y.lo, ty),
                    _ => None,
                },
                // Bounds below 2^64 can multiply past `i128`: both products checked.
                BinOp::Mul if x.base.is_none() && y.base.is_none() => {
                    exact(x.lo.checked_mul(y.lo)?, x.hi.checked_mul(y.hi)?, ty)
                }
                // A shift as a multiplication: `checked_shl` checks only the count, not bits
                // shifted into the sign.
                BinOp::Shl if x.base.is_none() && y.base.is_none() && y.hi < 64 => exact(
                    x.lo.checked_mul(1 << y.lo)?,
                    x.hi.checked_mul(1 << y.hi)?,
                    ty,
                ),
                BinOp::UShr | BinOp::Shr if x.base.is_none() && y.base.is_none() && y.hi < 64 => {
                    exact(x.lo >> y.hi, x.hi >> y.lo, ty)
                }
                BinOp::BitAnd => {
                    // `x & m` is at most `m` (and at most `x`).
                    let m = match (x.base, y.base) {
                        (_, None) => y.hi.min(if x.base.is_none() { x.hi } else { y.hi }),
                        (None, _) => x.hi,
                        _ => return None,
                    };
                    exact(0, m, ty)
                }
                BinOp::BitOr if x.base.is_none() && y.base.is_none() => {
                    exact(x.lo.max(y.lo), (x.hi + y.hi).min(max_of(ty)), ty)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The states along each successor edge of block `bi`, with what its branch condition tells.
pub(super) fn edge_states(cx: &Cx, tracked: &[bool], bi: usize, out: State) -> Vec<(usize, State)> {
    let term = &cx.func.blocks[bi].term;
    if let Terminator::Branch {
        cond: Operand::Copy(c),
        then,
        els,
    } = term
    {
        if c.proj.is_empty() {
            if let Some(cond) = out.conds.iter().find(|k| k.c == c.local.0).cloned() {
                let mut t = out.clone();
                assume(cx, tracked, &mut t, &cond, true);
                let mut e = out;
                assume(cx, tracked, &mut e, &cond, false);
                return vec![(then.0 as usize, t), (els.0 as usize, e)];
            }
        }
    }
    successors(term)
        .into_iter()
        .map(|s| (s.0 as usize, out.clone()))
        .collect()
}

/// `p + k <= q` from a condition holding (`truth`) or not.
pub(super) fn relation(cond: &Cond, truth: bool) -> (Term, i128, Term) {
    let (a, b) = (cond.a, cond.b);
    match (cond.op, truth) {
        (BinOp::Lt, true) | (BinOp::Ge, false) => (a, 1, b),
        (BinOp::Le, true) | (BinOp::Gt, false) => (a, 0, b),
        (BinOp::Gt, true) | (BinOp::Le, false) => (b, 1, a),
        _ => (b, 0, a), // Ge true, Lt false
    }
}

pub(super) fn assume(cx: &Cx, tracked: &[bool], st: &mut State, cond: &Cond, truth: bool) {
    let (p, k, q) = relation(cond, truth);
    // A signed `c + k <= s` with `c >= 0` makes `s` non-negative, so the unsigned value cast to
    // `s` is `s`: a lower bound. Nothing else follows (a negative `s` is a huge unsigned one).
    if cond.signed && !matches!(p, Term::Const(_)) {
        return;
    }
    let (pv, qv) = (val_of_term(st, p), val_of_term(st, q));
    let local = |t: Term| match t {
        Term::Local(l) if (l as usize) < cx.func.locals.len() && tracked[l as usize] => Some(l),
        _ => None,
    };
    // Intervals: a constant bound on a local without a base.
    if let (Some(l), None, None) = (local(p), pv.base, qv.base) {
        {
            let hi = pv.hi.min(qv.hi - k);
            st.vals.insert(
                l,
                Val {
                    base: None,
                    lo: pv.lo,
                    hi,
                },
            );
        }
    }
    if let (None, Some(l), None) = (pv.base, local(q), qv.base) {
        {
            let lo = qv.lo.max(pv.lo + k);
            st.vals.insert(
                l,
                Val {
                    base: None,
                    lo,
                    hi: qv.hi,
                },
            );
        }
    }
    // Facts: `p + k <= q` with `q` = `t + ql` (exact when `t >= -ql`).
    let Some(t) = qv.base else { return };
    let x = match (pv.base, &p) {
        (None, _) => None,
        (Some(b), Term::Local(l)) if b == *l && pv.lo == 0 && pv.hi == 0 => Some(b),
        _ => return,
    };
    let lower = if x.is_none() { pv.lo } else { 0 };
    // `q` exact: `t + ql >= 0`, known from `t`'s lower bounds.
    let ql = qv.lo;
    if qv.lo != qv.hi {
        return;
    }
    if ql < 0 && lower_bound(st, t) < -ql {
        return;
    }
    add_fact(
        st,
        Fact {
            x,
            k: k + lower - ql,
            y: t,
        },
    );
}

pub(super) fn decide(st: &State, cond: &Cond) -> Option<bool> {
    if cond.signed {
        return None;
    }
    let (a, b) = (val_of_term(st, cond.a), val_of_term(st, cond.b));
    let (lt_ab, le_ab, lt_ba, le_ba) = (
        prove_le(st, a, 1, b),
        prove_le(st, a, 0, b),
        prove_le(st, b, 1, a),
        prove_le(st, b, 0, a),
    );
    match cond.op {
        BinOp::Lt if lt_ab => Some(true),
        BinOp::Lt if le_ba => Some(false),
        BinOp::Le if le_ab => Some(true),
        BinOp::Le if lt_ba => Some(false),
        BinOp::Gt if lt_ba => Some(true),
        BinOp::Gt if le_ab => Some(false),
        BinOp::Ge if le_ba => Some(true),
        BinOp::Ge if lt_ab => Some(false),
        _ => None,
    }
}
