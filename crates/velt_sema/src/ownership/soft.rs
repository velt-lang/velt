//! Soft moves: a place passed to an owned parameter of an async function (every non-Copy param
//! of an async function is owned, so the future never borrows from its caller). Moving it is
//! free, but JS code reuses such values (`await mkdir(dir); await writeFile(dir + "/a", s)`), so
//! the argument becomes a deep copy (`Intrinsic::Clone` of a borrow) when the place cannot be
//! moved from (a borrowed param, a class field, an array element — found by `validate`) or is
//! used again later (found by the move dataflow, `crate::moves`). Otherwise it stays a move.
//! Types owning a `[Symbol.dispose]` resource have no clone, so their soft moves are never recorded.

use std::collections::{HashMap, HashSet};

use velt_common::Span;

use crate::body::places::set_place_mode;
use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, UseMode};
use crate::visit;

/// Turn the place `e` (moved) into `clone(e)`.
pub(super) fn make_clone(e: &mut Expr) {
    let (ty, span) = (e.ty, e.span);
    let mut place = std::mem::replace(
        e,
        Expr {
            kind: E::Lit(crate::hir::Lit::Unit),
            ty,
            span,
        },
    );
    set_place_mode(&mut place, UseMode::Borrow);
    *e = Expr {
        kind: E::Call {
            callee: Callee::Intrinsic(Intrinsic::Clone),
            args: vec![place],
        },
        ty,
        span,
    };
}

/// Is `e` a place used by `Move` (a soft move candidate)?
pub(super) fn is_moved_place(e: &Expr) -> bool {
    matches!(
        e.kind,
        E::Local(_, UseMode::Move)
            | E::Field {
                mode: UseMode::Move,
                ..
            }
            | E::Index {
                mode: UseMode::Move,
                ..
            }
            | E::UnwrapSome(_, UseMode::Move)
            | E::UnwrapVariant {
                mode: UseMode::Move,
                ..
            }
    )
}

/// Clone the soft moves whose place the move dataflow saw used again (for a closure: copy its
/// string captures).
pub(crate) fn clone_reused(cx: &mut Ctx, reused: &HashMap<DefId, HashSet<Span>>) {
    let mut closures = vec![];
    for (d, spans) in reused {
        let Some(Def::Fn(f)) = &mut cx.defs[d.0 as usize] else {
            continue;
        };
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| match &e.kind {
            E::Closure(c) if spans.contains(&e.span) => closures.push(*c),
            _ if is_moved_place(e) && spans.contains(&e.span) => make_clone(e),
            _ => {}
        });
    }
    for c in closures {
        super::strings::copy_string_captures(cx, c);
    }
}
