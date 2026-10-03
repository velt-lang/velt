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
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, PassMode, TyId};
use crate::visit;

/// Why `spawn` copies a value.
enum Copied {
    /// It is used again after the `spawn` (the variable's name when it is one).
    UsedAgain(Option<String>),
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
        let locals: Vec<TyId> = f.body.locals.iter().map(|l| l.ty).collect();
        let names: Vec<String> = f.body.locals.iter().map(|l| l.name.clone()).collect();
        let mut found: Vec<(Span, TyId, Copied)> = vec![];
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Call {
                callee: Callee::Intrinsic(Intrinsic::Spawn),
                args,
            } = &e.kind
            {
                if let [p] = args.as_slice() {
                    spawned_copies(cx, p, (&locals, &names), &mut found);
                }
            }
        });
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        for (span, ty, why) in found {
            report(cx, span, ty, why);
        }
    }
}

/// The values `p` (the operand of `spawn`) copies into the task although they own a resource
/// that cannot be copied.
fn spawned_copies(
    cx: &mut Ctx,
    p: &Expr,
    (locals, names): (&[TyId], &[String]),
    out: &mut Vec<(Span, TyId, Copied)>,
) {
    match &p.kind {
        E::If { then, els, .. } => {
            spawned_copies(cx, then, (locals, names), out);
            spawned_copies(cx, els, (locals, names), out);
        }
        E::Block(b) if b.stmts.is_empty() => {
            if let Some(v) = &b.value {
                spawned_copies(cx, v, (locals, names), out);
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
                        let name = place.first().and_then(|x| match x.kind {
                            E::Local(l, _) => names.get(l.0 as usize).cloned(),
                            _ => None,
                        });
                        out.push((a.span, a.ty, Copied::UsedAgain(name)));
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
            spawned_copies(cx, f, (locals, names), out);
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
                let ty = locals[l.0 as usize];
                if cx.owns_uncopyable(ty) {
                    let name = names.get(l.0 as usize).cloned();
                    out.push((p.span, ty, Copied::UsedAgain(name)));
                }
            }
        }
        _ => {}
    }
}

fn report(cx: &mut Ctx, span: Span, ty: TyId, why: Copied) {
    let tn = cx.display(ty);
    let d = match why {
        Copied::Borrowed => Diagnostic::error(
            format!("the spawned task would get a copy of this value (a call through a function value, an interface or an overridden method only borrows it), but `{tn}` owns a resource (`[Symbol.dispose]`) and has no `clone()`"),
            span,
        )
        .with_note("call a function directly to hand the value over (`spawn(f(x))`), give the resource type a `clone()` method that duplicates it, or share it with `shared(...)`"),
        Copied::UsedAgain(name) => {
            let what = name.map_or_else(|| "this value".to_string(), |n| format!("`{n}`"));
            Diagnostic::error(
                format!("{what} is still used after `spawn`, so the task would get a copy, but `{tn}` owns a resource (`[Symbol.dispose]`) and has no `clone()`"),
                span,
            )
            .with_note(format!(
                "pass the last reference (don't use {what} after the `spawn`), give the resource type a `clone()` method that duplicates it, or share it with `shared(...)`"
            ))
        }
    };
    cx.error(d);
}
