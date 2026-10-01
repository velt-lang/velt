//! Place expressions (`Local`, `Field`, `Index`, `UnwrapSome`, `UnwrapVariant`, `Global`): use-mode adjustment
//! after the consumer is known, and mutability of the place's root.

use velt_common::Diagnostic;

use super::{FnCx, LocalKind};
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
    )
}

/// The local a place is rooted at (through projections), if any.
pub(crate) fn place_root(e: &hir::Expr) -> Option<LocalId> {
    match &e.kind {
        H::Local(l, _) => Some(*l),
        H::Field { base, .. }
        | H::Index { base, .. }
        | H::UnwrapSome(base, _)
        | H::UnwrapVariant { expr: base, .. } => place_root(base),
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
        let name = self.f.locals[l.0 as usize].name.clone();
        let span = root_expr(e).span;
        // Params and `this` may always be modified: whether the caller's value is (a mutable
        // borrow) is inferred afterwards (`crate::ownership`).
        let (ok, note): (bool, String) = match self.local_kind(l) {
            LocalKind::Let
            | LocalKind::Const
            | LocalKind::Using
            | LocalKind::Temp
            | LocalKind::Capture
            | LocalKind::Param
            | LocalKind::This => (true, String::new()),
            LocalKind::Bind if self.f.const_refs.contains(&l) => (
                false,
                format!("`{name}` refers to a class field or array element in place and is read-only; modify that place directly"),
            ),
            LocalKind::Bind => (
                self.f.locals[l.0 as usize].mutable,
                "pattern bindings of a `match` are read-only".to_string(),
            ),
            LocalKind::Elem => (
                self.f.locals[l.0 as usize].mutable,
                "`for...of` borrows the array's elements; index the array to modify them"
                    .to_string(),
            ),
        };
        if !ok {
            self.cx
                .error(Diagnostic::error(format!("cannot {what} `{name}`"), span).with_note(note));
            return false;
        }
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
