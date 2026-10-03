//! Soft moves: a move that becomes a share (`Intrinsic::Share` of a borrow) when the place
//! cannot be moved from (a borrowed param, a class field, an array element — found by
//! `validate`) or is used again later (found by the move dataflow, `crate::moves`); otherwise
//! it stays a move. Every move of a shared value is soft (`super::shares`), and so is a place
//! passed to an owned parameter of an async function (every non-Copy param of an async
//! function is owned, so the future never borrows from its caller).

use std::collections::{HashMap, HashSet};

use velt_common::Span;

use crate::body::places::set_place_mode;
use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, LocalId, UseMode};
use crate::visit;

/// Turn the place `e` (moved) into `share(e)`.
pub(super) fn make_share(e: &mut Expr) {
    wrap_place(e, Intrinsic::Share);
}

/// Turn the place `e` (moved) into `clone(e)` (a deep copy).
pub(super) fn make_deep_copy(e: &mut Expr) {
    wrap_place(e, Intrinsic::Clone);
}

/// Turn the place `e` (moved) into `op(e)` of the borrowed place.
fn wrap_place(e: &mut Expr, op: Intrinsic) {
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
            callee: Callee::Intrinsic(op),
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

/// Share the soft moves whose place the move dataflow saw used again; for a closure, the
/// captures of the variables used again (its other captures stay moves, so they live exactly
/// as long as the closure).
pub(crate) fn clone_reused(cx: &mut Ctx, reused: &HashMap<DefId, HashSet<(Span, LocalId)>>) {
    let mut closures: Vec<(DefId, Vec<LocalId>)> = vec![];
    for (d, sites) in reused {
        let Some(Def::Fn(f)) = &mut cx.defs[d.0 as usize] else {
            continue;
        };
        let spans: HashSet<Span> = sites.iter().map(|(span, _)| *span).collect();
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| match &e.kind {
            E::Closure(c) if spans.contains(&e.span) => {
                let used = sites.iter().filter(|(s, _)| *s == e.span);
                closures.push((*c, used.map(|(_, l)| *l).collect()));
            }
            _ if is_moved_place(e) && spans.contains(&e.span) => make_share(e),
            _ => {}
        });
    }
    for (c, used) in closures {
        super::shares::share_captures(cx, c, &used);
    }
}
