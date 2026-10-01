//! Strings are values (docs/internals/design/semantics.md, stage 1): using a string (or a `string | null`,
//! see `Ctx::is_string_value`) never leaves a variable "moved", and a string can be taken from
//! anywhere — a borrowed param, a field of a class, an array or `for...of` element — without
//! `.clone()`.
//!
//! After ownership inference (which still treats string moves as moves, so a param that stores
//! or returns a string is `Owned` and callers can hand theirs over for free), every move of a
//! string place, and every by-value string capture of an escaping closure, becomes a *soft move*
//! of its function (`FnInfo::soft_moves`, see [`super::soft`]): [`super::validate`] turns the ones
//! that may not move into copies, `crate::moves` the ones whose place is used again. What is left
//! are moves out of dead places, which cost nothing; a copy is a refcount increment at most.

use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::hir::{Capture, Def, DefId, Expr, ExprKind as E, PassMode};
use crate::visit;

use super::soft::is_moved_place;

/// Record the string moves and string-capturing closures of every checked body as soft moves.
pub(crate) fn soften_string_moves(cx: &mut Ctx) {
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
        let mut spans = vec![];
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| match &e.kind {
            _ if cx.is_string_value(e.ty) && is_moved_place(e) => spans.push(e.span),
            E::Closure(c) if string_captures(cx, *c).next().is_some() => spans.push(e.span),
            _ => {}
        });
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        cx.fn_info_mut(d).soft_moves.extend(spans);
    }
}

/// The by-value captures of string variables of closure `c`.
pub(crate) fn string_captures<'a>(cx: &'a Ctx, c: DefId) -> impl Iterator<Item = &'a Capture> {
    let f = match &cx.defs[c.0 as usize] {
        Some(Def::Fn(f)) => Some(f),
        _ => None,
    };
    f.into_iter().flat_map(move |f| {
        f.captures.iter().filter(move |cap| {
            cap.mode == PassMode::Owned
                && cx.is_string_value(f.body.locals[cap.inner.0 as usize].ty)
        })
    })
}

/// Make the string captures of closure `c` copies (the enclosing variables stay usable).
pub(crate) fn copy_string_captures(cx: &mut Ctx, c: DefId) {
    let (str_, opt_str) = (cx.ty.str_, cx.ty.option(cx.ty.str_));
    if let Some(Def::Fn(f)) = &mut cx.defs[c.0 as usize] {
        let locals = &f.body.locals;
        for cap in f.captures.iter_mut() {
            let ty = locals[cap.inner.0 as usize].ty;
            if cap.mode == PassMode::Owned && (ty == str_ || ty == opt_str) {
                cap.clone = true;
            }
        }
    }
}
