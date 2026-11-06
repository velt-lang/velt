//! What tells `record_hooks_needed` that the program converts an `any` to a record with string
//! keys.

use rustc_hash::FxHashSet;

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

/// What one definition of a non-std module tells `record_hooks_needed`: the types in its
/// code, its `AnyTo` targets and the `(value, any)` types of its `ToDyn`s.
pub(super) fn note_user_conversions(
    cx: &mut Ctx,
    types: &[TyId],
    to: &[TyId],
    into: &[(TyId, TyId)],
) {
    if !cx.lazy.user_record {
        let mut seen = FxHashSet::default();
        cx.lazy.user_record = types.iter().any(|&t| holds_string_record(cx, t, &mut seen));
    }
    let held: Vec<TyId> = (into.iter())
        .filter(|(_, d)| cx.dynamic(*d).is_some())
        .map(|(v, _)| *v)
        .collect();
    if !cx.lazy.generic_any {
        cx.lazy.generic_any = to.iter().chain(&held).any(|&t| has_param(cx, t));
    }
    if !cx.lazy.record_conversion {
        cx.lazy.record_conversion = held.iter().any(|&t| holds_record_inside(cx, t));
    }
}

/// Does a value of type `t` held in an `any` have a member, element or value of a type holding a
/// record with string keys, which a write through the `any` converts to? (A record of numbers
/// put into an `any` does not.)
fn holds_record_inside(cx: &Ctx, t: TyId) -> bool {
    let mut seen = FxHashSet::default();
    match cx.record_type_args(t) {
        Some((k, v)) if k == cx.ty.str_ => holds_string_record(cx, v, &mut seen),
        _ => holds_string_record(cx, t, &mut seen),
    }
}

/// Does `t` mention a type parameter?
fn has_param(cx: &Ctx, t: TyId) -> bool {
    let kind = cx.ty.kind(t);
    matches!(kind, TyKind::Param(_))
        || (crate::types::children(kind).into_iter()).any(|c| has_param(cx, c))
}

/// Does `t` hold a record with string keys: itself, a part of it, or a field of a class or struct
/// in it (the type an `any` converts to)?
pub(super) fn holds_string_record(cx: &Ctx, t: TyId, seen: &mut FxHashSet<TyId>) -> bool {
    if !seen.insert(t) {
        return false;
    }
    if cx.record_type_args(t).is_some_and(|(k, _)| k == cx.ty.str_) {
        return true;
    }
    let kind = cx.ty.kind(t).clone();
    if let TyKind::Adt(d, _) = kind {
        let fields: Vec<TyId> = cx
            .adt(d)
            .map(|a| a.fields.iter().map(|f| f.ty).collect())
            .unwrap_or_default();
        if fields.into_iter().any(|f| holds_string_record(cx, f, seen)) {
            return true;
        }
    }
    (crate::types::children(&kind).into_iter()).any(|c| holds_string_record(cx, c, seen))
}
