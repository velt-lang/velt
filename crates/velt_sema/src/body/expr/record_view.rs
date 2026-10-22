//! Reads of a record viewed with named members (`crate::index_signatures`): a required named
//! member next to `[key: string]: any` (or `unknown`) reads as its declared type, written
//! `r.name` or `r["name"]`.

use velt_common::Span;
use velt_syntax::ast;

use super::record_call::RecordKey;
use crate::body::FnCx;
use crate::hir;

impl FnCx<'_, '_> {
    /// `obj.name` of a record (`obj` checked).
    pub(super) fn record_member_read(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        span: Span,
    ) -> hir::Expr {
        match self.record_view_read(obj, prop, span) {
            Ok(h) => h,
            Err(obj) => self.record_read(obj, RecordKey::Name(prop), span),
        }
    }

    /// `obj[index]` of a record (`obj` checked): `obj["name"]` reads as `obj.name` does.
    pub(super) fn record_index_read(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        if let Some(prop) = literal_key(index) {
            if self.cx.record_view_member(obj.ty, &prop.name).is_some() {
                return self.record_member_read(obj, &prop, span);
            }
        }
        self.record_read(obj, RecordKey::Index(index), span)
    }

    /// Is `key` a required named member of the record view `t` (`r.label += "x"` reads it as
    /// its declared type, so a compound assignment is defined)?
    pub(super) fn record_view_key(&self, t: hir::TyId, key: &RecordKey<'_>) -> bool {
        let name = match key {
            RecordKey::Name(id) => id.name.clone(),
            RecordKey::Index(e) => match literal_key(e) {
                Some(id) => id.name,
                None => return false,
            },
            RecordKey::Computed(_) => return false,
        };
        self.cx.record_view_member(t, &name).is_some()
    }

    /// `obj.name` of a required named member of a record view: its declared type, checked at
    /// run time (an index write may have stored another type, as TypeScript allows); `Err`
    /// gives `obj` back for any other read.
    fn record_view_read(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(member) = self.cx.record_view_member(obj.ty, &prop.name) else {
            return Err(obj);
        };
        let obj = self.unview(obj);
        let read = self.record_read(obj, RecordKey::Name(prop), span);
        let read = self.collapse_dynamic_option(read);
        Ok(self.cast_dynamic(read, member))
    }
}

/// The key of `r["name"]`, as the identifier `r.name` would have.
fn literal_key(index: &ast::Expr) -> Option<ast::Ident> {
    match &index.kind {
        ast::ExprKind::Lit(ast::Lit::Str(s)) => Some(ast::Ident {
            name: s.clone(),
            span: index.span,
        }),
        ast::ExprKind::Paren(e) => literal_key(e),
        _ => None,
    }
}
