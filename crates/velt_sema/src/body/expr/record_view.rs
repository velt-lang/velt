//! Reads of a record viewed with named members (`crate::index_signatures`): a named member reads
//! as its declared type (next to `any` or `unknown` values, a required one), written `r.name` or
//! `r["name"]`.

use velt_common::Span;
use velt_syntax::ast;

use super::record_call::RecordKey;
use crate::body::FnCx;
use crate::hir::{self, TyId};

impl FnCx<'_, '_> {
    /// `r[k]` of an open record (`record_reads`) where a type that holds no `undefined` is
    /// expected and nothing else converts it. TypeScript types the read as `V` (without
    /// `noUncheckedIndexedAccess`) and trusts that the key is there; Velt checks it, as it checks
    /// `x!`: a missing key stops the program where Node would go on with `undefined`, which a `V`
    /// here cannot hold. Tests and `||` take the read as it is, before any conversion.
    pub(super) fn record_value_as(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let converts = self.cx.ty.nothing_of(h.ty) == Some(hir::Nothing::Undefined)
            && self.cx.ty.opt_payload(h.ty) == Some(exp)
            && self.f.record_reads.contains_key(&h.span);
        if !converts {
            return Err(h);
        }
        let span = h.span;
        let msg = "TypeError: the record has no value at this key";
        Ok(self.checked_present(h, msg, Some(exp), true, span))
    }

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
                let errors = self.cx.diags.len();
                let h = self.coerce(h, m);
                if self.cx.diags.len() != errors {
                    // Reported against the member's type: not again against the values'.
                    return Some(self.error_expr(h.span));
                }
                Some(self.coerce(h, v))
            })
            .collect();
        self.record_view_missing(view, props, span);
        self.record_literal_with(d, k, v, props, pre, span)
    }

    /// Reports each required named member of record view `view` that literal `props` leaves
    /// out, as TypeScript does (TS2741). A literal with a spread may get it from there.
    fn record_view_missing(&mut self, view: TyId, props: &[ast::ObjectProp], span: Span) {
        if props.iter().any(|p| {
            !matches!(
                p,
                ast::ObjectProp::KeyValue(..) | ast::ObjectProp::Shorthand(_)
            )
        }) {
            return;
        }
        let Some((_, tag)) = self.cx.record_view(view) else {
            return;
        };
        let hir::TyKind::Adt(d, _) = self.cx.ty.kind(tag) else {
            return;
        };
        let required: Vec<String> = match self.cx.adt(*d) {
            Some(a) => a
                .fields
                .iter()
                .filter(|f| !f.optional)
                .map(|f| f.name.clone())
                .collect(),
            None => return,
        };
        let given = |n: &str| {
            props.iter().any(|p| match p {
                ast::ObjectProp::KeyValue(k, _) | ast::ObjectProp::Shorthand(k) => k.name == n,
                _ => false,
            })
        };
        let shown = self.cx.display(view);
        for name in required.iter().filter(|n| !given(n)) {
            self.cx.err(
                format!(
                    "missing field `{}` in `{shown}` literal",
                    self.cx.shown_name(name)
                ),
                span,
            );
        }
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
        if self.cx.ty.has_error(read.ty) || read.ty == member {
            return Ok(read);
        }
        let payload = self.cx.ty.opt_payload(read.ty).unwrap_or(read.ty);
        if self.cx.dynamic(payload).is_some() {
            let read = self.collapse_dynamic_option(read);
            return Ok(self.cast_dynamic(read, member));
        }
        // `V | undefined` to the member's narrower type, checked as `read as T` is.
        Ok(self
            .narrowing_assertion(read, member, span)
            .unwrap_or_else(|h| self.cast_dynamic(h, member)))
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
