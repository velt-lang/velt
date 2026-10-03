//! `m.with(f)` callbacks (docs/reference/async.md "Thread safety"). The lock protects the value
//! only while `f` runs, and the value's counts are not atomic, so nothing `f` does may leave a
//! reference to a part of the value outside the lock, or one to an outside object inside it
//! (#398). `with`'s result is transferred as it leaves the lock (velt_vir async_fn/sync.rs
//! `leave_lock`); this pass covers the rest:
//!
//! - **Promises** ([`promises`]). `f` runs synchronously, so a promise it makes runs after the
//!   lock is released — returned, stored or dropped — without the lock. One made from the
//!   locked value is an error, and so is a callback whose result holds a promise where the
//!   callback's body is not visible here (a function value of a concrete type; at a generic
//!   one lowering panics). Awaiting under the lock instead would need an asynchronous lock
//!   and a way to cancel a started promise together with its holder.
//! - **Stores** ([`stores`]). A value stored into the callback's captured state
//!   (`out.push(v.inner)`, `last = v.inner`), or an outside object stored into the value
//!   (`v.items.push(item)`), is transferred like a `spawn` argument: moved when it is the only
//!   reference, else copied. Stores within the value (`v.recent.push(v.byId.get(k)!)`) and
//!   within the outside state keep their identity.
//!
//! The callbacks are the closure literals passed to `with`, `const`s bound to one, and the
//! closures passed to a function parameter that reaches `with` (to a fixpoint). The standard
//! library's own callbacks keep identity through stores: they move values out of the lock
//! deliberately (std/prelude/promise.vlt `takeSettlement`).

mod promises;
mod regions;
mod stores;

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalDef, LocalId, Stmt,
    StmtKind as S, TyId, TyKind,
};
use crate::visit::{self, VisitMut};

/// Check and rewrite every `with` callback (module docs).
pub(crate) fn check_locked(cx: &mut Ctx) {
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    let mut found = Found::default();
    for &d in &fns {
        scan(cx, d, &mut found);
    }
    // Closures passed to parameters that reach `with`, until no new parameter is found.
    loop {
        let before = found.params.len();
        for &d in &fns {
            scan_param_calls(cx, d, &mut found);
        }
        if found.params.len() == before {
            break;
        }
    }
    let mut done = HashSet::new();
    for (c, direct) in found.callbacks {
        if !done.insert(c) {
            continue;
        }
        if cx.fn_info(c).is_async {
            // A literal passed to `with` itself is reported by `with_callback` (body/expr/sync.rs).
            if !direct {
                async_callback_error(cx, c);
            }
            continue;
        }
        check_callback(cx, c);
    }
}

#[derive(Default)]
struct Found {
    /// Closures `with` calls, and whether the literal is `with`'s argument itself.
    callbacks: Vec<(DefId, bool)>,
    /// `(function, parameter index)` of parameters passed to `with` as the callback.
    params: HashSet<(DefId, usize)>,
}

/// The `with` calls of function `d`: their callbacks, and the parameters passed as one.
fn scan(cx: &mut Ctx, d: DefId, found: &mut Found) {
    let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
        return;
    };
    let bound = closure_consts(&mut f.body.block, &f.body.locals);
    let mut calls: Vec<Expr> = vec![];
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
        if let E::Call {
            callee: Callee::Intrinsic(Intrinsic::MutexWith),
            args,
        } = &e.kind
        {
            if let [_, cb] = args.as_slice() {
                calls.push(cb.clone());
            }
        }
    });
    let params: Vec<LocalId> = f.params.iter().map(|p| p.local).collect();
    cx.defs[d.0 as usize] = Some(Def::Fn(f));
    for cb in calls {
        let closures = callback_closures(&cb, &bound);
        if closures.is_empty() {
            opaque_callback(cx, &cb);
        }
        let direct = matches!(cb.kind, E::Closure(_));
        found
            .callbacks
            .extend(closures.into_iter().map(|c| (c, direct)));
        if let E::Local(l, _) = cb.kind {
            if let Some(i) = params.iter().position(|p| *p == l) {
                found.params.insert((d, i));
            }
        }
    }
}

/// A callback whose body is not visible here: a result holding a promise was made from the
/// locked value it was given. (A generic result is checked by lowering, per instantiation.)
fn opaque_callback(cx: &mut Ctx, cb: &Expr) {
    let TyKind::FnPtr { ret, .. } = cx.ty.kind(cb.ty).clone() else {
        return;
    };
    if cx.mentions_params(ret) || !cx.holds_promise(ret) {
        return;
    }
    let r = cx.display(ret);
    cx.error(
        Diagnostic::error(
            format!("the function passed to `with` returns `{r}`, which would run after the lock is released"),
            cb.span,
        )
        .with_note(AWAIT_OUTSIDE),
    );
}

const AWAIT_OUTSIDE: &str = "`with` holds the lock only while the function runs, and a promise made there keeps running without it; take what you need out of the value (`const x = m.with((v) => v.x)` gives a copy), await outside `with`, and store the result with another `with`";

fn async_callback_error(cx: &mut Ctx, c: DefId) {
    let span = cx.fn_info(c).span;
    cx.error(
        Diagnostic::error("the function passed to `with` cannot be async", span)
            .with_note("it runs while the lock is held; await before or after `with`"),
    );
}

/// Calls of `(function, parameter)` pairs in `found.params` from function `d`: the closures
/// they pass are callbacks, and a parameter of `d` they pass reaches `with` too.
fn scan_param_calls(cx: &mut Ctx, d: DefId, found: &mut Found) {
    let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
        return;
    };
    let bound = closure_consts(&mut f.body.block, &f.body.locals);
    let params: Vec<LocalId> = f.params.iter().map(|p| p.local).collect();
    let mut passed: Vec<Expr> = vec![];
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
        if let E::Call {
            callee: Callee::Def(g, _),
            args,
        } = &e.kind
        {
            for (i, a) in args.iter().enumerate() {
                if found.params.contains(&(*g, i)) {
                    passed.push(a.clone());
                }
            }
        }
    });
    cx.defs[d.0 as usize] = Some(Def::Fn(f));
    for a in passed {
        let closures = callback_closures(&a, &bound);
        found
            .callbacks
            .extend(closures.into_iter().map(|c| (c, false)));
        if let E::Local(l, _) = a.kind {
            if let Some(i) = params.iter().position(|p| *p == l) {
                found.params.insert((d, i));
            }
        }
    }
}

/// The closures a callback argument is: a literal, or a `const` bound to one.
fn callback_closures(cb: &Expr, bound: &HashMap<LocalId, DefId>) -> Vec<DefId> {
    match &cb.kind {
        E::Closure(c) => vec![*c],
        E::Local(l, _) => bound.get(l).copied().into_iter().collect(),
        _ => vec![],
    }
}

/// `const` locals initialized with a closure literal.
fn closure_consts(b: &mut crate::hir::Block, locals: &[LocalDef]) -> HashMap<LocalId, DefId> {
    struct Lets<'a>(&'a [LocalDef], HashMap<LocalId, DefId>);
    impl VisitMut for Lets<'_> {
        fn stmt(&mut self, s: &mut Stmt) {
            if let S::Let {
                local,
                init: Some(init),
            } = &s.kind
            {
                if let (E::Closure(c), false) = (&init.kind, self.0[local.0 as usize].mutable) {
                    self.1.insert(*local, *c);
                }
            }
        }
    }
    let mut v = Lets(locals, HashMap::new());
    visit::block(b, &mut v);
    v.1
}

/// Callback `c` and every closure created in it (recursively).
fn locked_bodies(cx: &Ctx, c: DefId) -> Vec<DefId> {
    let mut out = vec![c];
    let mut i = 0;
    while i < out.len() {
        if let Some(Def::Fn(f)) = &cx.defs[out[i].0 as usize] {
            let mut block = f.body.block.clone();
            visit::exprs_mut(&mut block, &mut |e: &mut Expr| {
                if let E::Closure(n) = e.kind {
                    if !out.contains(&n) {
                        out.push(n);
                    }
                }
            });
        }
        i += 1;
    }
    out
}

/// Report the promises callback `c` makes from the locked value and transfer what it stores
/// across the lock (module docs).
fn check_callback(cx: &mut Ctx, c: DefId) {
    let mut bodies: Vec<(DefId, FnDef)> = vec![];
    for d in locked_bodies(cx, c) {
        if let Some(Def::Fn(f)) = cx.defs[d.0 as usize].take() {
            bodies.push((d, f));
        }
    }
    let mut r = regions::Regions::new(c, &bodies);
    loop {
        r.changed = false;
        for (d, f) in bodies.iter_mut() {
            stores::visit(cx, &mut r, *d, f, false);
        }
        if !r.changed {
            break;
        }
    }
    let mut made = vec![];
    for (d, f) in bodies.iter_mut() {
        // A closure stored for later, or an async one, runs its body after the lock is gone.
        let info = cx.fn_info(*d);
        if *d == c || !(info.escaping || info.is_async) {
            made.extend(promises::made_from_value(cx, &r, *d, f));
        }
    }
    if !cx.scopes[cx.fn_info(c).module].is_std {
        for (d, f) in bodies.iter_mut() {
            stores::visit(cx, &mut r, *d, f, true);
        }
    }
    for (d, f) in bodies {
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
    }
    for (span, ty) in made {
        promise_error(cx, span, ty);
    }
}

fn promise_error(cx: &mut Ctx, span: Span, ty: TyId) {
    let t = cx.display(ty);
    cx.error(
        Diagnostic::error(
            format!(
                "this `{t}` uses the locked value, and would run after `with` releases the lock"
            ),
            span,
        )
        .with_note(AWAIT_OUTSIDE),
    );
}
