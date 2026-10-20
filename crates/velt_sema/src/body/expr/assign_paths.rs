//! Assignment targets (`assign.rs`): those that can be read again without an effect, and
//! writes to readonly fields.

use velt_syntax::ast;

use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, TyId};

impl FnCx<'_, '_> {
    pub(super) fn check_readonly(&mut self, place: &hir::Expr, prop: &ast::Ident) {
        let H::Field { base, index, .. } = &place.kind else {
            return;
        };
        let Some((d, _)) = self.adt_of(base.ty) else {
            return;
        };
        let readonly = self
            .cx
            .adt(d)
            .and_then(|a| a.fields.get(*index as usize))
            .is_some_and(|f| f.readonly);
        if readonly && self.is_import_meta_type(base.ty) {
            // `import.meta.url = …` is reported as a write to the module constant; a write
            // through a copy (`const m = import.meta; m.url = …`) the same way, as in Node the
            // copy is `import.meta` itself.
            if !matches!(crate::body::places::root_expr(base).kind, H::Global(_)) {
                self.cx.err(
                    format!(
                        "cannot assign to a field of module-level constant `{}`",
                        ast::IMPORT_META
                    ),
                    place.span,
                );
            }
            return;
        }
        let in_ctor = self.f.kind == crate::defs::FnKind::Ctor
            && matches!(base.kind, H::Local(l, _) if self.local_kind(l) == LocalKind::This);
        if readonly && !in_ctor {
            self.cx.err(
                format!("cannot assign to `{}`: it is a readonly field", prop.name),
                prop.span,
            );
        }
    }

    /// Is `ty` the type of this module's `import.meta` (`ast::IMPORT_META`)?
    fn is_import_meta_type(&mut self, ty: TyId) -> bool {
        let Some(crate::ctx::Item::Def(d)) = self.cx.lookup_item(self.module, ast::IMPORT_META)
        else {
            return false;
        };
        if !matches!(self.cx.info[d.0 as usize], crate::defs::DefInfo::Global(_)) {
            return false;
        }
        crate::body::driver::ensure_global(self.cx, d);
        self.cx.global(d).is_some_and(|g| g.ty == ty)
    }
}

/// `x`, `this.a`, `x.a.b`: re-reading it has no side effects.
/// A place that reading again has no effect: names, `this`, member paths, and indexes by such
/// values, literals and arithmetic on them (`xs[i + 1].count`).
pub(super) fn is_pure_place(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Index {
            object,
            index,
            optional: false,
        } => is_pure_place(object) && is_pure_value(index),
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_pure_place(object),
        ast::ExprKind::Paren(inner) => is_pure_place(inner),
        _ => is_plain_path(e),
    }
}

pub(super) fn is_pure_value(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(_) => true,
        ast::ExprKind::Unary { expr, .. } => is_pure_value(expr),
        ast::ExprKind::Binary { lhs, rhs, .. } => is_pure_value(lhs) && is_pure_value(rhs),
        _ => is_pure_place(e),
    }
}

pub(super) fn is_plain_path(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_plain_path(object),
        ast::ExprKind::Paren(inner) => is_plain_path(inner),
        _ => false,
    }
}
