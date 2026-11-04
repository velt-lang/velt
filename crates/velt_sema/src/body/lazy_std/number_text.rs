//! The float hooks of std's number printing, checked on demand (velt_common
//! `std_hooks::ON_DEMAND_FLOAT`): reached once the program's own code converts to text a value
//! that may hold a float. `texts` are the types of the values it converts (`ToString`,
//! `TemplateToString`) in the code scanned since the last call, std's own excepted (the prelude
//! is checked whatever the program uses, and its conversions would make every program check the
//! hooks). The integer hooks are demanded by the constructs of live code instead (`live.rs`).

use crate::ctx::Ctx;
use crate::hir::{self, DefId, TyId, TyKind};

/// Adds the on-demand hooks that `texts` now need to `found`.
pub(super) fn reach_hooks(cx: &mut Ctx, texts: Vec<TyId>, found: &mut Vec<DefId>) {
    if cx.lazy.float_text || !texts.iter().any(|&t| may_hold_float(cx, t)) {
        return;
    }
    let float_hooks = velt_common::std_hooks::HOOKS.iter();
    for h in float_hooks.filter(|h| velt_common::std_hooks::ON_DEMAND_FLOAT.contains(&h.name)) {
        if let Some(crate::ctx::Item::Def(d)) = cx.std_def(&h.qualified()) {
            found.push(d);
        }
    }
    cx.lazy.float_text = true;
}

/// Can a value of type `t` hold a float, so that its text may need std's float printing? Only
/// integers, booleans and strings are known not to; anything else (an object, an array, `any`,
/// a type parameter) may, and is printed through glue that can reach a float.
fn may_hold_float(cx: &Ctx, t: TyId) -> bool {
    !matches!(
        cx.ty.kind(t),
        TyKind::Int(_)
            | TyKind::Bool
            | TyKind::Str
            | TyKind::Literal(
                hir::LitValue::Int(..) | hir::LitValue::Bool(_) | hir::LitValue::Str(_)
            )
    )
}
