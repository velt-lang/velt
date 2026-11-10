//! The float hooks of std's number printing, checked on demand (velt_common
//! `std_hooks::ON_DEMAND_FLOAT`): reached once the program's own code converts to text a value
//! that may hold a float. `texts` are the types of the values it converts (`ToString`,
//! `TemplateToString`) in the program's code and in live classes' field defaults, walked since the
//! last call. Std's function bodies don't count: their conversions would make every program that
//! reaches one check the hooks. `console.log`'s float writer (`ON_DEMAND_WRITE`) is reached once
//! live code prints a float ([`reach_writer`]). The integer hooks are demanded by the constructs of
//! live code instead (`live.rs`).

use crate::ctx::Ctx;
use crate::hir::{self, DefId, TyId, TyKind};
use rustc_hash::FxHashSet;

/// Adds the on-demand hooks that `texts` now need to `found`.
pub(super) fn reach_hooks(cx: &mut Ctx, texts: Vec<TyId>, found: &mut Vec<DefId>) {
    // Too late to add a module (`Ctx::demand_std`), and a float hook has the runtime's printer to
    // fall back on, as in std's own code.
    if cx.lazy.float_text
        || cx.lazy.live.finishing
        || !(texts.iter()).any(|&t| may_hold_float(cx, t, &mut FxHashSet::default(), 0))
    {
        return;
    }
    let float_hooks = velt_common::std_hooks::HOOKS.iter();
    for h in float_hooks.filter(|h| velt_common::std_hooks::ON_DEMAND_FLOAT.contains(&h.name)) {
        cx.demand_std(&h.module());
        if let Some(crate::ctx::Item::Def(d)) = cx.std_def(&h.qualified()) {
            found.push(d);
        }
    }
    cx.lazy.float_text = true;
}

/// Adds `console.log`'s float writer to `found` once live code prints a float: it demands no
/// integer hook, and lowering calls the runtime's printer where it is unchecked, as when the float
/// is found too late to add a module.
pub(super) fn reach_writer(cx: &mut Ctx, found: &mut Vec<DefId>) {
    if cx.lazy.float_write || cx.lazy.live.finishing || !cx.lazy.live.prints_float {
        return;
    }
    let hooks = velt_common::std_hooks::HOOKS.iter();
    for h in hooks.filter(|h| velt_common::std_hooks::ON_DEMAND_WRITE.contains(&h.name)) {
        cx.demand_std(&h.module());
        if let Some(crate::ctx::Item::Def(d)) = cx.std_def(&h.qualified()) {
            found.push(d);
        }
    }
    cx.lazy.float_write = true;
}

/// How deep [`may_hold_float`] looks: past it, a type counts as able to (a generic alias can expand
/// to a new type at every level).
const MAX_DEPTH: u32 = 32;

/// Can a value of type `t` hold a float, so that its text may need std's float printing? Integers,
/// booleans, strings, symbols, `null`, enums without payloads (a string enum prints its string, any
/// other its integer) and unions, arrays, tuples and options of those are known not to. Anything
/// else (an object, whose class may have a subclass with a float field, `any`, a type parameter) may,
/// and is printed through glue that can reach a float. A wrong `false` costs no text: what stays
/// unchecked falls back to the runtime's printer, which prints the same.
fn may_hold_float(cx: &mut Ctx, t: TyId, seen: &mut FxHashSet<TyId>, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    // A type seen already adds nothing: what its parts can hold is being answered.
    if !seen.insert(t) {
        return false;
    }
    let parts: Vec<TyId> = match cx.ty.kind(t) {
        TyKind::Int(_) | TyKind::Bool | TyKind::Str | TyKind::Symbol | TyKind::Unit => {
            return false
        }
        TyKind::Never => return false,
        TyKind::Literal(lit) => return matches!(lit, hir::LitValue::Float(..)),
        TyKind::Array(e) | TyKind::Option(e, _) | TyKind::Shared(e) => vec![*e],
        TyKind::Tuple(es) => es.clone(),
        TyKind::Adt(d, _) => {
            let d = *d;
            match cx.union_members(t) {
                Some(members) => members,
                None => match cx.enum_info(d) {
                    Some(e) if e.variants.iter().all(|v| v.payload.is_empty()) => return false,
                    _ => return true,
                },
            }
        }
        _ => return true,
    };
    parts
        .into_iter()
        .any(|p| may_hold_float(cx, p, seen, depth + 1))
}
