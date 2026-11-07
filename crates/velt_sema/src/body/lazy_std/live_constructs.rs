//! The constructs of `hook_demand::Construct` in what walking live code found ([`super`]).

use velt_common::std_hooks::{Construct, ConstructSet};

use super::Walk;
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{self, TyId, TyKind};

/// The constructs of `w`'s conversions and formatted values.
pub(super) fn evaluate(cx: &mut Ctx, w: &Walk) {
    let mut present = 0;
    for &t in &w.texts {
        present |= match cx.ty.kind(t) {
            TyKind::Int(it) if it.is_signed() => Construct::TextInt.bit(),
            TyKind::Int(_) => Construct::TextUint.bit(),
            // Floats print through the float hooks (`number_text::reach_hooks`).
            TyKind::Float(_) | TyKind::Bool | TyKind::Str => 0,
            // An instance may be an integer of either sign.
            TyKind::Param(_) | TyKind::Op(..) => {
                Construct::TextInt.bit() | Construct::TextUint.bit() | Construct::Format.bit()
            }
            _ => format_bit(cx, t),
        };
    }
    for &t in &w.formats {
        present |= format_bit(cx, t);
    }
    cx.lazy.live.present |= present;
}

/// Is `Construct::Format` missing for what lowering reports with the inferred `throws` of a live
/// function, when thrown and not caught? [`super::live`] sees what code throws, but inference also
/// gives a function what a call of a function value throws, and what the interface or the base
/// class it implements declares (`throws::infer`, the method groups), whatever its body does. That
/// is known only after the passes, too late to load a hook, so the check runs again with it
/// assumed (`check_program`). A type parameter stands for what its callers' `throws` show. A
/// module constant's initializer has no caller, and reports what it runs: any type argument in it
/// may be what a generic `throws` stands for, by whatever expression the instance is reached.
pub(crate) fn thrown_needs_format(cx: &mut Ctx) -> bool {
    let bit = Construct::Format.bit();
    if cx.lazy.modules.is_empty() || cx.lazy.live.present & bit != 0 {
        return false;
    }
    let live: Vec<_> = cx.lazy.live.live.iter().copied().collect();
    let mut thrown: Vec<TyId> = (live.iter())
        .filter_map(|d| match &cx.info[d.0 as usize] {
            DefInfo::Fn(f) => f.throws,
            _ => None,
        })
        .collect();
    if thrown.iter().any(|&t| is_param(cx, t)) {
        for d in live {
            let ty = &cx.ty;
            if let DefInfo::Global(g) = &mut cx.info[d.0 as usize] {
                if let Some(e) = &mut g.init {
                    crate::visit::exprs_in(e, &mut |e| type_args_in(ty, e, &mut thrown));
                }
            }
        }
    }
    thrown
        .into_iter()
        .any(|t| !is_param(cx, t) && format_bit(cx, t) != 0)
}

/// The type arguments `e` names or has in its type.
fn type_args_in(ty: &crate::types::Types, e: &hir::Expr, out: &mut Vec<TyId>) {
    if let TyKind::Adt(_, args) = ty.kind(e.ty) {
        out.extend(args);
    }
    match &e.kind {
        hir::ExprKind::Call {
            callee: hir::Callee::Def(_, targs),
            ..
        } => out.extend(targs),
        hir::ExprKind::New { type_args, .. } => out.extend(type_args),
        _ => {}
    }
}

fn is_param(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::Param(_) | TyKind::Op(..))
}

pub(super) fn format_bit(cx: &mut Ctx, t: TyId) -> ConstructSet {
    match may_print_int(cx, t) {
        true => Construct::Format.bit(),
        false => 0,
    }
}

/// May lowering's text or formatting glue for a value of type `t` print an integer: is it, or
/// may it hold, anything but a float, a boolean, a string, a symbol, `undefined` or a function?
/// An array prints its length past 100 elements, a map its size, a class its fields (and may
/// be a subclass's instance), an enum its discriminant; `any` and a type parameter may hold
/// anything.
fn may_print_int(cx: &mut Ctx, t: TyId) -> bool {
    if let Some(&known) = cx.lazy.live.int_text.get(&t) {
        return known;
    }
    // A recursive type comes back here through its children: no integer so far.
    cx.lazy.live.int_text.insert(t, false);
    let kind = cx.ty.kind(t).clone();
    let may = match &kind {
        TyKind::Float(_)
        | TyKind::Bool
        | TyKind::Str
        | TyKind::Unit
        | TyKind::Never
        | TyKind::Error
        | TyKind::Symbol
        | TyKind::FnPtr { .. }
        | TyKind::Closure(_)
        | TyKind::Literal(hir::LitValue::Str(_) | hir::LitValue::Bool(_)) => false,
        TyKind::Tuple(_)
        | TyKind::Option(..)
        | TyKind::Result(..)
        | TyKind::Promise(..)
        | TyKind::Shared(_) => {
            (crate::types::children(&kind).into_iter()).any(|c| may_print_int(cx, c))
        }
        _ => true,
    };
    cx.lazy.live.int_text.insert(t, may);
    may
}
