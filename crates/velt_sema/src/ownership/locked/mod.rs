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
mod opaque;
mod promises;
mod regions;
mod stores;
mod summary;
mod values;

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, FnDef, TyKind};
use crate::visit::{self, VisitMut};

/// Check and rewrite every `with` callback (module docs).
pub(crate) fn check_locked(cx: &mut Ctx) {
    let mut res = values::Resolver::default();
    let found = callbacks::find(cx, &mut res);
    opaque::check(cx, &found.opaque);
    let callbacks = found.callbacks;
    if callbacks.is_empty() && found.named.is_empty() {
        return;
    }
    let summaries = summary::Summaries::compute(cx);
    let mut seen_named = HashSet::new();
    for &(g, span) in &found.named {
        if seen_named.insert((g, span)) {
            named_callback(cx, &summaries, g, span);
        }
    }
    let mut done = HashSet::new();
    let mut reported = HashSet::new();
    for (c, direct) in callbacks {
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
        check_callback(cx, &summaries, &mut res, c, &mut reported);
    }
}

/// A named function passed to `with` (`m.with(fire)`), checked like `(v) => fire(v)`: it has
/// nothing captured to store into, but a promise it starts from the value, or returns, would
/// run after the lock is released.
fn named_callback(cx: &mut Ctx, s: &summary::Summaries, g: DefId, span: Span) {
    let name = cx.fn_info(g).name.clone();
    let ret = cx.fn_info(g).ret;
    let msg = if cx.holds_promise(ret) {
        let r = cx.display(ret);
        format!("the function passed to `with` returns `{r}`, which would run after the lock is released")
    } else if s.get(g).is_some_and(|s| s.promises & 1 != 0) {
        format!("`{name}` starts a promise with the locked value, which would run after `with` releases the lock")
    } else {
        return;
    };
    cx.error(Diagnostic::error(msg, span).with_note(AWAIT_OUTSIDE));
}

pub(super) const AWAIT_OUTSIDE: &str = "`with` holds the lock only while the function runs, and a promise made there keeps running without it; take what you need out of the value (`const x = m.with((v) => v.x)` gives a copy), await outside `with`, and store the result with another `with`";

fn async_callback_error(cx: &mut Ctx, c: DefId) {
    let span = cx.fn_info(c).span;
    cx.error(
        Diagnostic::error("the function passed to `with` cannot be async", span)
            .with_note("it runs while the lock is held; await before or after `with`"),
    );
}

/// Callback `c`, every closure made in it, and every closure a function value it calls (or
/// passes to a call) may be (`values`), recursively, taken out of `cx`; with the spans of the
/// function values resolved (to closures that are not async, and named functions) and the
/// named functions of each.
type Bodies = (
    Vec<(DefId, FnDef)>,
    HashSet<Span>,
    HashMap<Span, Vec<DefId>>,
);

fn take_bodies(cx: &mut Ctx, res: &mut values::Resolver, c: DefId) -> Bodies {
    let mut seen: Vec<DefId> = vec![];
    let mut resolved = HashSet::new();
    let mut named: HashMap<Span, Vec<DefId>> = HashMap::new();
    let mut todo = vec![c];
    while let Some(d) = todo.pop() {
        if seen.contains(&d) {
            continue;
        }
        seen.push(d);
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let mut found = FnValues {
            cx,
            closures: vec![],
            values: vec![],
        };
        visit::block(&mut f.body.block, &mut found);
        let FnValues {
            closures, values, ..
        } = found;
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        todo.extend(closures);
        for v in values {
            let Some(cs) = res.expr(cx, d, &v) else {
                continue;
            };
            let (closures, fns): (Vec<DefId>, Vec<DefId>) = cs
                .into_iter()
                .partition(|&n| cx.fn_info(n).kind == crate::defs::FnKind::Closure);
            if closures.iter().all(|&n| !cx.fn_info(n).is_async) {
                resolved.insert(v.span);
                todo.extend(closures);
                named.insert(v.span, fns);
            }
        }
    }
    let mut bodies = vec![];
    for d in seen {
        if let Some(Def::Fn(f)) = cx.defs[d.0 as usize].take() {
            bodies.push((d, f));
        }
    }
    (bodies, resolved, named)
}

/// The closures made in a body, and the function values it calls or passes to a call.
struct FnValues<'a, 'm> {
    cx: &'a mut Ctx<'m>,
    closures: Vec<DefId>,
    values: Vec<Expr>,
}

impl VisitMut for FnValues<'_, '_> {
    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Closure(n) => self.closures.push(*n),
            E::Call { callee, args } => {
                if let Callee::Indirect(c) = callee {
                    self.values.push((**c).clone());
                }
                for a in args {
                    let fn_typed = matches!(
                        self.cx.ty.kind(a.ty),
                        TyKind::FnPtr { .. } | TyKind::Closure(_)
                    );
                    if fn_typed && !matches!(a.kind, E::Closure(_)) {
                        self.values.push(a.clone());
                    }
                }
            }
            _ => {}
        }
    }
}

/// Report the promises callback `c` makes from the locked value and transfer what it stores
/// across the lock (module docs).
/// `reported`: spans already reported (a closure may be checked with several callbacks).
fn check_callback(
    cx: &mut Ctx,
    s: &summary::Summaries,
    res: &mut values::Resolver,
    c: DefId,
    reported: &mut HashSet<Span>,
) {
    let (mut bodies, resolved, named) = take_bodies(cx, res, c);
    let mut r = regions::Regions::new(c, &mut bodies);
    r.resolved = resolved;
    r.named = named;
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
        let span = match &m {
            promises::Made::Here(span, _)
            | promises::Made::ByCall(span, _)
            | promises::Made::Resource(span, _) => *span,
        };
        if !reported.insert(span) {
            continue;
        }
        promise_error(cx, m);
    }
    for u in unfixable {
        if !reported.insert(u.span) {
            continue;
        }
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
        promises::Made::Resource(span, ty) => {
            let (t, why) = (cx.display(ty), cx.uncopyable_why(ty));
            let part = cx.uncopyable_part(ty).unwrap_or(ty);
            let pn = cx.display(part);
            let msg = format!("the promise made here runs after `with` releases the lock, so it would need its own copy of this `{t}`, but {why}");
            let note = format!("give `{pn}` a `clone()` method that duplicates the resource, take what the promise needs out of the value (`m.with((v) => v.x)`) and start it after `with`, or keep the resource shared: `shared(new Mutex(…))`");
            cx.error(Diagnostic::error(msg, span).with_note(note));
            return;
        }
        promises::Made::ByCall(span, name) => (
            format!("`{name}` starts a promise with the locked value, which would run after `with` releases the lock"),
            span,
        ),
    };
    cx.error(Diagnostic::error(msg, span).with_note(AWAIT_OUTSIDE));
}

fn unfixable_error(cx: &mut Ctx, u: stores::Unfixable) {
    let (msg, note) = match u.kind {
        stores::Cross::Changed { inward } => {
            let what = match inward {
                true => "an outside object in the locked value",
                false => "a part of the locked value outside it",
            };
            (
                format!("this call may store {what}, and also changes that argument"),
                "the stored object would be shared by threads without the lock, and an argument the call changes cannot be passed as a copy; return what you need from `with` (it comes out as a copy), or pass a copy (`x.clone()`) of what the call only reads",
            )
        }
        stores::Cross::Opaque => (
            "this call passes the locked value to a function whose body is not visible here, together with something outside the lock".to_string(),
            "the function might store a part of one in the other, shared by threads without the lock; call a function or a closure written here (`const f = (s) => ...`), or pass a copy (`x.clone()`)",
        ),
        stores::Cross::Resource => (
            "this would copy an object that owns a resource without `clone()` across the `with` lock".to_string(),
            "a part of the locked value stored outside it must be copied, and so must one a promise made here keeps; give the resource type a `clone()` method, or use it inside the callback",
        ),
    };
    cx.error(Diagnostic::error(msg, u.span).with_note(note));
}
