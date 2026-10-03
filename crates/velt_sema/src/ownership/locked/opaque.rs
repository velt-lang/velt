//! `with` callbacks whose closures are not found (super module docs, `super::callbacks`): a
//! field, an array element, a `Map` value, a call's result, a parameter of a closure or a
//! method. Nothing about such a callback's body is known, so:
//!
//! - one given a value that can hold a shared object is an error: it could keep a part of the
//!   value, or store an outside object into it, and other threads use the value as soon as the
//!   lock is released (`m.with(h.cb)`: "… comes from a field …"). Pass a closure written there,
//!   or a named function, whose body is checked;
//! - a result holding a promise is an error.
//!
//! Where the parameter or result type depends on a type parameter of the function calling
//! `with`, both are checked at each call that makes it concrete
//! (`function run<T>(m, fs: ((s: S) => T)[]): T { return m.with(fs[0]); }` called with
//! `T = Promise<…>`).

use std::collections::HashSet;

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, TyId, TyKind};
use crate::visit;

/// What an opaque callback's type says, before or after substituting type arguments.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Sig {
    param: Option<TyId>,
    ret: TyId,
}

/// Report the opaque callbacks (in the function calling `with`): here when their types are
/// concrete, else at the calls that make them concrete.
pub(super) fn check(cx: &mut Ctx, opaque: &[(DefId, Expr)]) {
    let mut work: Vec<(DefId, Sig, String)> = vec![];
    let mut reported = HashSet::new();
    for (d, cb) in opaque {
        let TyKind::FnPtr { params, ret, .. } = cx.ty.kind(cb.ty).clone() else {
            continue;
        };
        let sig = Sig {
            param: params.first().copied(),
            ret,
        };
        let what = describe(cb);
        if !report(cx, sig, cb.span, &what, None, &mut reported) {
            work.push((*d, sig, what));
        }
    }
    let mut seen = HashSet::new();
    while let Some((f, sig, what)) = work.pop() {
        if !seen.insert((f, sig)) {
            continue;
        }
        let name = cx.fn_info(f).name.clone();
        for (caller, span, targs) in calls_of(cx, f) {
            let sub = Sig {
                param: sig.param.map(|t| cx.ty.subst(t, &targs)),
                ret: cx.ty.subst(sig.ret, &targs),
            };
            if !report(cx, sub, span, &what, Some(&name), &mut reported) {
                work.push((caller, sub, what.clone()));
            }
        }
    }
}

/// Report what is wrong with an opaque callback of `sig` at `span` (through function `via`
/// when its types became concrete at a call of it); false when a type still depends on a type
/// parameter (checked where it becomes concrete).
fn report(
    cx: &mut Ctx,
    sig: Sig,
    span: Span,
    what: &str,
    via: Option<&str>,
    reported: &mut HashSet<Span>,
) -> bool {
    let param_shared = match sig.param {
        Some(t) if cx.mentions_params(t) => return false,
        Some(t) => {
            (cx.is_shared_value(t) && !cx.is_string_value(t)) || super::regions::holds_shared(cx, t)
        }
        None => false,
    };
    if cx.mentions_params(sig.ret) {
        return false;
    }
    let promise = cx.holds_promise(sig.ret);
    if !(param_shared || promise) || !reported.insert(span) {
        return true;
    }
    let at = via.map_or(String::new(), |f| format!(" (through `{f}`)"));
    let (msg, note) = if promise {
        let r = cx.display(sig.ret);
        (
            format!("the function passed to `with`{at} returns `{r}`, which would run after the lock is released"),
            super::AWAIT_OUTSIDE.to_string(),
        )
    } else {
        let t = cx.display(sig.param.unwrap_or(sig.ret));
        (
            format!("the function passed to `with`{at} comes from {what}, so what it does with the locked `{t}` cannot be checked"),
            "other threads use the value as soon as the lock is released, so a part of it kept outside, or an outside object stored into it, would be shared without the lock; pass a closure written here (`m.with((v) => { … })`) or a named function, whose body is checked".to_string(),
        )
    };
    cx.error(Diagnostic::error(msg, span).with_note(note));
    true
}

/// What an opaque callback expression is, for the message.
fn describe(cb: &Expr) -> String {
    match &cb.kind {
        E::Field { .. } => "an object's field".into(),
        E::Index { .. } => "an array element".into(),
        E::Call { .. } => "a call's result".into(),
        E::Local(..) | E::UnwrapSome(..) | E::UnwrapVariant { .. } => {
            "a variable that is not bound only to closures or named functions (a `Map` value, a parameter of a closure or method, …)".into()
        }
        _ => "an expression".into(),
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
