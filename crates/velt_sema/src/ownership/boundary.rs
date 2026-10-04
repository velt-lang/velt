//! Thread boundaries (docs/reference/async.md "Tasks"): a value that enters a spawned task while
//! the program still uses it is deep-copied for the task, and a deep copy of a resource calls
//! the type's own `clone()` (velt_vir `transfer.rs`). A value owning a `[Symbol.dispose]`
//! resource without one cannot be copied, so passing it to `spawn` while it is still in use is
//! an error here, where it is visible: an argument of `spawn(f(…))` that became a share
//! (`Intrinsic::Share`, the variable is used again) and a shared capture of a spawned async
//! closure literal (or of one called at once). Values that only turn out to be shared at run time (another reference made
//! earlier) are checked by the transfer itself, which panics instead of releasing the
//! resource twice.

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, LocalDef, PassMode, TyId, TyKind,
};
use crate::visit;

use super::validate::place_text;

/// Why `spawn` copies a value.
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
    for d in fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let locals = f.body.locals.clone();
        let mut found: Vec<(Span, TyId, Copied)> = vec![];
        let mut kept: Vec<(Span, String, bool, TyId)> = vec![];
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            let E::Call {
                callee: Callee::Intrinsic(i),
                args,
            } = &e.kind
            else {
                return;
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
        for (span, ty, why) in found {
            report(cx, span, ty, why);
        }
        for (span, name, local, ty) in kept {
            report_shared(cx, span, &name, local, ty);
        }
    }
}

/// The values `p` (the operand of `spawn`) copies into the task although they own a resource
/// that cannot be copied.
fn spawned_copies(
    cx: &mut Ctx,
    p: &Expr,
    locals: &[LocalDef],
    out: &mut Vec<(Span, TyId, Copied)>,
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
                if let E::Call {
                    callee: Callee::Intrinsic(Intrinsic::Share),
                    args: place,
                } = &a.kind
                {
                    if cx.owns_uncopyable(a.ty) {
                        let why = match place.first().map(|x| &x.kind) {
                            Some(E::Local(l, _)) => {
                                Copied::UsedAgain(locals[l.0 as usize].name.clone())
                            }
                            Some(_) => Copied::Held(Some(place_text(cx, locals, &place[0]))),
                            None => Copied::Held(None),
                        };
                        out.push((a.span, a.ty, why));
                    }
                }
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
                    out.push((a.span, a.ty, Copied::Borrowed));
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
                    out.push((p.span, local.ty, Copied::UsedAgain(local.name.clone())));
                }
            }
        }
        _ => {}
    }
}

fn report(cx: &mut Ctx, span: Span, ty: TyId, why: Copied) {
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
            format!("`{name}` is still used after `spawn`, so the task would get a copy, but {what}"),
            span,
        )
        .with_note(format!(
            "pass the last reference (don't use `{name}` after the `spawn`), {clone}, {share}"
        )),
        Copied::Held(place) => {
            let held = place.map_or_else(
                || "this value is still referenced elsewhere".to_string(),
                |p| format!("`{p}` stays where it is held"),
            );
            Diagnostic::error(
                format!("{held}, so the task would get a copy, but {what}"),
                span,
            )
            .with_note(format!(
                "the task cannot take it from where it is held; {clone}, {share}"
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
