//! Values reachable from several threads at once: the argument of `shared(x)` (every thread
//! holding the `shared` reaches it). A function value there may be called from several threads
//! at once, and each call shares what the function captured
//! with the call (velt_vir async_fn/ctor.rs `take_capture`), so a captured resource without
//! `clone()` would have its count updated from several threads. Where the function is visible
//! here — a closure literal in the argument, or a `const` bound to one — that is an error;
//! otherwise the many-threads check where the value is made panics (velt_vir glue/many.rs).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, LocalDef, LocalId, PassMode, Stmt,
    StmtKind as S, TyId,
};
use crate::visit::{self, VisitMut};

/// Report closures reachable from several threads at once that capture a resource without
/// `clone()` (module docs).
pub(crate) fn check_many_threads(cx: &mut Ctx) {
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    for d in fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let bound = closure_consts(&mut f.body.block, &f.body.locals);
        let mut points: Vec<Vec<DefId>> = vec![];
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            let E::Call { callee, args } = &e.kind else {
                return;
            };
            if let Callee::Intrinsic(Intrinsic::SharedNew) = callee {
                for a in args {
                    points.push(closures_in(a, &bound));
                }
            }
        });
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        let mut reported = FxHashSet::default();
        for closures in points {
            for c in closures {
                if reported.insert(c) {
                    check_closure(cx, c, &Copied::Shared);
                }
            }
        }
    }
}

/// `const` locals initialized with a closure literal or a method value (`const h = o.handle`).
fn closure_consts(b: &mut crate::hir::Block, locals: &[LocalDef]) -> FxHashMap<LocalId, DefId> {
    struct Lets<'a>(&'a [LocalDef], FxHashMap<LocalId, DefId>);
    impl VisitMut for Lets<'_> {
        fn stmt(&mut self, s: &mut Stmt) {
            if let S::Let {
                local,
                init: Some(init),
            } = &s.kind
            {
                if let (Some(c), false) = (
                    crate::body::closure_def(init),
                    self.0[local.0 as usize].mutable,
                ) {
                    self.1.insert(*local, c);
                }
            }
        }
    }
    let mut v = Lets(locals, FxHashMap::default());
    visit::block(b, &mut v);
    v.1
}

/// The closures `e` makes reachable that are visible here: literals in it and `const`s bound
/// to one.
fn closures_in(e: &Expr, bound: &FxHashMap<LocalId, DefId>) -> Vec<DefId> {
    let mut e = e.clone();
    let mut out = vec![];
    visit::expr(&mut e, &mut Found(bound, &mut out));
    out
}

struct Found<'a, 'b>(&'a FxHashMap<LocalId, DefId>, &'b mut Vec<DefId>);

impl VisitMut for Found<'_, '_> {
    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Closure(c) => self.1.push(*c),
            E::Local(l, _) => {
                if let Some(c) = self.0.get(l) {
                    self.1.push(*c);
                }
            }
            _ => {}
        }
    }
}

/// Where a function value is copied for other threads.
pub(crate) enum Copied {
    /// It is in `shared(...)`: every thread holding it calls it.
    Shared,
    /// It reaches another thread inside the standard library, which runs a copy of it on each
    /// (a server's handler): `label` names the user's call it leaves through (`local_async`).
    Crossing(Span, String),
}

/// Report the captures of closure `c` that each copy would duplicate although they own a
/// resource without `clone()`.
pub(crate) fn check_closure(cx: &mut Ctx, c: DefId, copied: &Copied) {
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return;
    };
    let span = f.span;
    let caps: Vec<(String, TyId)> = f
        .captures
        .iter()
        .filter(|k| k.mode == PassMode::Owned)
        .map(|k| {
            let l = &f.body.locals[k.inner.0 as usize];
            (l.name.clone(), l.ty)
        })
        .collect();
    for (name, ty) in caps {
        if cx.owns_uncopyable(ty) {
            report(cx, span, &name, ty, copied);
        }
    }
}

fn report(cx: &mut Ctx, span: Span, name: &str, ty: TyId, copied: &Copied) {
    let what = cx.uncopyable_why(ty);
    let part = cx.uncopyable_part(ty).unwrap_or(ty);
    let pn = cx.display(part);
    let mut d = match copied {
        Copied::Shared => Diagnostic::error(
            format!("this function captures `{name}`, and it is in `shared(...)`, so several threads may call it at once: each call would share `{name}` across threads, but {what}"),
            span,
        ),
        Copied::Crossing(at, label) => Diagnostic::error(
            format!("this function captures `{name}`, and it runs on other threads, each with a copy of it: each copy would copy `{name}`, but {what}"),
            span,
        )
        .with_label(*at, label.clone()),
    };
    let each = match copied {
        Copied::Shared => "each call then gets its own copy",
        Copied::Crossing(..) => "each copy then gets its own",
    };
    d = d.with_note(format!(
        "give `{pn}` a `clone()` method that duplicates the resource ({each}), or capture a shared one: `shared(new Mutex(…))`"
    ));
    cx.error(d);
}
