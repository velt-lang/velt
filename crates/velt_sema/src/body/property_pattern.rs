//! Object patterns that read properties rather than fields (`const { length } = xs;`, a getter
//! of a class): JavaScript's destructuring reads each key as a property, so a value whose type
//! lacks one of the pattern's keys as a field is taken apart key by key, each binding declared
//! from `value.key` as a property read would produce it.
//!
//! A value that is a variable or a field path of one is read in place for each key; any other
//! value is first held in a hidden temporary, so it is evaluated once, as in JS.

use velt_syntax::ast;

use super::pattern::BindCtx;
use super::places::is_place;
use super::{FnCx, Want};
use crate::hir::{self, ExprKind as H, StmtKind as S, UseMode};

impl FnCx<'_, '_> {
    /// Does destructuring a value of type `ty` with the object pattern `fields` need property
    /// reads (a key that is not a field: `length`, a getter)?
    pub(super) fn reads_properties(
        &mut self,
        ty: hir::TyId,
        fields: &[(ast::Ident, ast::Pattern)],
    ) -> bool {
        let t = &self.cx.ty;
        if t.is_bottom(ty) || t.opt_payload(ty).is_some() {
            return false;
        }
        fields
            .iter()
            .any(|(key, _)| self.field_of(ty, &key.name).is_none())
    }

    /// `kind { k1: p1, k2: p2 } = init;` as `kind p1 = init.k1; kind p2 = init.k2;`.
    pub(super) fn property_decls(
        &mut self,
        kind: ast::VarKind,
        fields: &[(ast::Ident, ast::Pattern)],
        init: hir::Expr,
        out: &mut Vec<hir::Stmt>,
    ) {
        let obj = if is_path(&init) {
            init
        } else {
            let span = init.span;
            let name = ast::Ident {
                name: format!("#d{}", self.f.locals.len()),
                span,
            };
            let ty = init.ty;
            let l = self.hidden_local(name, init, false, out);
            self.mk(H::Local(l, UseMode::Borrow), ty, span)
        };
        let mutable = kind == ast::VarKind::Let;
        for (key, sub) in fields {
            let value = self.member_of(obj.clone(), key, Want::Borrow, key.span);
            let value = self.inferred_local_init(value);
            let ctx = BindCtx::Let {
                mutable,
                place: is_place(&value),
            };
            let pat = self.pattern(sub, value.ty, ctx);
            if let hir::PatKind::Binding(l, _) = pat.kind {
                self.note_inferred_local(l, &value);
            }
            out.push(hir::Stmt {
                kind: S::LetPat { pat, init: value },
                span: sub.span,
            });
        }
    }
}

/// A variable, constant or field path of one: reading it again has no effect.
fn is_path(e: &hir::Expr) -> bool {
    match &e.kind {
        H::Local(..) | H::Global(_) => true,
        H::Field { base, .. } => is_path(base),
        _ => false,
    }
}
