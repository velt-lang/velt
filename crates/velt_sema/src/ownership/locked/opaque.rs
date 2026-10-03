//! `with` callbacks whose closures are not found (super module docs, `super::callbacks`): a
//! field, an array element, a parameter of a closure or a method.
//!
//! - Every closure of the program with the callback's parameter types may be it, so those are
//!   checked as callbacks too (a part of the value one stores outside is a copy, a promise it
//!   makes from the value an error). That is conservative for such closures called elsewhere,
//!   where a store between their parameter and what they captured is a copy as well.
//! - A result holding a promise is an error where the callback's type is concrete, and at each
//!   call that makes it concrete when it depends on a type parameter of the function calling
//!   `with` (`function run<T>(m, fs: ((s: S) => T)[]): T { return m.with(fs[0]); }` called
//!   with `T = Promise<…>`).

use std::collections::HashSet;

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::defs::{BodyState, FnKind};
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, TyId, TyKind};
use crate::visit;

/// The closures of the program whose parameters have the types of callback type `ty`.
pub(super) fn closures_like(cx: &Ctx, ty: TyId) -> Vec<DefId> {
    let TyKind::FnPtr { params, .. } = cx.ty.kind(ty) else {
        return vec![];
    };
    cx.fn_defs
        .iter()
        .copied()
        .filter(|d| {
            let info = cx.fn_info(*d);
            info.kind == FnKind::Closure && info.state == BodyState::Done && !info.is_async
        })
        .filter(|d| match &cx.defs[d.0 as usize] {
            Some(Def::Fn(f)) => {
                let own = &f.params[f.captures.len()..];
                own.len() == params.len() && own.iter().zip(params).all(|(p, t)| p.ty == *t)
            }
            _ => false,
        })
        .collect()
}

/// Report the opaque callbacks (in the function calling `with`, with their types) whose result
/// holds a promise: here when concrete, else at the calls that make it concrete.
pub(super) fn check_results(cx: &mut Ctx, opaque: &[(DefId, Expr)]) {
    let mut work: Vec<(DefId, TyId)> = vec![];
    let mut reported = HashSet::new();
    for (d, cb) in opaque {
        let TyKind::FnPtr { ret, .. } = cx.ty.kind(cb.ty).clone() else {
            continue;
        };
        if cx.mentions_params(ret) {
            work.push((*d, ret));
        } else if cx.holds_promise(ret) && reported.insert(cb.span) {
            let r = cx.display(ret);
            cx.error(
                Diagnostic::error(
                    format!("the function passed to `with` returns `{r}`, which would run after the lock is released"),
                    cb.span,
                )
                .with_note(super::AWAIT_OUTSIDE),
            );
        }
    }
    let mut seen = HashSet::new();
    while let Some((f, ret)) = work.pop() {
        if !seen.insert((f, ret)) {
            continue;
        }
        for (caller, span, targs) in calls_of(cx, f) {
            let r = cx.ty.subst(ret, &targs);
            if cx.mentions_params(r) {
                work.push((caller, r));
            } else if cx.holds_promise(r) && reported.insert(span) {
                let (name, t) = (cx.fn_info(f).name.clone(), cx.display(r));
                cx.error(
                    Diagnostic::error(
                        format!("`{name}` passes `Mutex.with` a function returning `{t}` here, which would run after the lock is released"),
                        span,
                    )
                    .with_note(super::AWAIT_OUTSIDE),
                );
            }
        }
    }
}

/// The direct calls of `f`: the calling function, the call's span and its type arguments.
fn calls_of(cx: &mut Ctx, f: DefId) -> Vec<(DefId, velt_common::Span, Vec<TyId>)> {
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    let mut out = vec![];
    for caller in fns {
        let Some(Def::Fn(mut body)) = cx.defs[caller.0 as usize].take() else {
            continue;
        };
        visit::exprs_mut(&mut body.body.block, &mut |e: &mut Expr| {
            if let E::Call {
                callee: Callee::Def(g, targs),
                ..
            } = &e.kind
            {
                if *g == f {
                    out.push((caller, e.span, targs.clone()));
                }
            }
        });
        cx.defs[caller.0 as usize] = Some(Def::Fn(body));
    }
    out
}
