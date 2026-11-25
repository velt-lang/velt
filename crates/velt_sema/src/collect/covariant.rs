//! Covariant results of interface implementations (#3771). A class method implements an
//! interface method whose result its own converts to as a call converts it: a class that
//! declares `implements` of the result interface (`step(): Counter` for `step(): Fluent`), a
//! subclass of the result class, or `this` of such a class, as in TypeScript (which also
//! accepts a class that matches the interface without declaring it; Velt converts a class to
//! an interface only through `implements`, at a call as here).
//!
//! The implementation's table entry is then a method of the class, `C.<Fluent.step>`, that
//! calls the method's own entry (the method, its vtable slot's trampoline, or the entry that
//! returns the receiver of a method returning `this`, `crate::hand_back`) and converts the
//! result: an `Upcast` to a base class, or a `ToDyn` through the result class's implementation
//! of the interface. Its implementation index is known once the batch's implementations are
//! built (`finish`). Calls through the class itself still call the method directly.

use velt_common::{Diagnostic, Span};

use super::forwarders::{synth_method, Host, Target};
use super::ret_infer::is_pending;
use crate::ctx::Ctx;
use crate::defs::ParamSig;
use crate::hir::{self, Def, DefId, ExprKind as H, StmtKind as S, TyId, TyKind};

/// How the result of an implementing method converts to the interface method's.
#[derive(Clone, Copy, Debug)]
pub(super) enum Conv {
    /// To a base class (`want`).
    Upcast(TyId),
    /// To interface `iface` (as `want`), through the result class's implementation of it.
    ToDyn { iface: DefId, want: TyId },
}

/// A converting entry whose `ToDyn` waits for its implementation index (`finish`): the entry,
/// the class type it converts and the interface.
pub(super) type Pending = (DefId, TyId, DefId);

/// How method `def` (of a class instantiated with `owner_args`), whose result differs from
/// `want`, converts to it, if it does as a call would. `None` for an equal result, and while
/// the result is still being inferred.
pub(super) fn conversion(
    cx: &mut Ctx,
    def: DefId,
    owner_args: &[TyId],
    want: TyId,
) -> Option<Conv> {
    if is_pending(cx, def) {
        return None;
    }
    let have = cx.fn_info(def).ret;
    let have = cx.subst(have, owner_args);
    if cx.same_ty(have, want) {
        return None;
    }
    match cx.ty.kind(want).clone() {
        TyKind::Dyn(iface, args) if implements(cx, have, iface, &args) => {
            Some(Conv::ToDyn { iface, want })
        }
        TyKind::Adt(..) if cx.class_of(want).is_some() && is_base(cx, have, want) => {
            Some(Conv::Upcast(want))
        }
        _ => None,
    }
}

/// Does class type `t` (or a base class of it) declare `implements iface<args>`?
fn implements(cx: &mut Ctx, t: TyId, iface: DefId, args: &[TyId]) -> bool {
    let mut cur = Some(t);
    for _ in 0..64 {
        let Some((d, targs)) = cur.and_then(|t| cx.class_of(t)) else {
            return false;
        };
        let bounds = cx.adt(d).map(|a| a.implements.clone()).unwrap_or_default();
        for b in bounds.iter().filter(|b| b.iface == iface) {
            let bargs: Vec<TyId> = b.args.iter().map(|a| cx.subst(*a, &targs)).collect();
            if cx.same_tys(&bargs, args) {
                return true;
            }
        }
        cur = cur.and_then(|t| cx.base_of(t));
    }
    false
}

/// Is class type `base` a base class of class type `t`?
fn is_base(cx: &mut Ctx, t: TyId, base: TyId) -> bool {
    let mut cur = cx.base_of(t);
    for _ in 0..64 {
        let Some(b) = cur else {
            return false;
        };
        if cx.same_ty(b, base) {
            return true;
        }
        cur = cx.base_of(b);
    }
    false
}

/// The table entry of class `d` (self type `self_ty`) for interface method `name` of
/// `iname`, implemented by `def` (instantiated with `owner_args`) through `entry`, whose
/// result converts by `conv`: a method calling `entry` and converting what it returns.
#[allow(clippy::too_many_arguments)] // the implementation pair's parts and the conversion
pub(super) fn entry(
    cx: &mut Ctx,
    (d, self_ty): (DefId, TyId),
    (iname, name): (&str, &str),
    (def, owner_args): (DefId, &[TyId]),
    entry: DefId,
    conv: Conv,
    pending: &mut Vec<Pending>,
) -> DefId {
    let host = Host::adt(cx, d);
    let self_args: Vec<TyId> = (0..host.generics.len() as u32)
        .map(|i| cx.ty.param(i))
        .collect();
    // The entry is a method of `d` (a trampoline) or of the class declaring `def`.
    let targs = match cx.fn_info(entry).owner == Some(d) {
        true => self_args,
        false => owner_args.to_vec(),
    };
    let params: Vec<ParamSig> = (cx.fn_info(def).params.clone().into_iter())
        .map(|mut p| {
            p.ty = cx.subst(p.ty, owner_args);
            p.default = None;
            p
        })
        .collect();
    let have = cx.fn_info(def).ret;
    let have = cx.subst(have, owner_args);
    let want = match conv {
        Conv::Upcast(w) | Conv::ToDyn { want: w, .. } => w,
    };
    let mut_this = cx
        .fn_info(def)
        .this
        .as_ref()
        .is_some_and(|t| t.mode == hir::PassMode::BorrowMut);
    let qual = cx.adt(d).map(|a| a.qual_name.clone()).unwrap_or_default();
    let label = format!("{qual}.<{iname}.{name}>");
    let target = Target::Default(entry, targs);
    let e = synth_method(cx, &host, self_ty, label, params, want, mut_this, target);
    let Some(Def::Fn(f)) = &mut cx.defs[e.0 as usize] else {
        panic!("ICE: a synthesized method has a body");
    };
    let Some(hir::Stmt {
        kind: S::Return(Some(value)),
        span,
    }) = f.body.block.stmts.last_mut()
    else {
        panic!("ICE: a synthesized method returns its call");
    };
    let span: Span = *span;
    let mut call = std::mem::replace(value, unit_placeholder(want, span));
    call.ty = have;
    let kind = match conv {
        Conv::Upcast(_) => H::Upcast(Box::new(call)),
        Conv::ToDyn { iface, .. } => {
            pending.push((e, have, iface));
            H::ToDyn {
                expr: Box::new(call),
                impl_index: u32::MAX,
            }
        }
    };
    *value = hir::Expr {
        kind,
        ty: want,
        span,
    };
    e
}

/// Sets the implementation index of each pending entry's `ToDyn`, now that the batch's
/// implementations are built.
pub(super) fn finish(cx: &mut Ctx, pending: Vec<Pending>) {
    for (e, have, iface) in pending {
        let Some(found) = cx.find_impl(have, iface).map(|(i, ..)| i) else {
            let span = cx.fn_info(e).span;
            cx.error(Diagnostic::error(
                "a method whose result converts to the interface's is not supported here yet: its class's implementation of the result interface is declared in another batch of modules",
                span,
            ));
            continue;
        };
        let Some(Def::Fn(f)) = &mut cx.defs[e.0 as usize] else {
            continue;
        };
        let Some(hir::Stmt {
            kind:
                S::Return(Some(hir::Expr {
                    kind: H::ToDyn { impl_index, .. },
                    ..
                })),
            ..
        }) = f.body.block.stmts.last_mut()
        else {
            continue;
        };
        *impl_index = found;
    }
}

/// A placeholder for the value being replaced.
fn unit_placeholder(ty: TyId, span: Span) -> hir::Expr {
    hir::Expr {
        kind: H::Lit(hir::Lit::Null),
        ty,
        span,
    }
}
