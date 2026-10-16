//! Checks of assignment targets split out of `assign.rs`: writes to `static readonly` fields, and
//! the field writes `crate::object_copies` needs to know about.

use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// `Type.NAME = v` on a `static readonly` field (reported; statics are constants).
    pub(super) fn static_field_target(&mut self, object: &ast::Expr, prop: &ast::Ident) -> bool {
        let ast::ExprKind::Ident(id) = &object.kind else {
            return false;
        };
        if self.is_local_name(&id.name) {
            return false;
        }
        let Some(crate::ctx::Item::Def(d)) = self.lookup_item(&id.name, id.span) else {
            return false;
        };
        if !self
            .cx
            .adt(d)
            .is_some_and(|a| a.statics.contains_key(&prop.name))
        {
            return false;
        }
        self.cx.err(
            format!(
                "cannot assign to `{}.{}`: static fields are readonly",
                id.name, prop.name
            ),
            prop.span,
        );
        true
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
            });
    }
}
