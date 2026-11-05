//! The constructs of `hook_demand::Construct` in what walking live code found ([`super`]).

use velt_common::std_hooks::{Construct, ConstructSet};

use super::Walk;
use crate::ctx::Ctx;
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
