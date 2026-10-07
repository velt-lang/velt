//! `const x = <place>` where the place's value cannot be moved out — a field of a class instance
//! (`const left = node.left`) or an array element (`const row = grid[i]`) of a non-Copy type —
//! binds `x` by reference, like a `for...of` element, instead of being "cannot move a field out
//! of a class instance". In JS both names see the same object, so reads agree; the exclusive
//! check (`crate::ownership::exclusive`) rejects changing the place while `x` is in scope.
//! `const me = this` in a method or constructor binds `me` by reference too: it names the
//! object `this` points to, and sharing it would make the class reference-counted.

use velt_common::Span;
use velt_syntax::ast;

use super::places::is_place;
use super::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, PatKind as P, StmtKind as S, TyId, UseMode};

impl FnCx<'_, '_> {
    /// Declare `const name = init` by reference when its initializer is such a place; `None`
    /// hands the checked initializer (as a move) back to the ordinary declaration.
    pub(super) fn borrowed_const(
        &mut self,
        v: &ast::VarDecl,
        name: &ast::Ident,
        ann: Option<TyId>,
        span: Span,
        out: &mut Vec<hir::Stmt>,
    ) -> Result<(), Option<hir::Expr>> {
        let Some(e) = v
            .init
            .as_ref()
            .filter(|e| v.kind == ast::VarKind::Const && (is_member_or_index(e) || is_this(e)))
        else {
            return Err(None);
        };
        let mut h = match ann {
            Some(t) => self.expr_coerce(e, t, Want::Borrow),
            None => self.expr(e, None, Want::Borrow),
        };
        let ty = ann.unwrap_or(h.ty);
        // A promise bound by reference to an element or a field would be awaited (moved out of
        // the array or object) later, or shared with the place.
        if self.binds_promise(ty) {
            match h.kind {
                H::Index { .. } => {
                    self.promise_out_of_array(ty, e.span);
                    h = self.error_expr(e.span);
                }
                // Through a class instance (shared): a value type's field moves out as before.
                H::Field { ref base, .. } if self.cx.class_of(base.ty).is_some() => {
                    self.promise_out_of_object(ty, e.span);
                    h = self.error_expr(e.span);
                }
                _ => {}
            }
        }
        let pinned = is_place(&h)
            && !self.cx.is_copy(ty)
            && !self.cx.is_string_value(ty)
            && (self.pinned_place(&h) || self.is_this_local(&h));
        if !pinned {
            match &mut h.kind {
                // A widened promise owns the promise it wraps: that place is moved (and a field
                // or element can't be).
                H::Call {
                    callee: hir::Callee::Intrinsic(hir::Intrinsic::PromiseWiden),
                    args,
                } => self.force_move(&mut args[0]),
                _ => self.force_move(&mut h),
            }
            return Err(Some(h));
        }
        let local = self.declare_local_mut(name, ty, LocalKind::Bind, false);
        self.f.const_refs.insert(local);
        let pat = self.pat(P::Binding(local, UseMode::Borrow), ty, name.span);
        Self::push(out, S::LetPat { pat, init: h }, span);
        Ok(())
    }

    /// Is `h` the method's or constructor's own `this` (an object: `const me = this` names the
    /// same object, so it refers to `this` instead of sharing it; `ownership::exclusive`
    /// makes it a share where the two names are used together)?
    fn is_this_local(&self, h: &hir::Expr) -> bool {
        matches!(h.kind, H::Local(l, _) if self.local_kind(l) == LocalKind::This)
            && self.cx.class_of(h.ty).is_some()
    }

    /// Is place `h` rooted at a local (not a temporary) and does it go through a class instance
    /// or an array element (so its value cannot be moved out)?
    fn pinned_place(&self, h: &hir::Expr) -> bool {
        let mut cur = h;
        let mut pinned = false;
        loop {
            match &cur.kind {
                H::Local(..) => return pinned,
                H::Index { base, .. } => {
                    pinned = true;
                    cur = base;
                }
                H::Field { base, .. } => {
                    pinned |= self.cx.class_of(base.ty).is_some();
                    cur = base;
                }
                H::UnwrapSome(base, _)
                | H::UnwrapVariant { expr: base, .. }
                | H::Downcast(base) => cur = base,
                _ => return false,
            }
        }
    }
}

fn is_this(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::This => true,
        ast::ExprKind::Paren(inner) => is_this(inner),
        _ => false,
    }
}

fn is_member_or_index(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Member {
            optional: false, ..
        }
        | ast::ExprKind::Index {
            optional: false, ..
        } => true,
        ast::ExprKind::Paren(inner) => is_member_or_index(inner),
        _ => false,
    }
}
