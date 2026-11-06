//! Values reachable from several threads at once: the argument of `shared(x)` (every thread
//! holding the `shared` reaches it). A function value there may be called from several threads
//! at once, and each call shares what the function captured
//! with the call (velt_vir async_fn/ctor.rs `take_capture`), so a captured resource without
//! `clone()` would have its count updated from several threads. Where the function is visible
//! here — a closure literal in the argument, or a `const` bound to one — that is an error;
//! otherwise the many-threads check where the value is made panics (velt_vir glue/many.rs).
//!
//! A function there runs each call on a copy of what it captured, so what a copy would break is
//! refused too, at any depth of the capture's type ([`Barred`]), for the closures visible here and
//! those `local_async` followed here; the strict walk of that check panics on the rest.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::defs::DefInfo;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, LocalDef, LocalId, PassMode, Stmt,
    StmtKind as S, TyId, TyKind,
};
use crate::known::MAX_LEVELS;
use crate::type_reach::Look;
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
    let mut checked = FxHashSet::default();
    for d in fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let bound = closure_consts(&mut f.body.block, &f.body.locals);
        let mut points: Vec<(Vec<DefId>, bool)> = vec![];
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            let E::Call { callee, args } = &e.kind else {
                return;
            };
            if let Callee::Intrinsic(Intrinsic::SharedNew) = callee {
                for a in args {
                    points.push((closures_in(a, &bound), fn_type(cx, a.ty)));
                }
            }
        });
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        let mut reported = FxHashSet::default();
        for (closures, is_fn) in points {
            for c in closures {
                if reported.insert(c) {
                    check_closure(cx, c, &Copied::Shared);
                }
                // Only a function in `shared(...)` runs each call on a copy: not one in a `Mutex`,
                // or held by an object in `shared(...)`.
                if is_fn && checked.insert(c) {
                    check_shared_captures(cx, c);
                }
            }
        }
    }
    // The closures `local_async` followed to a `shared(...)` from elsewhere.
    for c in std::mem::take(&mut cx.in_shared) {
        if checked.insert(c) {
            check_shared_captures(cx, c);
        }
    }
}

/// Is `t` a function type?
fn fn_type(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::FnPtr { .. })
}

/// Is `t` a `Mutex` or an `RwLock`, whose value one thread at a time has?
fn lock_type(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::Adt(d, _)
        if Some(*d) == cx.mutex_ty() || Some(*d) == cx.rwlock_ty())
}

impl Ctx<'_> {
    /// Does a deep copy of a `t` run its class's own `clone()`? Only for a class that owns a
    /// resource, its own or a part's, or holds a promise (velt_vir `Cx::own_clone`); any other class
    /// is copied field by field whatever its `clone()` does.
    fn runs_own_clone(&mut self, t: TyId) -> bool {
        let TyKind::Adt(d, _) = *self.ty.kind(t) else {
            return false;
        };
        if self.own_clone(d).is_none() {
            return false;
        }
        let mut look = |cx: &mut Self, t: TyId| match cx.ty.kind(t).clone() {
            TyKind::Shared(_) | TyKind::Dyn(..) | TyKind::FnPtr { .. } | TyKind::Closure(_) => {
                Look::Stop
            }
            TyKind::Promise(..) => Look::Found,
            TyKind::Adt(d, _) if matches!(&cx.info[d.0 as usize], DefInfo::Adt(a) if a.has_dispose) => {
                Look::Found
            }
            k => Look::Parts(cx.parts_of(k)),
        };
        self.find_within(t, MAX_LEVELS, &mut look).is_some()
    }
}

/// What a capture holds that a function in `shared(...)` can't have, each call running on a copy.
enum Barred {
    /// A lock held by value: the call would lock its own copy.
    Lock(TyId),
    /// An `Atomic` held by value: the call would change its own copy.
    Atomic(TyId),
    /// A class whose `clone()` may change its object, which would run on one object from several
    /// threads at once. `RegExp`'s only reads strings, whose counts are atomic (`FnDef::changes_held`
    /// says it stays so).
    Clone(TyId),
}

/// The first thing `t` holds that a function in `shared(...)` can't capture (`Barred`). What a
/// `shared` holds is one object for every thread; a function or interface value is checked by the
/// many-threads check at run time (velt_vir glue/many.rs).
fn barred(cx: &mut Ctx, t: TyId, seen: &mut FxHashSet<TyId>) -> Option<Barred> {
    if !seen.insert(t) {
        return None;
    }
    let k = cx.ty.kind(t).clone();
    match k {
        TyKind::Shared(_)
        | TyKind::Dyn(..)
        | TyKind::FnPtr { .. }
        | TyKind::Closure(_)
        | TyKind::Param(_) => return None,
        TyKind::Adt(d, _) => {
            if lock_type(cx, t) {
                return Some(Barred::Lock(t));
            }
            if Some(d) == cx.atomic_ty() {
                return Some(Barred::Atomic(t));
            }
            if cx.runs_own_clone(t) {
                let changes = cx.own_clone(d).is_some_and(|m| {
                    !cx.std_class(d, &["RegExp"])
                        || matches!(&cx.defs[m.0 as usize], Some(Def::Fn(f)) if f.changes_held)
                });
                if changes {
                    return Some(Barred::Clone(t));
                }
            }
        }
        _ => {}
    }
    let parts = cx.parts_of(k);
    parts.into_iter().find_map(|p| barred(cx, p, seen))
}

/// The captures that a function in `shared(...)` can't have (`Barred`).
fn check_shared_captures(cx: &mut Ctx, c: DefId) {
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return;
    };
    let span = f.span;
    let caps: Vec<(String, TyId)> = (f.captures.iter())
        .filter(|k| k.mode == PassMode::Owned)
        .map(|k| {
            let l = &f.body.locals[k.inner.0 as usize];
            (l.name.clone(), l.ty)
        })
        .collect();
    for (name, ty) in caps {
        let held = |cx: &Ctx, part: TyId| match part == ty {
            true => format!("`{name}`, a `{}` held by value", cx.display(part)),
            false => format!("`{name}`, which holds a `{}` by value", cx.display(part)),
        };
        let d = match barred(cx, ty, &mut FxHashSet::default()) {
            None => continue,
            Some(Barred::Lock(p)) => {
                let tn = cx.display(p);
                Diagnostic::error(
                    format!("this function captures {}, and it is in `shared(...)`, so each call would have a lock of its own", held(cx, p)),
                    span,
                )
                .with_note(format!("share the lock: `shared(new {tn}(…))`"))
            }
            Some(Barred::Atomic(p)) => {
                let tn = cx.display(p);
                Diagnostic::error(
                    format!("this function captures `{name}`, which holds an `{tn}` by value, and it is in `shared(...)`, so each call would change an `Atomic` of its own"),
                    span,
                )
                .with_note(format!("share the value: `const {name} = shared(...)` and `{name}.add(1)`, or hold the `{tn}` in a `shared` object"))
            }
            Some(Barred::Clone(p)) => {
                let tn = cx.display(p);
                Diagnostic::error(
                    format!("this function captures `{name}`, and it is in `shared(...)`, so each call would run `{tn}`'s `clone()` on the same object, from several threads at once"),
                    span,
                )
                .with_note(format!("share the object: `const {name} = shared(new Mutex(…))`, or put the function itself in a `Mutex`"))
            }
        };
        cx.error(d);
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
