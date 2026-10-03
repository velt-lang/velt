//! After the inference fixpoint:
//! - named functions used as function values may be called with aliasing arguments (calls
//!   through a function value don't check exclusivity between their arguments), so their
//!   modified params lose `BorrowMut`'s no-alias guarantee: they become `Borrow` with
//!   `LocalDef::mutable` set (like closure params; direct call sites stay checked);
//! - reassigning a non-Copy param of a function with a fixed ABI (closures, overridden and
//!   interface methods) is an error: it cannot own its argument;
//! - arguments for "soft" owned params (owned only because the callee reassigns them) are
//!   recorded as soft moves: a caller that uses the value again passes a clone.

use velt_common::Diagnostic;

use crate::body::places::is_place;
use crate::ctx::Ctx;
use crate::defs::FnKind;
use crate::hir::{Callee, DefId, Expr, ExprKind as E, FnDef, PassMode};
use crate::visit;

use super::evidence;

/// Demote the modified params of named functions used as values (module docs).
pub(super) fn demote_fn_values(cx: &mut Ctx) {
    let values: Vec<DefId> = cx.fn_values.iter().map(|(d, ..)| *d).collect();
    for d in values {
        let info = cx.fn_info_mut(d);
        let mut demoted = vec![];
        for (k, p) in info.params.iter_mut().enumerate() {
            if p.mode == PassMode::BorrowMut {
                p.mode = PassMode::Borrow;
                demoted.push(k);
            }
        }
        let skip = usize::from(info.this.is_some());
        if let Some(crate::hir::Def::Fn(f)) = &mut cx.defs[d.0 as usize] {
            for k in demoted {
                if let Some(p) = f.params.get(skip + k) {
                    f.body.locals[p.local.0 as usize].mutable = true;
                }
            }
        }
    }
}

/// Fixed-ABI functions cannot reassign a borrowed (non-Copy) param.
pub(super) fn reassigned_fixed_params(cx: &mut Ctx, d: DefId, f: &mut FnDef) {
    let info = cx.fn_info(d);
    if !info.fixed_modes || info.is_async || info.kind == FnKind::Extern {
        return;
    }
    let what = match info.kind {
        FnKind::Closure => "a closure",
        _ => "an overridden or interface method",
    };
    let ev = evidence::collect(cx, &mut f.body.block);
    let first = f.captures.len() + usize::from(f.self_ty.is_some());
    for p in &f.params[first..] {
        let Some(&at) = ev.reassigned.get(&p.local) else {
            continue;
        };
        if matches!(p.mode, PassMode::Copy | PassMode::Owned) {
            continue;
        }
        let name = &f.body.locals[p.local.0 as usize].name;
        cx.error(
            Diagnostic::error(
                format!("cannot assign to parameter `{name}` of {what}"),
                at,
            )
            .with_note(format!(
                "its argument stays the caller's; assign a local copy instead: `let local = {name}.clone();`"
            )),
        );
    }
}

/// Record the arguments of soft owned params in `d`'s soft moves.
pub(super) fn soft_args(cx: &mut Ctx, d: DefId, f: &mut FnDef) {
    let mut spans = vec![];
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
        let E::Call {
            callee: Callee::Def(callee, _),
            args,
        } = &e.kind
        else {
            return;
        };
        let info = cx.fn_info(*callee);
        let skip = usize::from(info.this.is_some());
        for &k in &info.soft_params {
            let Some(mut a) = args.get(skip + k) else {
                continue;
            };
            while let E::WrapSome(x) | E::Upcast(x) | E::Downcast(x) = &a.kind {
                a = x;
            }
            if is_place(a) && super::soft::is_moved_place(a) {
                spans.push((a.span, a.ty));
            }
        }
    });
    for (span, ty) in spans {
        if cx.is_shared_value(ty) {
            cx.fn_info_mut(d).soft_moves.push(span);
        }
    }
}
