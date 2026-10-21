//! What a body's values are derived from, for the rule that a part of a value read out of a
//! `shared` is never changed (#708): JavaScript would change the shared value, and Velt's
//! reader works on a copy or on memory other threads read. Instead of naming each way a part
//! can travel, everything a value may be or contain is followed: a binding, an assignment, a
//! loop element (also of a map's cursor loop and the iterator protocol, `Ctx::loop_roots`), a
//! branch of a conditional or `match`, an element of an array, tuple or object literal, an
//! argument of a constructor, and what a method returns (except the copies std's arrays make,
//! `slice()` and the like: copying out is explicit). A change to a value derived from a
//! parameter counts as a change of the parameter (`FnInfo::modified_params`); one to a value
//! derived from a `shared` read is an error (`finish::frozen_bindings`, `patch`).

use crate::ctx::Ctx;
use crate::hir::StmtKind as S;
use crate::hir::{Callee, Expr, ExprKind as E, FnDef, Intrinsic, LocalId, Pat, PatKind, Stmt};
use crate::visit::{self, VisitMut};

/// The roots of a value: the locals it may be a part of, and whether it may be a part of a
/// value read out of a `shared`.
#[derive(Default)]
pub(super) struct Roots {
    pub locals: Vec<LocalId>,
    pub frozen: bool,
    /// The bound local only holds the value (`obj.f = v`): only a change through it
    /// (`obj.f.x = 1`, `Evidence::deep`) changes what the value is part of, not one of its own
    /// fields.
    pub held: bool,
    /// The value's type (an edge of a value without references is dropped).
    pub ty: Option<crate::hir::TyId>,
}

/// The roots of the value `e` evaluates to.
pub(super) fn roots(cx: &Ctx, e: &Expr) -> Roots {
    let mut r = Roots::default();
    walk(cx, e, &mut r, 0);
    r
}

fn walk(cx: &Ctx, e: &Expr, r: &mut Roots, depth: u32) {
    if depth > 64 {
        return;
    }
    let go = |x: &Expr, r: &mut Roots| walk(cx, x, r, depth + 1);
    match &e.kind {
        E::Local(l, _) => r.locals.push(*l),
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::SharedRead),
            ..
        } => r.frozen = true,
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::Share),
            args,
        } => args.iter().for_each(|a| go(a, r)),
        E::Field { base, .. }
        | E::Index { base, .. }
        | E::UnwrapSome(base, _)
        | E::UnwrapVariant { expr: base, .. }
        | E::WrapSome(base)
        | E::Downcast(base)
        | E::Upcast(base) => go(base, r),
        E::If { then, els, .. } => {
            go(then, r);
            go(els, r);
        }
        E::Logical { lhs, rhs, .. } => {
            go(lhs, r);
            go(rhs, r);
        }
        E::Match { scrutinee, arms } => {
            go(scrutinee, r);
            arms.iter().for_each(|a| go(&a.body, r));
        }
        E::Block(b) => b.value.iter().for_each(|v| go(v, r)),
        E::ArrayLit(es) | E::Tuple(es) => es.iter().for_each(|x| go(x, r)),
        E::AdtLit { fields: es, .. } | E::Variant { args: es, .. } | E::New { args: es, .. } => {
            es.iter().for_each(|x| go(x, r))
        }
        E::Call {
            callee: Callee::Def(d, _),
            args,
        } if cx.fn_info(*d).this.is_some() && !copies(cx, *d) => {
            args.first().iter().for_each(|a| go(a, r))
        }
        E::Call {
            callee: Callee::Virtual { .. } | Callee::Dyn { .. },
            args,
        } => args.first().iter().for_each(|a| go(a, r)),
        _ => {}
    }
}

/// Is `d` a std array method that returns a new array of copies (`slice()`: copying out of a
/// `shared` value is explicit)?
fn copies(cx: &Ctx, d: crate::hir::DefId) -> bool {
    let name = &cx.fn_info(d).name;
    name.starts_with("std/")
        && [
            "slice",
            "concat",
            "map",
            "filter",
            "flat",
            "flatMap",
            "toSorted",
            "toReversed",
            "toSpliced",
            "with",
        ]
        .iter()
        .any(|m| name.ends_with(&format!(".{m}")))
}

/// Each local a statement or expression of `f`'s body binds or assigns, with the roots of what
/// it gets (loop elements included).
pub(super) fn edges(cx: &mut Ctx, f: &mut FnDef) -> Vec<(LocalId, Roots)> {
    let mut v = Edges { cx, out: vec![] };
    visit::block(&mut f.body.block, &mut v);
    let mut out = v.out;
    // A value without references holds no part of anything (a number), and a change to it
    // changes nothing it came from.
    out.retain(|(l, r)| {
        let local = &f.body.locals[l.0 as usize];
        // The iterator protocol's hidden locals change as the loop steps; its elements are
        // tied to the loop's source directly (`Ctx::loop_roots`).
        let stepping = ["<cursor", "<iterator", "<generator", "<result"];
        !stepping.iter().any(|h| local.name.starts_with(h))
            && !cx.is_copy(local.ty)
            && !r.ty.is_some_and(|t| cx.is_copy(t))
    });
    for (i, l) in f.body.locals.iter().enumerate() {
        if let Some(&root) = cx.loop_roots.get(&l.span) {
            out.push((
                LocalId(i as u32),
                Roots {
                    locals: vec![root],
                    ..Roots::default()
                },
            ));
        }
    }
    out
}

struct Edges<'a, 'm> {
    cx: &'a Ctx<'m>,
    out: Vec<(LocalId, Roots)>,
}

impl Edges<'_, '_> {
    fn bind(&mut self, bound: Vec<LocalId>, from: &Expr) {
        for l in bound {
            let mut r = roots(self.cx, from);
            r.ty = Some(from.ty);
            self.out.push((l, r));
        }
    }
}

impl VisitMut for Edges<'_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &s.kind {
            S::Let {
                local,
                init: Some(init),
            } => self.bind(vec![*local], init),
            S::LetPat { pat, init } => self.bind(pat_locals(pat), init),
            S::ForOf { binding, iter, .. } => self.bind(pat_locals(binding), iter),
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Assign { place, value } => match place.kind {
                E::Local(l, _) => self.bind(vec![l], value),
                // `this.i = i`: what the place is a part of now holds what the value is part
                // of, so a change to it may change that.
                _ => {
                    for l in roots(self.cx, place).locals {
                        let mut r = roots(self.cx, value);
                        r.held = true;
                        r.ty = Some(value.ty);
                        self.out.push((l, r));
                    }
                }
            },
            E::Match { scrutinee, arms } => {
                for a in arms {
                    self.bind(pat_locals(&a.pat), scrutinee);
                }
            }
            _ => {}
        }
    }
}

/// The locals pattern `p` binds.
pub(super) fn pat_locals(p: &Pat) -> Vec<LocalId> {
    fn go(p: &Pat, out: &mut Vec<LocalId>) {
        match &p.kind {
            PatKind::Binding(l, _) => out.push(*l),
            PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
                ps.iter().for_each(|x| go(x, out))
            }
            PatKind::Array { elems, rest } => {
                elems.iter().for_each(|x| go(x, out));
                out.extend(rest.iter().copied());
            }
            PatKind::Adt { fields } => fields.iter().for_each(|(_, x)| go(x, out)),
            PatKind::Some(x) => go(x, out),
            PatKind::Wildcard | PatKind::Lit(_) | PatKind::None | PatKind::InstanceOf(_) => {}
        }
    }
    let mut out = vec![];
    go(p, &mut out);
    out
}
