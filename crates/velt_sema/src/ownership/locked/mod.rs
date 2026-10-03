//! `m.with(f)` callbacks (docs/reference/async.md "Thread safety"). The lock protects the value
//! only while `f` runs, and the value's counts are not atomic, so nothing `f` does may leave a
//! reference to a part of the value outside the lock, or one to an outside object inside it
//! (#398). `with`'s result is transferred as it leaves the lock (velt_vir async_fn/sync.rs
//! `leave_lock`); this pass covers the rest:
//!
//! - **Promises** ([`promises`]). `f` runs synchronously, so a promise it makes runs after the
//!   lock is released — returned, stored or dropped — without the lock. One made from the
//!   locked value is an error (also by a function the callback calls, `super::summary`), and
//!   so is a callback whose result holds a promise where the callback's body is not visible
//!   (`callbacks`). Awaiting under the lock instead would need an asynchronous lock and a way
//!   to cancel a started promise together with its holder.
//! - **Stores** ([`stores`]). A value stored into the callback's captured state
//!   (`out.push(v.inner)`, `last = v.inner`), or an outside object stored into the value
//!   (`v.items.push(item)`), is transferred like a `spawn` argument: moved when it is the only
//!   reference, else copied — also when a function the callback calls stores it
//!   (`super::summary`). Stores within the value (`v.recent.push(v.byId.get(k)!)`) and within
//!   the outside state keep their identity. A call that stores a part of an argument it also
//!   modifies (`v.addTo(out)`) cannot copy it, and is an error.
//!
//! The callbacks are found by [`callbacks`]. The standard
//! library's own callbacks keep identity through stores: they move values out of the lock
//! deliberately (std/prelude/promise.vlt `takeSettlement`).

mod callbacks;
mod promises;
mod regions;
mod stores;
mod summary;

use std::collections::HashSet;

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::hir::{Def, DefId, Expr, FnDef, TyKind};
use crate::visit::{self, VisitMut};

/// Check and rewrite every `with` callback (module docs).
pub(crate) fn check_locked(cx: &mut Ctx) {
    let found = callbacks::find(cx);
    let mut reported = HashSet::new();
    for cb in &found.opaque {
        if reported.insert(cb.span) {
            opaque_callback(cx, cb);
        }
    }
    if found.callbacks.is_empty() {
        return;
    }
    let summaries = summary::Summaries::compute(cx);
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
        check_callback(cx, &summaries, c);
    }
}

/// A callback whose body is not visible: a result holding a promise may have been made from
/// the locked value it was given. (A result type that still depends on a type parameter is
/// not checked: a known gap.)
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

/// Callback `c` and every closure created in it (recursively), taken out of `cx`.
fn take_bodies(cx: &mut Ctx, c: DefId) -> Vec<(DefId, FnDef)> {
    struct Closures(Vec<DefId>);
    impl VisitMut for Closures {
        fn expr(&mut self, e: &mut Expr) {
            if let crate::hir::ExprKind::Closure(n) = e.kind {
                self.0.push(n);
            }
        }
    }
    let mut out: Vec<(DefId, FnDef)> = vec![];
    let mut todo = vec![c];
    while let Some(d) = todo.pop() {
        if out.iter().any(|(x, _)| *x == d) {
            continue;
        }
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let mut found = Closures(vec![]);
        visit::block(&mut f.body.block, &mut found);
        todo.extend(found.0);
        out.push((d, f));
    }
    out
}

/// Report the promises callback `c` makes from the locked value and transfer what it stores
/// across the lock (module docs).
fn check_callback(cx: &mut Ctx, s: &summary::Summaries, c: DefId) {
    let mut bodies = take_bodies(cx, c);
    let mut r = regions::Regions::new(c, &mut bodies);
    loop {
        r.changed = false;
        for (d, f) in bodies.iter_mut() {
            stores::visit(cx, &mut r, s, *d, f, false);
        }
        if !r.changed {
            break;
        }
    }
    let std = cx.scopes[cx.fn_info(c).module].is_std;
    let mut made = vec![];
    for (d, f) in bodies.iter_mut() {
        // An async closure's body runs when it is called (that call makes the promise).
        if *d == c || !cx.fn_info(*d).is_async {
            made.extend(promises::made_from_value(cx, &r, s, *d, f, !std));
        }
    }
    let mut unfixable = vec![];
    if !std {
        for (d, f) in bodies.iter_mut() {
            unfixable.extend(stores::visit(cx, &mut r, s, *d, f, true));
        }
    }
    for (d, f) in bodies {
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
    }
    for m in made {
        promise_error(cx, m);
    }
    for u in unfixable {
        unfixable_error(cx, u);
    }
}

fn promise_error(cx: &mut Ctx, m: promises::Made) {
    let (msg, span) = match m {
        promises::Made::Here(span, ty) => {
            let t = cx.display(ty);
            let msg = format!(
                "this `{t}` uses the locked value, and would run after `with` releases the lock"
            );
            (msg, span)
        }
        promises::Made::ByCall(span, name) => (
            format!("`{name}` starts a promise with the locked value, which would run after `with` releases the lock"),
            span,
        ),
    };
    cx.error(Diagnostic::error(msg, span).with_note(AWAIT_OUTSIDE));
}

fn unfixable_error(cx: &mut Ctx, u: stores::Unfixable) {
    let what = match u.inward {
        true => "an outside object in the locked value",
        false => "a part of the locked value outside it",
    };
    cx.error(
        Diagnostic::error(
            format!("this call may store {what}, and also changes that argument"),
            u.span,
        )
        .with_note("the stored object would be shared by threads without the lock, and an argument the call changes cannot be passed as a copy; return what you need from `with` (it comes out as a copy), or pass a copy (`x.clone()`) of what the call only reads"),
    );
}
