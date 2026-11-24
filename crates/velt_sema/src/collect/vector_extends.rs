//! The `extend` blocks of built-in vector and mask types (`extend u8x16 { … }`, velt/simd) that a
//! std module added in the middle of the check brings (`super::collect_added`). velt_loader's
//! (a1) rule lets such a block in (`std_index/rule.rs`): it only adds methods to a type no module
//! declares, so code checked before it found none of them, provided none of its methods is one the
//! type already had. A method of the same name in an earlier block that applies to the type
//! would now lose to the new, more specific one, and a call already checked against the old one
//! would change (`FnCx::extension_method` takes the most specific block). That is checked here: a
//! std module that does it is an internal error.

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::hir::TyKind;

pub(super) fn check(cx: &mut Ctx, from: super::Added) {
    for i in from.extensions..cx.extensions.len() {
        let target = cx.extensions[i].target;
        if !matches!(cx.ty.kind(target), TyKind::Simd(_)) {
            continue;
        }
        let mut names: Vec<String> = cx.extensions[i].methods.keys().cloned().collect();
        names.sort();
        for name in names {
            let Some(earlier) = (0..from.extensions)
                .find(|&j| cx.extensions[j].methods.contains_key(&name) && applies(cx, j, target))
            else {
                continue;
            };
            let m = cx.extensions[i].methods[&name];
            let old = cx.extensions[earlier].methods[&name];
            let ty = cx.display(target);
            let d = Diagnostic::error(
                format!(
                    "ICE: a std module added during the check gives `{ty}` a method `{name}` it \
                     already had: calls checked before it would change"
                ),
                cx.fn_info(m.def).name_span,
            )
            .with_label(cx.fn_info(old.def).name_span, "the method it would replace");
            cx.error(d);
        }
    }
}

/// Whether extension `j`'s target pattern applies to `t`.
fn applies(cx: &mut Ctx, j: usize, t: crate::hir::TyId) -> bool {
    let pattern = cx.extensions[j].target;
    let mut slots = vec![None; cx.extensions[j].generics.names.len()];
    cx.match_ty(pattern, t, &mut slots)
}
