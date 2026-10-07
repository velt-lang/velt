//! Narrowing of nullable fields (`if (node.left === null) return 1; … node.left`), as TS does
//! for property accesses. A field path of a local (`node.left`, `this.a.b`) that a condition
//! proves non-null gets a *token*: a hidden unit local of the frame whose `NonNull` fact lives in
//! the scopes like a local's, so branches, joins and loops treat it like any other narrowing.
//!
//! A narrowed read is an in-place unwrap that lowering checks (`UnwrapSome` of a field, unlike
//! of a narrowed local, tests the tag): a call between the check and the read may change the
//! field (TS ignores that), and then the read panics instead of reading `null` as an object. Assigning the path (or a prefix of it, or
//! its root local) drops the narrowing.
//!
//! `instanceof` narrows a field path to a subclass only when every field on it is `readonly`
//! (`node.left instanceof Num`): the read is not checked again, so the object must not change
//! between the test and the read.

use velt_common::Span;
use velt_syntax::ast;

use super::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, LocalId, TyId};

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

    /// `h`, the value of `object.prop`, as the subclass an `instanceof` test narrowed it to.
    pub(crate) fn downcast_field(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        h: hir::Expr,
    ) -> hir::Expr {
        let token = self
            .member_path(object, prop)
            .and_then(|(root, path)| self.find_token(root, &path));
        match token {
            Some(t) if self.f.mutable_tests.contains(&t) => {
                self.f.unnarrowed_reads.push(h.span);
                h
            }
            Some(t) => {
                self.note_refused_read(t, h.span);
                self.downcast_narrowed(t, h)
            }
            None => h,
        }
    }

    /// `e instanceof C` on a field path that cannot be narrowed (not all `readonly`): later reads
    /// of it get a note when they fail (`unnarrowed_note`).
    pub(crate) fn note_mutable_test(&mut self, e: &ast::Expr) {
        if let Some(token) = self.field_token(e) {
            self.f.mutable_tests.push(token);
        }
    }

    /// A note for a failed member access on `recv` (at `span`) if `recv` is a field that an
    /// `instanceof` test could not narrow.
    pub(crate) fn unnarrowed_note(&self, span: Span) -> Option<&'static str> {
        self.f.unnarrowed_reads.contains(&span).then_some(
            "`instanceof` does not narrow this field: it is not `readonly`, so it could change between the test and this read; copy it into a local and test the local",
        )
    }

    /// The token and the type (as narrowed so far) of `e` when it is a path of `readonly` class
    /// fields of a local (`node.left.right`), and not of a union type.
    pub(crate) fn readonly_field_path(&mut self, e: &ast::Expr) -> Option<(LocalId, TyId)> {
        let (root, path) = self.field_path(e)?;
        let mut t = self.narrowed_local_ty(root);
        for k in 0..path.len() {
            let (d, _) = self.cx.class_of(t)?;
            // The field this code names (`#x` lexically: a subclass's own `#x` is another
            // field than the base's), then whether that one is `readonly`.
            let (i, fty) = self.cx.field_seen_from(t, &path[k], self.owner)?;
            let a = self.cx.adt(d)?;
            if !a.fields.get(i as usize).is_some_and(|f| f.readonly) {
                return None;
            }
            t = fty;
            let narrowed = self.find_token(root, &path[..=k]);
            if let Some(c) = narrowed.and_then(|tk| self.narrowed_class(tk)) {
                if self.downcast_applies(t, c) {
                    t = c;
                }
            }
        }
        let u = self.cx.ty.opt_payload(t).unwrap_or(t);
        if self.cx.union_def(u).is_some() {
            return None;
        }
        Some((self.field_token(e)?, t))
    }

    /// The type local `l` reads as here (null, union member and subclass narrowing applied).
    fn narrowed_local_ty(&mut self, l: LocalId) -> TyId {
        let mut t = self.local_ty(l);
        if let Some(p) = self.cx.ty.opt_payload(t) {
            if !self.is_narrowed(l) {
                return t;
            }
            t = p;
        }
        if let Some(members) = self.cx.union_members(t) {
            match self.allowed_members(l).as_deref() {
                Some([v]) => t = members[*v as usize],
                _ => return t,
            }
        }
        match self.narrowed_class(l) {
            Some(c) if self.downcast_applies(t, c) => c,
            _ => t,
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

    /// The root local and field names of field token `token`.
    pub(crate) fn token_path(&self, token: LocalId) -> Option<(LocalId, &[String])> {
        let mut tokens = self.f.field_tokens.iter();
        tokens
            .find(|t| t.token == token)
            .map(|t| (t.root, t.path.as_slice()))
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
