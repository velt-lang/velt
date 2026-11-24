//! The `extend` blocks of built-in vector and mask types (`extend u8x16 { … }`, velt/simd) that a
//! std module added in the middle of the check brings (`super::collect_added`). velt_loader's
//! (a1) rule lets such a block in (`std_index/rule.rs`): it only adds methods to a type no module
//! declares. It changes code checked before it only where that code called a method of the same
//! name on the vector type through an earlier block (a blanket `extend<T> T` of the program's,
//! say) that the new, more specific block now takes over (`FnCx::extension_method` takes the most
//! specific). The calls the check resolved that way are recorded (`Ctx::ext_lookup`); when
//! one would change, the module is refused, as velt_loader refuses a module that breaks the rule:
//! the check stops, and the driver checks again with it up front (`StdLoads::refuse_served`).
//! A clash no call went through changes nothing: the up-front check picks the new block too.

use crate::ctx::Ctx;
use crate::defs::Bound;
use crate::hir::{TyId, TyKind};

pub(super) fn check(cx: &mut Ctx, from: super::Added) {
    for i in from.extensions..cx.extensions.len() {
        let target = cx.extensions[i].target;
        if !matches!(cx.ty.kind(target), TyKind::Simd(_)) {
            continue;
        }
        let names: Vec<String> = cx.extensions[i].methods.keys().cloned().collect();
        let taken_over = names.iter().any(|name| {
            (0..from.extensions).any(|j| {
                cx.ext_lookup.vector_calls.contains(&(j, target))
                    && cx.extensions[j].methods.contains_key(name)
                    && applies(cx, j, target)
            })
        });
        if !taken_over {
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

/// Whether extension `j` applies to `t`, as a call decides (`FnCx::applicable_extensions`): its
/// target pattern has an instance equal to `t`, and its bounds hold for the type arguments that
/// make it.
fn applies(cx: &mut Ctx, j: usize, t: TyId) -> bool {
    let pattern = cx.extensions[j].target;
    let mut slots = vec![None; cx.extensions[j].generics.names.len()];
    if !cx.match_ty(pattern, t, &mut slots) || cx.subst_known(pattern, &slots) != t {
        return false;
    }
    let owner: Vec<TyId> = slots.iter().map(|s| s.unwrap_or(cx.ty.error)).collect();
    let bounds = cx.extensions[j].generics.bounds.clone();
    bounds.iter().zip(&owner).all(|(bs, &arg)| {
        bs.iter().all(|b| {
            let b = Bound {
                iface: b.iface,
                args: b.args.iter().map(|a| cx.subst(*a, &owner)).collect(),
            };
            cx.satisfies(arg, &b, &[])
        })
    })
}
