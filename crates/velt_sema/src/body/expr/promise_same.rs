//! A promise converted for an argument of a spawned call (`spawn(g(p))`, `g` taking a
//! `Promise<unknown>`) is not `Intrinsic::PromiseSame`: the call's arguments move to the new
//! task, where a promise has an identity of its own (docs/reference/async.md), and the second
//! operand would read the temporary the first one moved.

use crate::hir::{self, Callee, ExprKind as H, Intrinsic};

/// Replace each `PromiseSame(q, p)` among the arguments of the calls spawned call `p` starts
/// (`super::tasks::spawned_calls`) with `q`.
pub(super) fn unconvert_spawned(p: &mut hir::Expr) {
    match &mut p.kind {
        H::Call { args, .. } => args.iter_mut().for_each(unconvert),
        H::If { then, els, .. } => {
            unconvert_spawned(then);
            unconvert_spawned(els);
        }
        H::Block(b) => {
            if let Some(v) = &mut b.value {
                unconvert_spawned(v);
            }
        }
        _ => {}
    }
}

fn unconvert(a: &mut hir::Expr) {
    match &mut a.kind {
        H::Call {
            callee: Callee::Intrinsic(Intrinsic::PromiseSame),
            args,
        } if args.len() == 2 => {
            let q = args.swap_remove(0);
            *a = q;
        }
        H::Block(b) => {
            if let Some(v) = &mut b.value {
                unconvert(v);
            }
        }
        _ => {}
    }
}
