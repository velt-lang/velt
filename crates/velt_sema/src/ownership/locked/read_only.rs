//! `rw.read(f)` (`Intrinsic::LockRead`) gives the callback the locked value while other readers
//! read it too: a callback that may modify it (its parameter is written, directly or through a
//! function it passes it to, as mode inference found) is an error, for a named function as for a
//! literal. A function value, whose body is not visible, is an error too: velt_rt's read mode
//! stops a write only to an object with a count (one that has aliases, which mode inference cannot
//! follow).

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::hir::{Callee, Def, Expr, ExprKind as E, Intrinsic, PassMode};
use crate::visit;

pub(super) fn check(cx: &mut Ctx) {
    let fns: Vec<_> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    let mut callbacks: Vec<Expr> = vec![];
    for d in fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Call {
                callee: Callee::Intrinsic(Intrinsic::LockRead),
                args,
            } = &e.kind
            {
                if let Some(cb) = args.get(1) {
                    callbacks.push(cb.clone());
                }
            }
        });
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
    }
    for cb in callbacks {
        let at = match &cb.kind {
            E::Closure(def) | E::FnRef(def, _) => match &cx.defs[def.0 as usize] {
                Some(Def::Fn(c)) => c.params[c.captures.len()..].first().and_then(|p| {
                    let l = &c.body.locals[p.local.0 as usize];
                    let at = match cb.kind {
                        E::Closure(_) => l.span,
                        _ => cb.span,
                    };
                    (l.mutable && p.mode != PassMode::Copy).then_some(at)
                }),
                // A function without a body here: its inferred mode says.
                _ => {
                    let modifies = cx
                        .fn_info(*def)
                        .params
                        .first()
                        .is_some_and(|p| p.mode == PassMode::BorrowMut);
                    modifies.then_some(cb.span)
                }
            },
            // A function value: its body may write an object no count tracks, which read mode
            // could not stop either.
            _ => {
                cx.diags.push(
                    Diagnostic::error("`read` needs a function literal or a named function", cb.span)
                        .with_note("other readers read the value at the same time, so the compiler checks that the function passed to `read` does not modify it"),
                );
                continue;
            }
        };
        if let Some(at) = at {
            cx.diags.push(
                Diagnostic::error(
                    "the function passed to `read` modifies the locked value",
                    at,
                )
                .with_note("other readers read it at the same time: change it in `write` instead"),
            );
        }
    }
}
