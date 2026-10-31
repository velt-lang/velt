//! The instances the driver generated, bound to their definitions ([`super::phase`]): the
//! declared generated function and its type arguments identify the instance, the generated
//! function item is its body.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::hir::{DefId, TyId};

/// `(generated function, concrete type arguments)` → the generated definition of its body.
pub(crate) type Bound = FxHashMap<(DefId, Vec<TyId>), DefId>;

/// A generated function and the bodies of its instances share one ABI, which callers were checked
/// against before the bodies were known: no body takes a parameter as its own (one that keeps or
/// moves its argument takes a share of it instead), as for a method in a vtable slot.
pub(crate) fn fix_modes(cx: &mut Ctx) {
    let defs: Vec<DefId> = (cx.gen_binding.bound.iter())
        .flat_map(|((g, _), body)| [*g, *body])
        .collect();
    for d in defs {
        cx.fn_info_mut(d).fixed_modes = true;
    }
}
