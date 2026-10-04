//! Read-only HIR inspection for tests: every expression / pattern of a function, and lookups.

use velt_sema::hir::{
    self, Block, Callee, Def, Expr, ExprKind as E, FnDef, Pat, PatKind, Program, StmtKind as S,
};

pub fn func<'p>(p: &'p Program, name: &str) -> &'p FnDef {
    p.defs
        .iter()
        .find_map(|d| match d {
            Def::Fn(f) if f.name == name => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no fn {name}"))
}

pub fn def_id(p: &Program, name: &str) -> hir::DefId {
    let i = p
        .defs
        .iter()
        .position(|d| match d {
            Def::Fn(f) => f.name == name,
            Def::Adt(a) => a.name == name,
            Def::Enum(e) => e.name == name,
            Def::Interface(i) => i.name == name,
            Def::Global(g) => g.name == name,
            Def::ExternFn(x) => x.name == name,
        })
        .unwrap_or_else(|| panic!("no def {name}"));
    hir::DefId(i as u32)
}

pub fn adt<'p>(p: &'p Program, name: &str) -> &'p hir::AdtDef {
    match p.def(def_id(p, name)) {
        Def::Adt(a) => a,
        _ => panic!("{name} is not an ADT"),
    }
}

/// Every expression in `f`'s body, pre-order.
pub fn exprs(f: &FnDef) -> Vec<&Expr> {
    let mut out = vec![];
    block(&f.body.block, &mut out, &mut vec![]);
    out
}

/// Every pattern node in `f`'s body.
pub fn pats(f: &FnDef) -> Vec<&Pat> {
    let mut ps = vec![];
    block(&f.body.block, &mut vec![], &mut ps);
    ps
}

/// Does `f`'s body contain a `while` loop (also inside expressions)?
pub fn has_while(f: &FnDef) -> bool {
    fn in_block(b: &Block) -> bool {
        b.stmts.iter().any(|s| match &s.kind {
            S::While { .. } => true,
            S::Block(b) => in_block(b),
            S::If { then, els, .. } => in_block(then) || els.as_ref().is_some_and(in_block),
            _ => false,
        })
    }
    in_block(&f.body.block)
        || exprs(f)
            .iter()
            .any(|e| matches!(&e.kind, E::Block(b) if in_block(b)))
}

fn block<'a>(b: &'a Block, out: &mut Vec<&'a Expr>, ps: &mut Vec<&'a Pat>) {
    for s in &b.stmts {
        match &s.kind {
            S::Let { init, .. } => init.iter().for_each(|e| expr(e, out, ps)),
            S::LetPat { pat: p, init } => {
                pat(p, ps);
                expr(init, out, ps);
            }
            S::Expr(e) => expr(e, out, ps),
            S::Return(e) => e.iter().for_each(|e| expr(e, out, ps)),
            S::If { cond, then, els } => {
                expr(cond, out, ps);
                block(then, out, ps);
                els.iter().for_each(|b| block(b, out, ps));
            }
            S::While {
                cond, body, step, ..
            } => {
                expr(cond, out, ps);
                block(body, out, ps);
                step.iter().for_each(|e| expr(e, out, ps));
            }
            S::ForOf {
                binding,
                iter,
                body,
                ..
            } => {
                pat(binding, ps);
                expr(iter, out, ps);
                block(body, out, ps);
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                block(body, out, ps);
                catch.iter().for_each(|(_, b)| block(b, out, ps));
                finally.iter().for_each(|b| block(b, out, ps));
            }
            S::Break(_) | S::Continue(_) => {}
            S::Block(b) => block(b, out, ps),
        }
    }
    b.value.iter().for_each(|e| expr(e, out, ps));
}

fn expr<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>, ps: &mut Vec<&'a Pat>) {
    out.push(e);
    let sub = |x: &'a Expr, out: &mut Vec<&'a Expr>, ps: &mut Vec<&'a Pat>| expr(x, out, ps);
    match &e.kind {
        E::Unary { expr: x, .. }
        | E::Cast(x)
        | E::Await(x)
        | E::WrapSome(x)
        | E::UnwrapSome(x, _)
        | E::UnwrapVariant { expr: x, .. }
        | E::Upcast(x)
        | E::Downcast(x)
        | E::ToDyn { expr: x, .. }
        | E::Throw(x)
        | E::Field { base: x, .. } => sub(x, out, ps),
        E::Binary { lhs, rhs, .. } | E::Logical { lhs, rhs, .. } => {
            sub(lhs, out, ps);
            sub(rhs, out, ps);
        }
        E::Assign { place, value } | E::CompoundAssign { place, value, .. } => {
            sub(place, out, ps);
            sub(value, out, ps);
        }
        E::Index { base, index, .. } => {
            sub(base, out, ps);
            sub(index, out, ps);
        }
        E::Call { callee, args } => {
            if let Callee::Indirect(c) = callee {
                sub(c, out, ps);
            }
            args.iter().for_each(|a| sub(a, out, ps));
        }
        E::If { cond, then, els } => {
            sub(cond, out, ps);
            sub(then, out, ps);
            sub(els, out, ps);
        }
        E::Block(b) => block(b, out, ps),
        E::AdtLit { fields: xs, .. }
        | E::Variant { args: xs, .. }
        | E::ArrayLit(xs)
        | E::Tuple(xs)
        | E::New { args: xs, .. } => xs.iter().for_each(|a| sub(a, out, ps)),
        E::Match { scrutinee, arms } => {
            sub(scrutinee, out, ps);
            for a in arms {
                pat(&a.pat, ps);
                a.guard.iter().for_each(|g| sub(g, out, ps));
                sub(&a.body, out, ps);
            }
        }
        E::Lit(_) | E::Local(..) | E::Global(_) | E::FnRef(..) | E::Closure(_) => {}
    }
}

fn pat<'a>(p: &'a Pat, ps: &mut Vec<&'a Pat>) {
    ps.push(p);
    match &p.kind {
        PatKind::Variant { args: xs, .. } | PatKind::Tuple(xs) | PatKind::Or(xs) => {
            xs.iter().for_each(|x| pat(x, ps))
        }
        PatKind::Array { elems, .. } => elems.iter().for_each(|x| pat(x, ps)),
        PatKind::Adt { fields } => fields.iter().for_each(|(_, x)| pat(x, ps)),
        PatKind::Some(x) => pat(x, ps),
        _ => {}
    }
}

/// Calls in `f` (callee, arg count).
pub fn calls(f: &FnDef) -> Vec<(&Callee, &[Expr])> {
    exprs(f)
        .into_iter()
        .filter_map(|e| match &e.kind {
            E::Call { callee, args } => Some((callee, args.as_slice())),
            _ => None,
        })
        .collect()
}

/// Uses of the local named `name` in `f`, as (use mode) in pre-order.
pub fn uses_of(f: &FnDef, name: &str) -> Vec<hir::UseMode> {
    exprs(f)
        .into_iter()
        .filter_map(|e| match e.kind {
            E::Local(l, m) if f.body.locals[l.0 as usize].name == name => Some(m),
            _ => None,
        })
        .collect()
}
