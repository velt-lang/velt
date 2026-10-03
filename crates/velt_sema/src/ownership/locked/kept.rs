//! Function values kept in the locked value (`m.with((f) => f())`, `v.go()`; super module
//! docs). Such a function may have captured objects, which are parts of the value then, and a
//! promise its call makes (returned or left running) may use them after the lock is released.
//! Which function it is is not known where it is called, so every function of the program
//! with its type is considered: an async closure copies what it captured for each call, and a
//! named function captures nothing, so those are safe; a synchronous closure that captured
//! something able to hold an object and makes a promise is not.

use crate::ctx::Ctx;
use crate::defs::{BodyState, FnKind};
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, TyId, TyKind};
use crate::visit;

/// May calling a function value of type `ty` make a promise from what the function captured?
pub(super) fn may_promise_captures(cx: &mut Ctx, ty: TyId) -> bool {
    let TyKind::FnPtr { params, ret, .. } = cx.ty.kind(ty).clone() else {
        return false;
    };
    let candidates: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| {
            let info = cx.fn_info(*d);
            info.kind == FnKind::Closure && info.state == BodyState::Done && !info.is_async
        })
        .collect();
    candidates.into_iter().any(|d| {
        let info = cx.fn_info(d);
        let same = info.ret == ret
            && info.params.len() == params.len()
            && info.params.iter().zip(&params).all(|(p, t)| p.ty == *t);
        if !same {
            return false;
        }
        let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
            // Being checked as a callback right now: assume the worst.
            return true;
        };
        let caps: Vec<TyId> = f
            .captures
            .iter()
            .map(|k| f.body.locals[k.inner.0 as usize].ty)
            .collect();
        let holds = caps.into_iter().any(|t| {
            (cx.is_shared_value(t) && !cx.is_string_value(t)) || super::regions::holds_shared(cx, t)
        });
        holds && (cx.holds_promise(ret) || makes_promise(cx, d))
    })
}

/// Does closure `d`'s body make a promise (other than one it spawns)?
fn makes_promise(cx: &mut Ctx, d: DefId) -> bool {
    let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
        return true;
    };
    let mut types = vec![];
    let mut spawned = vec![];
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| match &e.kind {
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::Spawn),
            args,
        } => spawned.extend(args.iter().map(|a| a.span)),
        E::Call { .. } | E::New { .. } if !spawned.contains(&e.span) => types.push(e.ty),
        _ => {}
    });
    cx.defs[d.0 as usize] = Some(Def::Fn(f));
    types.into_iter().any(|t| cx.holds_promise(t))
}
