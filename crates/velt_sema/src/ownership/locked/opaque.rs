//! `with` callbacks whose closures are not found (super module docs, `super::callbacks`): a
//! field, an array element, a `Map` value, a call's result, a parameter of a closure or a
//! method. Nothing about such a callback's body is known, so:
//!
//! - one given an object value could keep a part of the value, or store an outside object into
//!   it, and other threads use the value as soon as the lock is released. Every function it may
//!   be is checked instead (`super::candidates`), without changing any: when one of them
//!   crosses the lock, the call is an error naming that function (`m.with(h.cb)`: "… comes
//!   from an object's field, and may be a closure that stores …"); otherwise it is accepted
//!   (#457: reducers in a field, operation lists, handler registries);
//! - a result holding a promise is an error.
//!
//! Where the parameter or result type depends on a type parameter of the function calling
//! `with`, both are checked at each call that makes it concrete
//! (`function run<T>(m, fs: ((s: S) => T)[]): T { return m.with(fs[0]); }` called with
//! `T = Promise<…>`).

use std::collections::HashSet;

use velt_common::{Diagnostic, Span};

use super::candidates::{Candidates, Crossing, Offender};
use super::values::Resolver;
use crate::ctx::Ctx;
use crate::defs::{BodyState, FnKind};
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
pub(super) fn check(
    cx: &mut Ctx,
    cands: &mut Candidates,
    res: &mut Resolver,
    opaque: &[(DefId, Expr)],
) {
    let mut reporter = Reporter {
        cands,
        res,
        reported: HashSet::new(),
    };
    let mut work: Vec<(DefId, Sig, String)> = vec![];
    for (d, cb) in opaque {
        let TyKind::FnPtr { params, ret, .. } = cx.ty.kind(cb.ty).clone() else {
            continue;
        };
        let sig = Sig {
            param: params.first().copied(),
            ret,
        };
        let what = describe(cb);
        if !reporter.report(cx, sig, cb.span, &what, None) {
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
            if !reporter.report(cx, sub, span, &what, Some(&name)) {
                work.push((caller, sub, what.clone()));
            }
        }
    }
}

struct Reporter<'c, 's, 'r> {
    cands: &'c mut Candidates<'s>,
    res: &'r mut Resolver,
    reported: HashSet<Span>,
}

impl Reporter<'_, '_, '_> {
    /// Report what is wrong with an opaque callback of `sig` at `span` (through function `via`
    /// when its types became concrete at a call of it); false when a type still depends on a
    /// type parameter (checked where it becomes concrete).
    fn report(
        &mut self,
        cx: &mut Ctx,
        sig: Sig,
        span: Span,
        what: &str,
        via: Option<&str>,
    ) -> bool {
        let param = match sig.param {
            Some(t) if cx.mentions_params(t) => return false,
            Some(t)
                if (cx.is_shared_value(t) && !cx.is_string_value(t))
                    || super::regions::holds_shared(cx, t) =>
            {
                Some(t)
            }
            _ => None,
        };
        if cx.mentions_params(sig.ret) {
            return false;
        }
        let at = via.map_or(String::new(), |f| format!(" (through `{f}`)"));
        if cx.holds_promise(sig.ret) {
            if self.reported.insert(span) {
                let r = cx.display(sig.ret);
                let msg = format!("the function passed to `with`{at} returns `{r}`, which would run after the lock is released");
                cx.error(Diagnostic::error(msg, span).with_note(super::AWAIT_OUTSIDE));
            }
            return true;
        }
        let Some(param) = param else { return true };
        let Some(off) = self.cands.offender(cx, self.res, param) else {
            return true;
        };
        if self.reported.insert(span) {
            offender_error(
                cx,
                off,
                param,
                span,
                &format!("`with`{at} comes from {what}"),
            );
        }
        true
    }
}

/// The error for an opaque callback (`whence`: where it comes from) that may be function
/// `off`, which crosses the lock.
fn offender_error(cx: &mut Ctx, off: Offender, param: TyId, span: Span, whence: &str) {
    let t = cx.display(param);
    let info = cx.fn_info(off.def);
    let (closure, fn_span) = (info.kind == FnKind::Closure, info.span);
    let who = match closure {
        true => "a closure that".to_string(),
        false => format!(
            "`{}`, which",
            info.name.rsplit("::").next().unwrap_or(&info.name)
        ),
    };
    let (does, label, at) = match off.at {
        Crossing::Store(at) => (
            format!("stores a part of the locked `{t}` outside it, or an outside object into it"),
            "crosses the lock here",
            at,
        ),
        Crossing::Promise(at) => (
            format!(
                "makes a promise from the locked `{t}` that would run after the lock is released"
            ),
            "makes the promise here",
            at,
        ),
    };
    let msg = format!("the function passed to {whence}, and may be {who} {does}");
    let mut d = Diagnostic::error(msg, span);
    if closure {
        d = d.with_label(
            fn_span,
            format!("this closure takes `{t}`, like the function passed"),
        );
    }
    let note = format!("`with` cannot see which function it is given here, so every closure and function taking `{t}` is checked, and this one would share an object between threads without the lock; change it, or pass a closure written here (`m.with((v) => {{ … }})`)");
    cx.error(d.with_label(at, label).with_note(note));
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
