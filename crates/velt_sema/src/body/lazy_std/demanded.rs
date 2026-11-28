//! The hooks lazy std demands a module for, found or reported (#3745): a hook whose module the
//! program has but that the module doesn't define is a std or loader bug (a renamed hook, a module
//! loaded under a second name), an internal error at the point of the demand. Dropping it, as a
//! `filter_map` over the hook list did, left lowering to fail far from the cause.

use crate::ctx::Ctx;
use crate::hir::DefId;
use velt_common::Span;

/// The function hook `name` (qualified: `std/lang/hooks/sched::schedStart`) after its module was
/// demanded: `None` where the program doesn't have the module (no std, a load refused or out of
/// reach); its module there without it is an `ICE:` error naming both.
pub(super) fn hook(cx: &mut Ctx, name: &str) -> Option<DefId> {
    if let Some(d) = cx.prelude_fn(name) {
        return Some(d);
    }
    let module = name.split_once("::").map_or(name, |(m, _)| m);
    if crate::collect::exports::std_module(cx, module).is_some() {
        cx.err(
            format!("ICE: std hook `{name}` demanded, but its module `{module}` doesn't define it"),
            Span::DUMMY,
        );
    }
    None
}

/// [`hook`] of each of `names`, the ones found added to `found`.
pub(super) fn hooks(cx: &mut Ctx, names: &[&str], found: &mut Vec<DefId>) {
    for name in names {
        found.extend(hook(cx, name));
    }
}
