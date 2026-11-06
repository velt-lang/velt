//! The bodies checked before `lazy_std::reach`, of a whole program, or of std modules added during
//! the check (`crate::std_load`).

use crate::ctx::Ctx;
use crate::hir::DefId;

/// The bodies checked before [`super::lazy_std::reach`], of the definitions from `from` on.
pub(super) fn check_bodies_from(cx: &mut Ctx, from: u32) {
    super::literal_hosts::find(cx, from);
    let all: Vec<DefId> = (from..cx.info.len() as u32).map(DefId).collect();
    for &d in &all {
        super::driver::field_defaults(cx, d);
    }
    for &d in &all {
        if cx.global(d).is_some() && !super::lazy_std::deferred(cx, d) {
            super::ensure_global(cx, d);
        }
    }
    let fns: Vec<DefId> = cx.fn_defs.iter().copied().filter(|d| d.0 >= from).collect();
    for &d in &fns {
        super::defaults::param_defaults(cx, d);
    }
    super::defaults::iface_defaults(cx, from);
    super::literal_hosts::ensure_all(cx);
    for d in fns {
        if !super::lazy_std::deferred(cx, d) {
            super::ensure_body(cx, d);
        }
    }
}

/// The definitions of std modules added during the check (`crate::std_load`): checked as
/// [`check_bodies`] checks the program's, so what stays deferred is the same as if they had been
/// there from the start. What they reach is `reach`'s, which is running.
pub(crate) fn check_added(cx: &mut Ctx, from: crate::collect::Added) {
    crate::body::expr::virtual_defaults::build_fill_bodies(cx, from.defs);
    check_bodies_from(cx, from.defs);
}
