//! A view over a captured buffer may be held only directly in a local of the function. Anywhere else
//! (handed out by a closure or generator, stored in a container, an object's field, an `any` or a
//! nullable) it is reported as a change, whether or not anything writes through it: following it
//! through all of those isn't worth what a rare false refusal costs. A read-only view in a local, and
//! a view over a copy, are fine. A write through the local is found by [`super::changes`].

use rustc_hash::FxHashSet;

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, Stmt, StmtKind as S, TyId, TyKind,
};
use crate::ownership::local_closures::walk::{self, Visit};

use super::changes::{in_library, Found, Memo, Roots};
use super::reach::{is_buffer, is_view, reached_locals, reaches};

/// Add to `out` the views over what `taint` reaches that body `f` holds outside a local (module docs).
pub(super) fn held_views(
    cx: &mut Ctx,
    f: &FnDef,
    taint: &Roots,
    memo: &mut Memo,
    out: &mut Vec<Found>,
) {
    let mut seen = FxHashSet::default();
    scan(cx, f, taint, memo, &mut seen, out);
}

fn scan(
    cx: &mut Ctx,
    f: &FnDef,
    taint: &Roots,
    memo: &mut Memo,
    seen: &mut FxHashSet<DefId>,
    out: &mut Vec<Found>,
) {
    let mut v = Views {
        cx: &mut *cx,
        taint,
        memo: &mut *memo,
        out: &mut *out,
        args: FxHashSet::default(),
        declared: vec![],
    };
    walk::block(&f.body.block, &mut v);
    if let Some(x) = &f.body.block.value {
        v.store(x);
    }
    // A function declared in the body has no `E::Closure` for `changes` to follow.
    let declared = v.declared;
    for d in declared {
        if !seen.insert(d) {
            continue;
        }
        let Some(Def::Fn(k)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let roots: Roots = (k.captures.iter())
            .filter_map(|c| taint.get(&c.outer).map(|&r| (c.inner, r)))
            .collect();
        if !roots.is_empty() {
            let inner = reached_locals(cx, &k, &roots, memo);
            scan(cx, &k, &inner, memo, seen, out);
        }
        cx.defs[d.0 as usize] = Some(Def::Fn(k));
    }
}

struct Views<'a, 'm> {
    cx: &'a mut Ctx<'m>,
    taint: &'a Roots,
    memo: &'a mut Memo,
    out: &'a mut Vec<Found>,
    /// Arguments of calls, with their conversions: a call may read a view it is given.
    args: FxHashSet<*const Expr>,
    declared: Vec<DefId>,
}

impl Views<'_, '_> {
    /// `x` goes somewhere other than a local: report it if it's a view over what is reached, or a
    /// buffer that is, which a view can be made over there.
    fn store(&mut self, x: &Expr) {
        if !is_view(self.cx, x.ty) && !is_buffer(self.cx, x.ty) {
            return;
        }
        // The capture itself isn't a view made here.
        let root =
            matches!(x.kind, E::Local(l, _) if self.taint.get(&l).is_some_and(|r| r.0.local == l));
        if root {
            return;
        }
        let Some((r, true)) = reaches(self.cx, x, self.taint, self.memo) else {
            return;
        };
        self.report(r.local, x.span);
    }

    fn report(&mut self, root: crate::hir::LocalId, at: Span) {
        if !self.out.iter().any(|y| y.0 == root) {
            let via = "a view over it, held outside a local".to_string();
            self.out.push((root, at, "…".to_string(), Some(via), None));
        }
    }

    /// `a`, an argument, with the conversions around it.
    fn allow(&mut self, a: &Expr) {
        self.args.insert(a as *const Expr);
        match &a.kind {
            E::WrapSome(x) | E::Upcast(x) | E::ToDyn { expr: x, .. } => self.allow(x),
            E::Variant { args, .. } => args.iter().for_each(|x| self.allow(x)),
            _ => {}
        }
    }

    /// Does a library's or the compiler's container of this type keep what it's given?
    fn keeps_views(&self, t: TyId) -> bool {
        let open = |t: TyId| is_view(self.cx, t) || matches!(self.cx.ty.kind(t), TyKind::Dyn(..));
        match self.cx.ty.kind(t) {
            TyKind::Array(e) => open(*e),
            TyKind::Adt(_, args) if !is_view(self.cx, t) => args.iter().any(|a| open(*a)),
            _ => false,
        }
    }
}

impl Visit for Views<'_, '_> {
    fn stmt(&mut self, s: &Stmt) {
        if let S::Return(Some(x)) = &s.kind {
            self.store(x);
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::ArrayLit(xs) | E::Tuple(xs) | E::AdtLit { fields: xs, .. } => {
                xs.iter().for_each(|x| self.store(x));
            }
            E::Variant { args: xs, .. } if !self.args.contains(&(e as *const Expr)) => {
                xs.iter().for_each(|x| self.store(x));
            }
            E::WrapSome(x) | E::Upcast(x) | E::ToDyn { expr: x, .. }
                if !self.args.contains(&(e as *const Expr)) =>
            {
                self.store(x);
            }
            E::Assign { place, value } if !matches!(place.kind, E::Local(..)) => self.store(value),
            E::New { args, .. } => args.iter().for_each(|a| self.allow(a)),
            E::Call { callee, args } => {
                let library = match callee {
                    Callee::Intrinsic(Intrinsic::Yield) => {
                        args.iter().for_each(|a| self.store(a));
                        false
                    }
                    Callee::Intrinsic(_) => true,
                    Callee::Def(d, _) => {
                        let nested = matches!(&self.cx.defs[d.0 as usize], Some(Def::Fn(k)) if !k.captures.is_empty());
                        if nested {
                            self.declared.push(*d);
                        }
                        in_library(self.cx, *d)
                    }
                    _ => false,
                };
                if library && args.first().is_some_and(|r| self.keeps_views(r.ty)) {
                    args.iter().skip(1).for_each(|a| self.store(a));
                }
                args.iter().for_each(|a| self.allow(a));
            }
            _ => {}
        }
    }
}
