//! Unsafe methods behind safe ones (docs/internals/design/c-ffi.md): a call through an
//! interface or a base class checks the method it names, so an unsafe method may not implement
//! an interface method (interfaces have no unsafe methods) nor override a safe one, or the
//! call would reach unsafe code without an unsafe context.

use velt_common::{Diagnostic, Span};

use crate::body::unsafe_fn as is_unsafe_fn;
use crate::ctx::Ctx;
use crate::hir::DefId;

/// Method `def` implements `what` (`` `Op.run` ``): an unsafe method can't.
pub(super) fn implements(cx: &mut Ctx, def: DefId, what: &str) {
    if is_unsafe_fn(cx, def) {
        let span = cx.fn_info(def).name_span;
        reject(cx, span, what, "interface methods are never unsafe");
    }
}

/// Method `def` overrides `base`: an unsafe method can only override an unsafe one.
pub(super) fn overrides(cx: &mut Ctx, def: DefId, base: DefId) {
    if is_unsafe_fn(cx, def) && !is_unsafe_fn(cx, base) {
        let span = cx.fn_info(def).name_span;
        let what = format!("`{}`", cx.fn_info(base).name);
        reject(cx, span, &what, "the base class method is safe");
    }
}

fn reject(cx: &mut Ctx, span: Span, what: &str, why: &str) {
    cx.error(
        Diagnostic::error(
            format!("an unsafe method can't stand in for {what}: {why}"),
            span,
        )
        .with_note(
            "a call through the safe method would need no `unsafe` block: make this method safe and put the unsafe code in an `unsafe { … }` block inside it",
        ),
    );
}
