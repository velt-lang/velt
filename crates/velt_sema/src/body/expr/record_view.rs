//! Reads of a record viewed with named members (`crate::index_signatures`): a required named
//! member next to `[key: string]: any` (or `unknown`) reads as its declared type, written
//! `r.name` or `r["name"]`.

use velt_common::Span;
use velt_syntax::ast;

use super::record_call::RecordKey;
use crate::body::FnCx;
use crate::hir::{self, TyId};

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
        if let Some(prop) = self.view_key(obj.ty, index) {
            if self.cx.record_view_member(obj.ty, &prop.name).is_some() {
                return self.record_member_read(obj, &prop, span);
            }
        }
        let obj = match self.record_symbol_key_read(obj, index, span) {
            Ok(h) => return h,
            Err(obj) => obj,
        };
        self.record_read(obj, RecordKey::Index(index), span)
    }

    /// Is `key` a required named member of the record view `t` (`r.label += "x"` reads it as
    /// its declared type, so a compound assignment is defined)?
    pub(super) fn record_view_key(&mut self, t: hir::TyId, key: &RecordKey<'_>) -> bool {
        self.record_view_key_ty(t, key).is_some()
    }

    /// The declared type of `key` when it is a required named member of the record view `t`.
    pub(super) fn record_view_key_ty(&mut self, t: hir::TyId, key: &RecordKey<'_>) -> Option<TyId> {
        let name = match key {
            RecordKey::Name(id) => id.name.clone(),
            RecordKey::Index(e) => self.view_key(t, e)?.name,
            RecordKey::Computed(_) => return None,
        };
        self.cx.record_view_member(t, &name)
    }

    /// An object literal where `Record<K, V>` (`d`) is expected, as the type `hint`. For a
    /// record view, the value of a required named member is checked against the member's
    /// declared type (`[]` is a `number[]` for `items: number[]`, a callback's parameters get
    /// their types) before it is stored as a `V`.
    pub(super) fn record_view_literal(
        &mut self,
        d: hir::DefId,
        hint: Option<TyId>,
        (k, v): (TyId, TyId),
        props: &[ast::ObjectProp],
        span: Span,
    ) -> hir::Expr {
        let Some(view) = hint.filter(|t| self.cx.record_view(*t).is_some()) else {
            return self.record_literal(d, k, v, props, span);
        };
        let pre = props
            .iter()
            .map(|p| {
                let (name, value) = match p {
                    ast::ObjectProp::KeyValue(n, value) => (n, Some(value)),
                    ast::ObjectProp::Shorthand(n) => (n, None),
                    _ => return None,
                };
                if crate::symbols::is_computed_key(name) {
                    return None;
                }
                let m = self.cx.record_view_member(view, &name.name)?;
                let h = self.prop_value(name, value, Some(m));
                let h = self.coerce(h, m);
                Some(self.coerce(h, v))
            })
            .collect();
        self.record_literal_with(d, k, v, props, pre, span)
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

impl FnCx<'_, '_> {
    /// The key of `r["name"]` (or `r[k]` with a variable `k` of a string literal type), as the
    /// identifier `r.name` would have, when `t` is a record view.
    fn view_key(&mut self, t: TyId, index: &ast::Expr) -> Option<ast::Ident> {
        self.cx.record_view(t)?;
        if let Some(id) = literal_key(index) {
            return Some(id);
        }
        if !matches!(index.kind, ast::ExprKind::Ident(_)) {
            return None;
        }
        match self.trial_type(index).map(|t| self.cx.ty.kind(t).clone()) {
            Some(hir::TyKind::Literal(hir::LitValue::Str(s))) => Some(ast::Ident {
                name: s.to_string(),
                span: index.span,
            }),
            _ => None,
        }
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
