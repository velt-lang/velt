//! Place expressions (`Local`, `Field`, `Index`, `UnwrapSome`, `UnwrapVariant`, `Global`): use-mode adjustment
//! after the consumer is known, and mutability of the place's root.

use super::FnCx;
use crate::hir::{self, ExprKind as H, LocalId, UseMode};

pub(crate) fn is_place(e: &hir::Expr) -> bool {
    matches!(
        e.kind,
        H::Local(..)
            | H::Field { .. }
            | H::Index { .. }
            | H::UnwrapSome(..)
            | H::UnwrapVariant { .. }
            | H::Global(_)
    ) || matches!(&e.kind, H::Downcast(x) if is_place(x))
}

/// A variable, constant or field path of one: reading it again has no effect (unlike a getter,
/// which is a call).
pub(crate) fn is_path(e: &hir::Expr) -> bool {
    match &e.kind {
        H::Local(..) | H::Global(_) => true,
        H::Field { base, .. } => is_path(base),
        _ => false,
    }
}

/// The local a place is rooted at (through projections), if any.
pub(crate) fn place_root(e: &hir::Expr) -> Option<LocalId> {
    match &e.kind {
        H::Local(l, _) => Some(*l),
        H::Field { base, .. }
        | H::Index { base, .. }
        | H::UnwrapSome(base, _)
        | H::UnwrapVariant { expr: base, .. }
        | H::Downcast(base) => place_root(base),
        _ => None,
    }
}

/// Set the use mode of a place's outermost node; projection bases become `Borrow`
/// (`BorrowMut` when the projection is mutably used). Non-places are left alone.
pub(crate) fn set_place_mode(e: &mut hir::Expr, m: UseMode) {
    let base_mode = if m == UseMode::BorrowMut {
        UseMode::BorrowMut
    } else {
        UseMode::Borrow
    };
    match &mut e.kind {
        H::Local(_, mode) => *mode = m,
        H::Field { base, mode, .. } | H::Index { base, mode, .. } => {
            *mode = m;
            set_place_mode(base, base_mode);
        }
        H::UnwrapSome(base, mode)
        | H::UnwrapVariant {
            expr: base, mode, ..
        } => {
            *mode = m;
            set_place_mode(base, base_mode);
        }
        // The same object (or the object of an interface value): the mode of the whole.
        H::Downcast(x) => set_place_mode(x, m),
        _ => {}
    }
}

impl FnCx<'_, '_> {
    /// The value of place `e` is consumed: `Move` (or `Copy` for Copy types).
    pub fn force_move(&mut self, e: &mut hir::Expr) {
        let m = if self.cx.is_copy(e.ty) {
            UseMode::Copy
        } else {
            UseMode::Move
        };
        if is_place(e) {
            set_place_mode(e, m);
        }
    }

    /// Use place `e` mutably (receiver of a method or argument of an intrinsic known to modify it,
    /// assignment through it):
    /// checks that its root may be mutated and marks the place `BorrowMut`.
    pub fn use_mutably(&mut self, e: &mut hir::Expr, what: &str) {
        if !is_place(e) {
            return;
        }
        self.require_mutable(e, what);
        set_place_mode(e, UseMode::BorrowMut);
    }

    /// Can the value behind place `e` be modified? Reports an error if not.
    pub fn require_mutable(&mut self, e: &hir::Expr, what: &str) -> bool {
        if let H::Global(d) = root_expr(e).kind {
            let name = self
                .cx
                .global(d)
                .map(|g| g.name.clone())
                .unwrap_or_default();
            self.cx.err(
                format!("cannot {what} module-level constant `{name}`"),
                e.span,
            );
            return false;
        }
        let Some(l) = place_root(e) else {
            return true;
        };
        let span = root_expr(e).span;
        // Every binding may be modified through, like in JS (semantics stage 2): params and
        // `this` (whether the caller's value is modified is inferred afterwards,
        // `crate::ownership`), and pattern / `for...of` / by-reference `const` bindings, which
        // point into the place they were bound from (`crate::ownership::evidence`).
        self.mark_mutated(l, span);
        true
    }
}

fn root_expr(e: &hir::Expr) -> &hir::Expr {
    match &e.kind {
        H::Field { base, .. }
        | H::Index { base, .. }
        | H::UnwrapSome(base, _)
        | H::UnwrapVariant { expr: base, .. } => root_expr(base),
        _ => e,
    }
}
