//! Place expressions (`Local`, `Field`, `Index`, `UnwrapSome`, `UnwrapVariant`, `Global`): use-mode adjustment
//! after the consumer is known, and mutability of the place's root.

use super::FnCx;
use crate::hir::{self, Callee, ExprKind as H, Intrinsic, LocalId, UseMode};

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
        // A module constant bound by reference (`const v = ns.K`, `Cls.K`, `K!`): one that is a
        // table can't be taken over, like `const v = K`.
        if let H::Global(d) = e.kind {
            if crate::body::pure_init::is_table(self.cx, d) {
                let name = self
                    .cx
                    .global(d)
                    .map(|g| g.name.clone())
                    .unwrap_or_default();
                let msg = format!("cannot move out of module constant `{name}`");
                let err = crate::body::pure_init::table_escapes(&name, msg, e.span);
                self.cx.error(err);
            }
        }
    }

    /// Use place `e` mutably (receiver of a method or argument of an intrinsic known to modify it,
    /// assignment through it):
    /// checks that its root may be mutated and marks the place `BorrowMut`.
    pub fn use_mutably(&mut self, e: &mut hir::Expr, what: &str) {
        if !is_place(e) {
            if let H::Call { args, .. } = &e.kind {
                if Self::is_frozen_view(e) {
                    let (shared, span) = (args[0].clone(), e.span);
                    self.frozen_write(&shared, span);
                }
            }
            return;
        }
        self.require_mutable(e, what);
        set_place_mode(e, UseMode::BorrowMut);
    }

    /// Can the value behind place `e` be modified? Reports an error if not.
    pub fn require_mutable(&mut self, e: &hir::Expr, what: &str) -> bool {
        self.readonly_element_write(e);
        if let H::Call { args, .. } = &root_expr(e).kind {
            if Self::is_frozen_view(root_expr(e)) {
                let shared = args[0].clone();
                self.frozen_write(&shared, e.span);
                return false;
            }
        }
        if let H::Global(d) = root_expr(e).kind {
            let (name, stored, mutable, module) = self
                .cx
                .global(d)
                .map(|g| (g.name.clone(), g.stored, g.mutable, g.module))
                .unwrap_or_default();
            if stored {
                if matches!(e.kind, H::Global(_)) && !mutable && what == "assign to" {
                    self.cx.err(
                        format!("cannot assign to `{name}`, a module `const`"),
                        e.span,
                    );
                    return false;
                }
                // An imported binding is read-only, as in ES modules: its contents are not.
                if matches!(e.kind, H::Global(_)) && module != self.module && what == "assign to" {
                    self.cx.error(
                        velt_common::Diagnostic::error(
                            format!("cannot assign to imported binding `{name}`"),
                            e.span,
                        )
                        .with_note(
                            "an imported `let` is read-only where it is imported, as in JavaScript: export a function that assigns it",
                        ),
                    );
                    return false;
                }
                return true;
            }
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

pub(crate) fn root_expr(e: &hir::Expr) -> &hir::Expr {
    match &e.kind {
        H::Field { base, .. }
        | H::Index { base, .. }
        | H::UnwrapSome(base, _)
        | H::UnwrapVariant { expr: base, .. } => root_expr(base),
        _ => e,
    }
}

/// Is `e` a value read out of a `shared<T>` (`Intrinsic::SharedRead`), or a part of one?
pub(crate) fn frozen_root(e: &hir::Expr) -> bool {
    match &e.kind {
        H::Call {
            callee: Callee::Intrinsic(Intrinsic::SharedRead),
            ..
        } => true,
        H::Field { base, .. }
        | H::Index { base, .. }
        | H::UnwrapSome(base, _)
        | H::UnwrapVariant { expr: base, .. }
        | H::Downcast(base) => frozen_root(base),
        _ => false,
    }
}

/// Is `e` a value read out of a `shared`, a part of one, or what a method of one returns (its
/// elements may be the shared value's, as `slice()`'s are in JavaScript)?
pub(crate) fn frozen_source(cx: &crate::ctx::Ctx, e: &hir::Expr) -> bool {
    match &e.kind {
        H::Call {
            callee: Callee::Def(d, _),
            args,
        } if cx.fn_info(*d).this.is_some() => args.first().is_some_and(|a| frozen_source(cx, a)),
        H::Call {
            callee: Callee::Virtual { .. },
            args,
        } => args.first().is_some_and(|a| frozen_source(cx, a)),
        H::Upcast(x) | H::Downcast(x) => frozen_source(cx, x),
        _ => frozen_root(e),
    }
}
