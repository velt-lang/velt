//! The bodies of generated modules (docs/internals/design/source-generators.md), checked after
//! the program's (`crate::generated::phase`).

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::DefId;

/// Check the definitions of generated modules collected after the other bodies
/// (`collect::collect_generated`), and what they reach of std's deferred definitions.
pub(crate) fn check_generated(cx: &mut Ctx, defs: &[DefId]) {
    for &d in defs {
        if cx.global(d).is_some() {
            super::ensure_global(cx, d);
        }
    }
    for &d in defs {
        if matches!(cx.info[d.0 as usize], DefInfo::Fn(_)) {
            super::defaults::param_defaults(cx, d);
        }
    }
    for &d in defs {
        if matches!(cx.info[d.0 as usize], DefInfo::Fn(_)) {
            super::ensure_body(cx, d);
        }
    }
    super::lazy_std::reach(cx);
    super::returns::check_deferred(cx);
}
