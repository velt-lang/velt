//! Thread boundaries (docs/reference/async.md "Tasks"): a value that enters a spawned task while
//! the program still uses it is deep-copied for the task, and a deep copy of a resource calls
//! the type's own `clone()` (velt_vir `transfer.rs`). A value owning a `[Symbol.dispose]`
//! resource without one cannot be copied, so passing it to `spawn` while it is still in use is
//! an error here, where it is visible: an argument of `spawn(f(…))` that became a share
//! (`Intrinsic::Share`, the variable is used again) and a shared capture of a spawned async
//! closure literal (or of one called at once). A value sent on a channel crosses the same way
//! (`ch.send(x)`, `ch.trySend(x)`: a function that hands its parameter to
//! `Intrinsic::ChanSend` / `ChanTrySend`), so the same rule applies to the value it is given.
//! A share inside an object, array, tuple or union literal built for the crossing is copied
//! too. Values that only turn out to be shared at run time (another reference made
//! earlier) are checked by the transfer itself, which panics instead of releasing the
//! resource twice.

use std::collections::HashMap;

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, LocalDef, PassMode, TyId, TyKind,
};
use crate::visit;

use super::validate::place_text;

/// Where a value crosses to another task.
#[derive(Clone, Copy)]
enum Site {
    /// The operand of `spawn`.
    Spawn,
    /// The value given to a channel's `send` / `trySend`, or to a function that passes it on
    /// to one (the function called).
    Send(DefId),
}

/// Why `spawn` (or a channel send) copies a value.
enum Copied {
    /// The variable (its name) is used again after the `spawn`.
    UsedAgain(String),
    /// It stays where it is held (a field or element; the place's text), or is an expression
    /// whose value something else still references.
    Held(Option<String>),
    /// A call through a function value, an interface or an overridden method borrows it.
    Borrowed,
}

/// Report resources copied into spawned tasks (module docs).
pub(crate) fn check_boundaries(cx: &mut Ctx) {
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    let senders = channel_senders(cx, &fns);
    for d in fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let locals = f.body.locals.clone();
        let mut found: Vec<(Span, TyId, Copied, Site)> = vec![];
        let mut kept: Vec<(Span, String, bool, TyId)> = vec![];
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            let (i, args) = match &e.kind {
                E::Call {
                    callee: Callee::Intrinsic(i),
                    args,
                } => (i, args),
                E::Call {
                    callee: Callee::Def(g, _),
                    args,
                } => {
                    for &p in senders.get(g).into_iter().flatten() {
                        if let Some(a) = args.get(p) {
                            crossing_copies(cx, a, &locals, Site::Send(*g), &mut found);
                        }
                    }
                    return;
                }
                _ => return,
            };
            match (i, args.as_slice()) {
                (Intrinsic::Spawn | Intrinsic::SpawnHandled, [p]) => {
                    spawned_copies(cx, p, &locals, &mut found)
                }
                (Intrinsic::SharedNew, [x]) => shared_kept(cx, x, &locals, &mut kept),
                _ => {}
            }
        });
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        for (span, ty, why, site) in found {
            report(cx, span, ty, why, site);
        }
        for (span, name, local, ty) in kept {
            report_shared(cx, span, &name, local, ty);
        }
    }
}

/// The functions among `fns` that hand a parameter to a channel, with the indices of those
/// parameters (`this` is parameter 0, as in the call's arguments): `Channel.send` and
/// `Channel.trySend` pass it to `Intrinsic::ChanSend` / `ChanTrySend`, and a function that
/// passes its parameter on to one of those (`enqueue(ch, x)` calling `ch.send(x)`) hands it
/// over too.
fn channel_senders(cx: &mut Ctx, fns: &[DefId]) -> HashMap<DefId, Vec<usize>> {
    let mut out: HashMap<DefId, Vec<usize>> = HashMap::new();
    // (function, callee, argument index, parameter index): the parameter is passed on as is.
    let mut passes: Vec<(DefId, DefId, usize, usize)> = vec![];
    for &d in fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let n = f.params.len();
        let param = |a: Option<&Expr>| match a.map(|v| &v.kind) {
            Some(E::Local(l, _)) if (l.0 as usize) < n => Some(l.0 as usize),
            _ => None,
        };
        let mut sent: Vec<usize> = vec![];
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| match &e.kind {
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::ChanSend | Intrinsic::ChanTrySend),
                args,
            } => sent.extend(param(args.get(1))),
            E::Call {
                callee: Callee::Def(g, _),
                args,
            } => {
                for (i, a) in args.iter().enumerate() {
                    if let Some(p) = param(Some(a)) {
                        passes.push((d, *g, i, p));
                    }
                }
            }
            _ => {}
        });
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        if !sent.is_empty() {
            out.insert(d, sent);
        }
    }
    loop {
        let mut grew = false;
        for &(d, g, i, p) in &passes {
            let forwards = out.get(&g).is_some_and(|ps| ps.contains(&i));
            if forwards && !out.get(&d).is_some_and(|ps| ps.contains(&p)) {
                out.entry(d).or_default().push(p);
                grew = true;
            }
        }
        if !grew {
            return out;
        }
    }
}

/// The values `a` (an argument that crosses to another task at `site`) copies there although
/// they own a resource that cannot be copied: `a` itself when it is a share
/// (`Intrinsic::Share`: the program still uses the value), and the shares an object, array,
/// tuple or union literal is built from in place.
fn crossing_copies(
    cx: &mut Ctx,
    a: &Expr,
    locals: &[LocalDef],
    site: Site,
    out: &mut Vec<(Span, TyId, Copied, Site)>,
) {
    let parts: Vec<&Expr> = match &a.kind {
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::Share),
            args: place,
        } => {
            if cx.owns_uncopyable(a.ty) {
                let why = match place.first().map(|x| &x.kind) {
                    Some(E::Local(l, _)) => Copied::UsedAgain(locals[l.0 as usize].name.clone()),
                    Some(_) => Copied::Held(Some(place_text(cx, locals, &place[0]))),
                    None => Copied::Held(None),
                };
                out.push((a.span, a.ty, why, site));
            }
            return;
        }
        E::AdtLit { fields: args, .. }
        | E::Variant { args, .. }
        | E::ArrayLit(args)
        | E::Tuple(args) => args.iter().collect(),
        E::WrapSome(e) | E::Upcast(e) | E::ToDyn { expr: e, .. } => vec![e],
        _ => return,
    };
    for p in parts {
        crossing_copies(cx, p, locals, site, out);
    }
}

/// The values `p` (the operand of `spawn`) copies into the task although they own a resource
/// that cannot be copied.
fn spawned_copies(
    cx: &mut Ctx,
    p: &Expr,
    locals: &[LocalDef],
    out: &mut Vec<(Span, TyId, Copied, Site)>,
) {
    match &p.kind {
        E::If { then, els, .. } => {
            spawned_copies(cx, then, locals, out);
            spawned_copies(cx, els, locals, out);
        }
        E::Block(b) if b.stmts.is_empty() => {
            if let Some(v) = &b.value {
                spawned_copies(cx, v, locals, out);
            }
        }
        E::Call {
            callee: Callee::Def(..),
            args,
        } => {
            for a in args {
                crossing_copies(cx, a, locals, Site::Spawn, out);
            }
        }
        // An async closure literal called at once is spawned like the literal itself (its
        // captures go to the task; velt_vir async_fn/spawn.rs).
        E::Call {
            callee: Callee::Indirect(f),
            args,
        } if args.is_empty() && matches!(f.kind, E::Closure(_)) => {
            spawned_copies(cx, f, locals, out);
        }
        // Through a function value, an interface or an overridden method, the callee only
        // borrows its arguments (and receiver): the task always gets copies.
        E::Call {
            callee: Callee::Indirect(_) | Callee::Virtual { .. } | Callee::Dyn { .. },
            args,
        } => {
            for a in args {
                if cx.owns_uncopyable(a.ty) {
                    out.push((a.span, a.ty, Copied::Borrowed, Site::Spawn));
                }
            }
        }
        E::Closure(c) => {
            let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
                return;
            };
            let shared: Vec<_> = f
                .captures
                .iter()
                .filter(|k| k.mode == PassMode::Owned && k.share)
                .map(|k| k.outer)
                .collect();
            for l in shared {
                let local = &locals[l.0 as usize];
                if cx.owns_uncopyable(local.ty) {
                    out.push((
                        p.span,
                        local.ty,
                        Copied::UsedAgain(local.name.clone()),
                        Site::Spawn,
                    ));
                }
            }
        }
        _ => {}
    }
}

fn report(cx: &mut Ctx, span: Span, ty: TyId, why: Copied, site: Site) {
    let (op, task) = match site {
        Site::Spawn => ("spawn".to_string(), "the task"),
        Site::Send(g) => {
            let name = match &cx.defs[g.0 as usize] {
                Some(Def::Fn(f)) => f.name.rsplit(['.', ':']).next().unwrap_or("send"),
                _ => "send",
            };
            (name.to_string(), "the receiving task")
        }
    };
    let what = cx.uncopyable_why(ty);
    let part = cx.uncopyable_part(ty).unwrap_or(ty);
    let share = "or share it instead of copying: `shared(new Mutex(…))`";
    let clone = match cx.ty.kind(part) {
        TyKind::Promise(..) => "await the promise first".to_string(),
        _ => format!(
            "give `{}` a `clone()` method that duplicates the resource",
            cx.display(part)
        ),
    };
    let d = match why {
        Copied::Borrowed => Diagnostic::error(
            format!("the spawned task would get a copy of this value (a call through a function value, an interface or an overridden method only borrows it), but {what}"),
            span,
        )
        .with_note(format!("call a function directly to hand the value over (`spawn(f(x))`), {clone}, {share}")),
        Copied::UsedAgain(name) => Diagnostic::error(
            format!("`{name}` is still used after `{op}`, so {task} would get a copy, but {what}"),
            span,
        )
        .with_note(format!(
            "pass the last reference (don't use `{name}` after the `{op}`), {clone}, {share}"
        )),
        Copied::Held(place) => {
            let held = place.map_or_else(
                || "this value is still referenced elsewhere".to_string(),
                |p| format!("`{p}` stays where it is held"),
            );
            Diagnostic::error(
                format!("{held}, so {task} would get a copy, but {what}"),
                span,
            )
            .with_note(format!(
                "{task} cannot take it from where it is held; {clone}, {share}"
            ))
        }
    };
    cx.error(d);
}

/// The values the program still uses that `shared(x)` would take into the `shared`: `x`, and
/// what it is built from in place (a `Mutex`, an object, array or union literal, a `new`
/// whose constructor keeps the argument, a closure's captures), as shares of a variable used
/// again. `shared` takes the value itself (semantics stage 2, §6): a copy made for it would
/// silently stop aliasing the variable.
fn shared_kept(cx: &Ctx, x: &Expr, locals: &[LocalDef], out: &mut Vec<(Span, String, bool, TyId)>) {
    let parts: Vec<&Expr> = match &x.kind {
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::Share),
            args,
        } => {
            let shared_ty = matches!(cx.ty.kind(x.ty), TyKind::Str | TyKind::Shared(_));
            if !shared_ty {
                if let Some(p) = args.first() {
                    let local = matches!(p.kind, E::Local(..));
                    out.push((x.span, place_text(cx, locals, p), local, x.ty));
                }
            }
            return;
        }
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::MutexNew),
            args,
        }
        | E::AdtLit { fields: args, .. }
        | E::Variant { args, .. }
        | E::ArrayLit(args)
        | E::Tuple(args)
        | E::New { args, .. } => args.iter().collect(),
        E::WrapSome(e) | E::Upcast(e) | E::Downcast(e) | E::ToDyn { expr: e, .. } => vec![e],
        E::Closure(c) => {
            if let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] {
                for k in f
                    .captures
                    .iter()
                    .filter(|k| k.mode == PassMode::Owned && k.share)
                {
                    let l = &locals[k.outer.0 as usize];
                    out.push((x.span, l.name.clone(), true, l.ty));
                }
            }
            return;
        }
        _ => return,
    };
    for p in parts {
        shared_kept(cx, p, locals, out);
    }
}

fn report_shared(cx: &mut Ctx, span: Span, name: &str, local: bool, ty: TyId) {
    let msg = match local {
        true => format!("`{name}` is still used after `shared(...)`; `shared` takes the value itself (move it in, or share a clone)"),
        false => format!("`{name}` stays where it is held; `shared(...)` takes the value itself (share a clone)"),
    };
    let copy = match cx.owns_uncopyable(ty) {
        true => {
            let part = cx.uncopyable_part(ty).unwrap_or(ty);
            format!(
                ", or give `{}` a `clone()` and pass `{name}.clone()`",
                cx.display(part)
            )
        }
        false => format!(", or pass a copy: `{name}.clone()`"),
    };
    cx.error(Diagnostic::error(msg, span).with_note(format!(
        "use it through the `shared` value from now on (`m.with((v) => …)`){copy}"
    )));
}
