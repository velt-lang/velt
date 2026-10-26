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
    pub(super) fn hoist_object(&mut self, base: &mut hir::Expr, stmts: &mut Vec<hir::Stmt>) {
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

    /// Binds a share of the string `cur` (a read of the place being updated) to a temporary
    /// (appended to `stmts`), and returns that temporary: the read happens there, before the
    /// right-hand side runs.
    pub(super) fn hoist_value(&mut self, cur: hir::Expr, stmts: &mut Vec<hir::Stmt>) -> hir::Expr {
        let (ty, span) = (cur.ty, cur.span);
        let tmp = self.new_local("<current>", ty, false, span, LocalKind::Temp);
        let init = self.intrinsic(hir::Intrinsic::Share, vec![cur], ty, span);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: tmp,
                init: Some(init),
            },
            span,
        });
        self.mk(H::Local(tmp, UseMode::Move), ty, span)
    }

    /// `st.b.items[0] = change()`: the object `place` writes into goes through a narrowed field
    /// path, which lowering checks where it is read. JavaScript evaluates the object before the
    /// value `v`, so a value that clears the field still writes into the object it held (#1280):
    /// when `v` may change memory, the nearest object that holds the written place and is shared
    /// by reference (`st.b.ts` for `st.b.ts[0][1]`, a tuple in an array) is bound first (another
    /// reference to it), then the indices (unless `stmts` binds them already: the object, which
    /// may use them, goes after them). A tuple right behind the checked path is a value and
    /// stays checked where it is written (`st.t[0] = change()`).
    pub(super) fn hoist_checked_object(
        &mut self,
        place: &mut hir::Expr,
        v: &hir::Expr,
        stmts: &mut Vec<hir::Stmt>,
    ) {
        let (H::Field { base, .. } | H::Index { base, .. }) = &mut place.kind else {
            return;
        };
        if !through_checked_path(base) || !crate::effects::may_change_memory(v) {
            return;
        }
        let first = stmts.is_empty();
        if self.bind_shared_object(base, stmts) && first {
            self.hoist_indices(place, stmts);
        }
    }

    /// Bind the nearest object at or below `e` that is shared by reference and has the checked
    /// path inside it (`hoist_checked_object`); `false` when there is none.
    fn bind_shared_object(&mut self, e: &mut hir::Expr, stmts: &mut Vec<hir::Stmt>) -> bool {
        if !through_checked_path(e) {
            return false;
        }
        if !self.shared_object(e.ty) {
            return match &mut e.kind {
                H::Field { base, .. }
                | H::Index { base, .. }
                | H::UnwrapSome(base, _)
                | H::UnwrapVariant { expr: base, .. }
                | H::Downcast(base) => self.bind_shared_object(base, stmts),
                _ => false,
            };
        }
        let (ty, span) = (e.ty, e.span);
        let tmp = self.new_local("<object>", ty, true, span, LocalKind::Temp);
        let mode = self.use_mode(ty, Want::BorrowMut);
        let init = std::mem::replace(e, self.mk(H::Local(tmp, mode), ty, span));
        let init = self.intrinsic(hir::Intrinsic::Share, vec![init], ty, span);
        let kind = hir::StmtKind::Let {
            local: tmp,
            init: Some(init),
        };
        stmts.push(hir::Stmt { kind, span });
        true
    }

    /// Is a value of type `t` an object another reference shares: an array, a class instance,
    /// a struct or an object type (not a tuple, a union or a primitive)?
    fn shared_object(&self, t: hir::TyId) -> bool {
        match self.cx.ty.kind(t) {
            hir::TyKind::Array(_) => true,
            hir::TyKind::Adt(d, _) => self.cx.adt(*d).is_some() && self.cx.union_def(t).is_none(),
            _ => false,
        }
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

    /// Does `place` go through an array element or an object that is a value (`rows[i].out`,
    /// `f().out`), where a call may reallocate or replace what holds it?
    pub(super) fn through_element(place: &hir::Expr) -> bool {
        match &place.kind {
            H::Index { .. } => true,
            H::Field { base, .. } => !is_place(base) || Self::through_element(base),
            H::UnwrapSome(base, _) | H::UnwrapVariant { expr: base, .. } | H::Downcast(base) => {
                Self::through_element(base)
            }
            _ => false,
        }
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

/// Does place `e` read a payload or member that lowering checks again: one of a narrowed field
/// path, not of a local (`check_narrowed_field`)?
fn through_checked_path(e: &hir::Expr) -> bool {
    match &e.kind {
        H::UnwrapSome(base, _) | H::UnwrapVariant { expr: base, .. } => {
            !matches!(base.kind, H::Local(..)) || through_checked_path(base)
        }
        H::Field { base, .. } | H::Index { base, .. } | H::Downcast(base) => {
            through_checked_path(base)
        }
        _ => false,
    }
}
