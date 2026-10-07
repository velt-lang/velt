//! Read-modify-write targets evaluated once: `xs[next()] |= 1`, `rows[f()].out += "a"` and
//! `g().out += "a"` bind the indices and the objects that are not places to temporaries
//! first, so the place can be read and then written without evaluating them again (and before
//! the right-hand side, as in JavaScript).

use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, UseMode};

impl FnCx<'_, '_> {
    /// Binds each index of `place` that is not a literal (`xs[next()]`), and each object that
    /// is not itself a place (`f().out`), to a temporary (appended to `stmts` as `let`s), so
    /// that a read-modify-write of the place evaluates them once, before the right-hand side.
    pub(super) fn hoist_indices(&mut self, place: &mut hir::Expr, stmts: &mut Vec<hir::Stmt>) {
        match &mut place.kind {
            H::Field { base, .. } => self.hoist_object(base, stmts),
            H::Index { base, index, .. } => {
                self.hoist_object(base, stmts);
                if matches!(index.kind, H::Lit(_)) {
                    return;
                }
                let (ty, span) = (index.ty, index.span);
                let tmp = self.new_local("<index>", ty, false, span, LocalKind::Temp);
                let init = std::mem::replace(
                    &mut **index,
                    self.mk(H::Local(tmp, UseMode::Copy), ty, span),
                );
                let kind = hir::StmtKind::Let {
                    local: tmp,
                    init: Some(init),
                };
                stmts.push(hir::Stmt { kind, span });
            }
            H::UnwrapSome(base, _) | H::UnwrapVariant { expr: base, .. } | H::Downcast(base) => {
                self.hoist_indices(base, stmts)
            }
            _ => {}
        }
    }

    /// The object of a field or element of a place being updated: its own indices hoisted, or
    /// the whole object when it is a value rather than a place (a call's result).
    fn hoist_object(&mut self, base: &mut hir::Expr, stmts: &mut Vec<hir::Stmt>) {
        if is_place(base) {
            return self.hoist_indices(base, stmts);
        }
        let (ty, span) = (base.ty, base.span);
        let tmp = self.new_local("<object>", ty, true, span, LocalKind::Temp);
        let mode = self.use_mode(ty, Want::BorrowMut);
        let init = std::mem::replace(base, self.mk(H::Local(tmp, mode), ty, span));
        let kind = hir::StmtKind::Let {
            local: tmp,
            init: Some(init),
        };
        stmts.push(hir::Stmt { kind, span });
    }

    /// `e` after the statements `stmts` (as a block when there are any).
    pub(super) fn with_temps(&mut self, mut stmts: Vec<hir::Stmt>, e: hir::Expr) -> hir::Expr {
        if stmts.is_empty() {
            return e;
        }
        let (ty, span) = (e.ty, e.span);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Expr(e),
            span,
        });
        let block = hir::Block {
            stmts,
            value: None,
            span,
        };
        self.mk(H::Block(block), ty, span)
    }

    /// The value `e` after the statements `stmts` (as a block when there are any).
    pub(super) fn with_temps_value(&mut self, stmts: Vec<hir::Stmt>, e: hir::Expr) -> hir::Expr {
        if stmts.is_empty() {
            return e;
        }
        let (ty, span) = (e.ty, e.span);
        let block = hir::Block {
            stmts,
            value: Some(Box::new(e)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }

    /// Does `place` go through an object that is a value rather than a place (`f().n`)? A
    /// compound assignment reads and writes such a place separately, so it is hoisted first;
    /// indices alone are evaluated once by `CompoundAssign` itself.
    pub(super) fn has_value_object(place: &hir::Expr) -> bool {
        match &place.kind {
            H::Field { base, .. } | H::Index { base, .. } => {
                !is_place(base) || Self::has_value_object(base)
            }
            H::UnwrapSome(base, _) | H::UnwrapVariant { expr: base, .. } | H::Downcast(base) => {
                Self::has_value_object(base)
            }
            _ => false,
        }
    }
}
