//! Narrowing of nullable fields (`if (node.left === null) return 1; … node.left`), as TS does
//! for property accesses. A field path of a local (`node.left`, `this.a.b`) that a condition
//! proves non-null gets a *token*: a hidden unit local of the frame whose `NonNull` fact lives in
//! the scopes like a local's, so branches, joins and loops treat it like any other narrowing.
//!
//! A narrowed read is an in-place unwrap that lowering checks (`UnwrapSome` of a field, unlike
//! of a narrowed local, tests the tag): a call between the check and the read may change the
//! field (TS ignores that), and then the read panics instead of reading `null` as an object. Assigning the path (or a prefix of it, or
//! its root local) drops the narrowing.

use velt_common::Span;
use velt_syntax::ast;

use super::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, LocalId};

/// A narrowed field path: the root local, the field names, and the token local.
pub(crate) struct FieldToken {
    root: LocalId,
    path: Vec<String>,
    token: LocalId,
}

impl FnCx<'_, '_> {
    /// The token of field path `e` (`x.a.b` on a local `x`), created when missing.
    pub(crate) fn field_token(&mut self, e: &ast::Expr) -> Option<LocalId> {
        let (root, path) = self.field_path(e)?;
        if let Some(t) = self.find_token(root, &path) {
            return Some(t);
        }
        let unit = self.cx.ty.unit;
        let token = self.new_local("<narrowed field>", unit, false, e.span, LocalKind::Temp);
        self.f.field_tokens.push(FieldToken { root, path, token });
        Some(token)
    }

    /// `h`, the value of `object.prop`, as its payload if a condition proved it non-null.
    pub(crate) fn narrowed_field(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        h: hir::Expr,
        want: Want,
    ) -> hir::Expr {
        let Some(payload) = self.cx.ty.opt_payload(h.ty) else {
            return h;
        };
        let Some((root, path)) = self.member_path(object, prop) else {
            return h;
        };
        match self.find_token(root, &path) {
            Some(t) if self.is_narrowed(t) => {
                let span = h.span;
                self.checked_unwrap(h, payload, want, span)
            }
            _ => h,
        }
    }

    /// Assigning field path `e` (or its root local) drops the narrowing of it and every path
    /// below it.
    pub(crate) fn unnarrow_fields(&mut self, e: &ast::Expr) {
        let Some((root, path)) = self.field_path(e) else {
            return;
        };
        let hit: Vec<LocalId> = self
            .f
            .field_tokens
            .iter()
            .filter(|t| t.root == root && t.path.starts_with(&path))
            .map(|t| t.token)
            .collect();
        for t in hit {
            self.unnarrow(t);
        }
    }

    /// The tokens of the field paths rooted at local `l`.
    pub(crate) fn field_tokens_of(&self, l: LocalId) -> Vec<LocalId> {
        let tokens = self.f.field_tokens.iter();
        tokens.filter(|t| t.root == l).map(|t| t.token).collect()
    }

    fn find_token(&self, root: LocalId, path: &[String]) -> Option<LocalId> {
        let mut tokens = self.f.field_tokens.iter();
        tokens
            .find(|t| t.root == root && t.path == path)
            .map(|t| t.token)
    }

    /// (root local, field names) of `x.a.b` (no `?.`, no calls or indexes; parentheses ok).
    fn field_path(&mut self, e: &ast::Expr) -> Option<(LocalId, Vec<String>)> {
        match &e.kind {
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } => self.member_path(object, prop),
            ast::ExprKind::Paren(inner) => self.field_path(inner),
            _ => None,
        }
    }

    fn member_path(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
    ) -> Option<(LocalId, Vec<String>)> {
        let (root, mut path) = match &object.kind {
            ast::ExprKind::Member { .. } | ast::ExprKind::Paren(_) => self.field_path(object)?,
            _ => (self.named_local(object)?, vec![]),
        };
        path.push(prop.name.clone());
        Some((root, path))
    }

    /// The payload of field value `h`, read in place (lowering checks it is still non-null,
    /// `velt_vir` `place.rs`); used like the field itself would be.
    fn checked_unwrap(
        &mut self,
        h: hir::Expr,
        payload: hir::TyId,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        let mode = self.use_mode(payload, want);
        self.mk(H::UnwrapSome(Box::new(h), mode), payload, span)
    }
}
