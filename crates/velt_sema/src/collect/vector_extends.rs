//! The `extend` blocks of built-in vector and mask types (`extend u8x16 { … }`, velt/simd) that a
//! std module added in the middle of the check brings (`super::collect_added`). velt_loader's
//! (a1) rule lets such a block in (`std_index/rule.rs`): it only adds methods to a type no module
//! declares. It changes code checked before it only where that code looked up a method of the
//! same name on the vector type through the `extend` blocks: a call or an operator (whose method
//! is looked up) that an earlier block answered and the new, exact block now takes over or makes
//! ambiguous (`FnCx::extension_method` takes the most specific), or one that nothing answered and
//! the new block would (a program holding a vector through `velt/arch/x86`'s `Vector256`, which
//! loads only part of velt/simd). Those lookups are recorded, found or not
//! (`ExtensionLookup::vector_lookups`); when one is for a name the new block declares, the module
//! is refused, as velt_loader refuses a module that breaks the rule: the check stops, and the
//! driver checks again with it up front (`StdLoads::refuse_served`). A clash no lookup went
//! through changes nothing: the up-front check picks the new block too.

use crate::ctx::Ctx;
use crate::hir::TyKind;

pub(super) fn check(cx: &mut Ctx, from: super::Added) {
    for i in from.extensions..cx.extensions.len() {
        let target = cx.extensions[i].target;
        if !matches!(cx.ty.kind(target), TyKind::Simd(_)) {
            continue;
        }
        let looked_up = cx.extensions[i].methods.keys().any(|name| {
            cx.ext_lookup
                .vector_lookups
                .contains(&(target, name.clone()))
        });
        if !looked_up {
            continue;
        }
        let m = *cx.extensions[i]
            .methods
            .values()
            .next()
            .expect("a block with a method");
        let path = cx.modules[cx.fn_info(m.def).module].path.clone();
        match cx.loads {
            Some(l) => l.kept.refuse_served(&path),
            // Only a check that loads in the middle of it adds modules (`collect_added`).
            None => panic!("ICE: std module `{path}` added to a check that loads nothing"),
        }
        return;
    }
}
