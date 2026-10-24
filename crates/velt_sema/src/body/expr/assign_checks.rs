//! Checks of assignment targets split out of `assign.rs`: writes to static fields,
//! the field writes `crate::object_copies` needs to know about, and `a ??= b` on a place.

use velt_syntax::ast;

use velt_common::{Diagnostic, Span};

use super::assign::AssignTarget;
use super::assign_paths::is_pure_place;
use crate::body::{FnCx, Want};
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `Type.NAME = v` on a static field: its slot when it is a `static` (module state, as a
    /// module-level `let`), `Some(None)` after reporting a write to a `static readonly` one;
    /// `None` when `Type.NAME` is no static field of `Type`.
    pub(super) fn static_field_target(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
    ) -> Option<Option<hir::Expr>> {
        let ast::ExprKind::Ident(id) = &object.kind else {
            return None;
        };
        if self.is_local_name(&id.name) {
            return None;
        }
        let Some(crate::ctx::Item::Def(d)) = self.lookup_item(&id.name, id.span) else {
            return None;
        };
        let g = *self.cx.adt(d)?.statics.get(&prop.name)?;
        if self.cx.global(g).is_some_and(|g| g.mutable) {
            let place = self.static_field(d, prop, Want::BorrowMut, object.span.to(prop.span))?;
            return Some(Some(place));
        }
        self.cx.err(
            format!(
                "cannot assign to `{}.{}`: it is a `static readonly` field",
                id.name, prop.name
            ),
            prop.span,
        );
        Some(None)
    }

    /// Remember an assignment to field `prop` of an object type (or of a type parameter, which
    /// may be one) for the copies of `crate::object_copies`.
    pub(super) fn note_field_write(&mut self, ty: TyId, prop: &ast::Ident) {
        let ty = match self.cx.ty.kind(ty) {
            TyKind::Param(_) => None,
            _ if self.cx.is_object_type(ty) => Some(ty),
            _ => return,
        };
        self.cx
            .object_copies
            .writes
            .push(crate::object_copies::Write {
                ty,
                name: prop.name.clone(),
                span: prop.span,
                dynamic: false,
            });
    }

    /// `a ??= b` on a place (`checked` is the target as resolved; not a record key).
    pub(super) fn logical_assign(
        &mut self,
        op: ast::BinaryOp,
        checked: Option<AssignTarget>,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        // `a ??= b` is `a = a ?? b` (likewise `&&=`, `||=`), and the assignment narrows `a`
        // as usual. That reads the target twice, which is only right when reading it has no
        // effect (JS evaluates `xs[f()]` once).
        if !is_pure_place(target) {
            self.cx.error(
                Diagnostic::error(
                    "`??=`, `||=` and `&&=` are not supported yet on a target that calls a function",
                    target.span,
                )
                .with_note("store the index or object in a variable first: `const k = f(); m[k] ??= v;`"),
            );
            self.expr(value, None, Want::Borrow);
            return self.error_expr(span);
        }
        // A target in error is reported once, here.
        match checked {
            None => {
                self.expr(value, None, Want::Borrow);
                return self.error_expr(span);
            }
            Some(
                AssignTarget::Place(_)
                | AssignTarget::Record(_)
                | AssignTarget::Setter(_)
                | AssignTarget::Indexer(_)
                | AssignTarget::Dynamic(_)
                | AssignTarget::Keyed(..),
            ) => {}
        }
        let rhs = ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Binary {
                op,
                lhs: Box::new(target.clone()),
                rhs: Box::new(value.clone()),
            },
            span: value.span,
        };
        self.assign(None, target, &rhs, span)
    }
}
