//! Drop glue that releases one counted pointer: the out-of-line release of a class object (or
//! a nullable one), which lowering calls with the address of the local holding it,
//! `_Gdrop_Source(&src)` for `const src = this.plain[k]`.
//!
//! Its body is the inline release (`lower/rc.rs`) of the pointer in its one parameter's slot:
//!
//! ```text
//! bb0: x = *p0; c = ne x, null; branch c, A, R      (or `eq x, null` with the targets swapped)
//! A:   q = ptradd x, -8; n = *q; t = eq n, 1; branch t, Last, Dec
//! Dec: m = sub n, 1; *q = m; goto R
//! R:   return ()
//! ```
//!
//! `Last` (the count was 1) frees whatever it must. A call of such glue is a release of the
//! family member whose address it is given.

use velt_vir::vir::{BinOp, Const, Function, Local, Operand, Proj, Rvalue, Stmt, Terminator, Ty};

/// Whether `func` is drop glue of the shape above.
pub(super) fn release_glue(func: &Function) -> bool {
    if func.params != [Ty::Ptr] || func.ret != Ty::Unit || !func.symbol.starts_with("_Gdrop_") {
        return false;
    }
    let block = |i: usize| func.blocks.get(i);
    let Some(b0) = block(0) else { return false };
    let stmts: Vec<&Stmt> = b0
        .stmts
        .iter()
        .filter(|s| !matches!(s, Stmt::Nop))
        .collect();
    let [Stmt::Assign(xd, Rvalue::Use(Operand::Copy(slot))), Stmt::Assign(cd, test)] = &stmts[..]
    else {
        return false;
    };
    if !xd.proj.is_empty() || slot.local != Local(0) || slot.proj != [Proj::Deref(Ty::Ptr)] {
        return false;
    }
    let x = xd.local;
    let Terminator::Branch {
        cond: Operand::Copy(c),
        then,
        els,
    } = &b0.term
    else {
        return false;
    };
    let (a, r) = match test {
        Rvalue::Binary(BinOp::Ne, l, z) if is_local(l, x) && is_null(z) => (*then, *els),
        Rvalue::Binary(BinOp::Eq, l, z) if is_local(l, x) && is_null(z) => (*els, *then),
        _ => return false,
    };
    if !c.proj.is_empty() || c.local != cd.local || !cd.proj.is_empty() {
        return false;
    }
    let returns = |b: usize| {
        block(b).is_some_and(|b| {
            b.stmts.iter().all(|s| matches!(s, Stmt::Nop))
                && matches!(b.term, Terminator::Return(Operand::Const(Const::Unit, _)))
        })
    };
    if !returns(r.0 as usize) {
        return false;
    }
    let Some(ba) = block(a.0 as usize) else {
        return false;
    };
    let stmts: Vec<&Stmt> = ba
        .stmts
        .iter()
        .filter(|s| !matches!(s, Stmt::Nop))
        .collect();
    let [Stmt::Assign(qd, Rvalue::Binary(BinOp::PtrAdd, base, off)), Stmt::Assign(nd, Rvalue::Use(Operand::Copy(cw))), Stmt::Assign(td, Rvalue::Binary(BinOp::Eq, n, one))] =
        &stmts[..]
    else {
        return false;
    };
    let q = qd.local;
    let count_ok = is_local(base, x)
        && matches!(off, Operand::Const(Const::Int(-8), _))
        && cw.local == q
        && cw.proj == [Proj::Deref(Ty::U64)]
        && is_local(n, nd.local)
        && matches!(one, Operand::Const(Const::Int(1), _));
    let Terminator::Branch {
        cond: Operand::Copy(t),
        els: dec,
        ..
    } = &ba.term
    else {
        return false;
    };
    if !count_ok || t.local != td.local || !t.proj.is_empty() {
        return false;
    }
    let Some(bd) = block(dec.0 as usize) else {
        return false;
    };
    let stmts: Vec<&Stmt> = bd
        .stmts
        .iter()
        .filter(|s| !matches!(s, Stmt::Nop))
        .collect();
    let dec_ok = match &stmts[..] {
        [Stmt::Assign(md, Rvalue::Binary(BinOp::Sub, l, o)), Stmt::Assign(st, Rvalue::Use(v))] => {
            is_local(l, nd.local)
                && matches!(o, Operand::Const(Const::Int(1), _))
                && st.local == q
                && st.proj == [Proj::Deref(Ty::U64)]
                && is_local(v, md.local)
        }
        _ => false,
    };
    dec_ok && matches!(bd.term, Terminator::Goto(g) if g == r)
}

fn is_local(op: &Operand, l: Local) -> bool {
    matches!(op, Operand::Copy(p) if p.proj.is_empty() && p.local == l)
}

fn is_null(op: &Operand) -> bool {
    matches!(op, Operand::Const(Const::Int(0), _))
}
