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
#[derive(Clone)]
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

    /// `h`, the value of `object.prop`, as its payload if a condition proved it non-null, and
    /// as the union member a `typeof` test narrowed it to.
    pub(crate) fn narrowed_field(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        h: hir::Expr,
        want: Want,
    ) -> hir::Expr {
        if self.f.field_tokens.is_empty() {
            return h;
        }
        let payload = self.present_payload(h.ty);
        let union = self.cx.union_members(payload.unwrap_or(h.ty));
        if payload.is_none() && union.is_none() {
            return h;
        }
        let Some((root, path)) = self.member_path(object, prop) else {
            return h;
        };
        let Some(t) = self.find_token(root, &path) else {
            return h;
        };
        let member = match (union, self.allowed_members(t).as_deref()) {
            (Some(ms), Some(&[v])) => Some((v, ms[v as usize])),
            _ => None,
        };
        let base_mode = match want {
            Want::BorrowMut => hir::UseMode::BorrowMut,
            _ => hir::UseMode::Borrow,
        };
        let span = h.span;
        let h = match payload {
            Some(payload) if self.is_narrowed(t) => {
                let mode = match member {
                    Some(_) => base_mode,
                    None => self.use_mode(payload, want),
                };
                // Each `UnwrapSome` of a field place checks it again (`checked_unwrap`).
                self.unwrap_present(h, mode, span)
            }
            // `x.f !== undefined` on a `T | null | undefined` field: its `T | null`.
            Some(_) if self.is_three_state(h.ty) && self.excludes(t, hir::Nothing::Undefined) => {
                let mid = self
                    .cx
                    .ty
                    .opt_payload(h.ty)
                    .expect("ICE: option without a payload");
                return self.checked_unwrap(h, mid, want, span);
            }
            Some(_) => return h,
            None => h,
        };
        match member {
            // `typeof x.f === "string"` (#1264): lowering checks the variant again, as a call
            // since the test may have changed the field.
            Some((variant, m)) => {
                let mode = self.use_mode(m, want);
                let kind = H::UnwrapVariant {
                    expr: Box::new(h),
                    variant,
                    mode,
                };
                self.mk(kind, m, span)
            }
            None => h,
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
            let owner = self.name_owner(&path[k]);
            let (i, fty) = self.cx.field_seen_from(t, &path[k], owner)?;
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
        let u = self.cx.ty.core(t).unwrap_or(t);
        if self.cx.union_def(u).is_some() {
            return None;
        }
        Some((self.field_token(e)?, t))
    }

    /// `e instanceof C` on a field path that is not all `readonly` (or not of classes), whose
    /// type is a nullable `C` (`o.next instanceof A` with `next?: A | null`): when true, the
    /// field is not null. Reads of it are checked again where they happen, as after `!= null`.
    pub(crate) fn nullable_instance_field(
        &mut self,
        e: &ast::Expr,
        class: hir::DefId,
    ) -> Option<LocalId> {
        let (root, path) = self.field_path(e)?;
        let mut t = self.narrowed_local_ty(root);
        for name in &path {
            let t0 = self.cx.ty.core(t).unwrap_or(t);
            t = self.cx.field_seen_from(t0, name, None)?.1;
        }
        let p = self.cx.ty.core(t)?;
        let yes = matches!(
            self.instance_kind(p, class),
            Ok(super::expr::downcast::Instance::Yes)
        );
        yes.then(|| self.field_token(e)).flatten()
    }

    /// The type local `l` reads as here (null, union member and subclass narrowing applied).
    fn narrowed_local_ty(&mut self, l: LocalId) -> TyId {
        let t = self.local_ty(l);
        self.narrowed_as(l, t)
    }

    /// `t`, the type of local or field token `l`, as narrowed here.
    fn narrowed_as(&mut self, l: LocalId, mut t: TyId) -> TyId {
        if let Some(p) = self.cx.ty.core(t) {
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

    /// `typeof x.f === tag` on field path `e` of a local (#1264): its token, whether its type is
    /// nullable, the type without the nothings, and the nothings.
    pub(super) fn field_with_members(
        &mut self,
        e: &ast::Expr,
    ) -> Option<(LocalId, bool, TyId, Option<hir::Nothing>)> {
        let (root, path) = self.field_path(e)?;
        let mut t = self.narrowed_local_ty(root);
        for k in 0..path.len() {
            if k > 0 {
                if let Some(token) = self.find_token(root, &path[..k]) {
                    t = self.narrowed_as(token, t);
                }
            }
            let owner = self.name_owner(&path[k]);
            (_, t) = self.cx.field_seen_from(t, &path[k], owner)?;
        }
        let token = self.field_token(e)?;
        let nothing = self.cx.ty.nothing_of(t);
        Some(match self.cx.ty.core(t) {
            Some(p) => (token, true, p, nothing),
            None => (token, false, t, nothing),
        })
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

    /// The tokens of the field paths rooted at local `l`, or indexed by it (`r[l]`).
    pub(crate) fn field_tokens_of(&self, l: LocalId) -> Vec<LocalId> {
        let key = key_name(l);
        let tokens = self.f.field_tokens.iter();
        tokens
            .filter(|t| t.root == l || t.path.contains(&key))
            .map(|t| t.token)
            .collect()
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

    /// (root local, field names) of `x.a.b` or `x["a"].b` (no `?.`, no calls or other indexes;
    /// parentheses ok).
    fn field_path(&mut self, e: &ast::Expr) -> Option<(LocalId, Vec<String>)> {
        if let ast::ExprKind::Paren(inner) = &e.kind {
            return self.field_path(inner);
        }
        // `x["a"]` (a constant key) is the same path as `x.a`.
        if let Some((object, prop)) = super::expr::member::member_view(e) {
            return self.member_path(object, &prop);
        }
        // `r[k]` with a key that cannot change (`const k`): a path of its own.
        let ast::ExprKind::Index {
            object,
            index,
            optional: false,
        } = &e.kind
        else {
            return None;
        };
        let prop = self.fixed_key(index)?;
        self.member_path(object, &prop)
    }

    /// Is `e` an index read `r[k]` with a key nothing can reassign ([`Self::fixed_key`])?
    pub(crate) fn fixed_index(&mut self, e: &ast::Expr) -> bool {
        match &e.kind {
            ast::ExprKind::Paren(inner) => self.fixed_index(inner),
            ast::ExprKind::Index {
                index,
                optional: false,
                ..
            } => self.fixed_key(index).is_some(),
            _ => false,
        }
    }

    /// Key `index` of `r[index]` when it is a variable nothing can reassign (a `const`, a
    /// `for (const k in r)` key): as a path name no property has, so `if (!r[k]) r[k] = [];
    /// r[k].push(x)` narrows `r[k]` like a field.
    pub(crate) fn fixed_key(&mut self, index: &ast::Expr) -> Option<ast::Ident> {
        let ast::ExprKind::Ident(id) = &index.kind else {
            return None;
        };
        let l = self.named_local(index)?;
        // A parameter may be reassigned: that drops the narrowing (`field_tokens_of`). Reads
        // are checked again anyway, so a key changed where this cannot see it (a loop's next
        // iteration) still finds the value missing rather than reading past it.
        let fixed = match self.local_kind(l) {
            LocalKind::Const => true,
            LocalKind::Param => self.closure_assignment(l).is_none(),
            LocalKind::Elem | LocalKind::Bind => !self.f.locals[l.0 as usize].mutable,
            _ => false,
        };
        fixed.then(|| ast::Ident {
            name: key_name(l),
            span: id.span,
        })
    }

    fn member_path(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
    ) -> Option<(LocalId, Vec<String>)> {
        let (root, mut path) = match &object.kind {
            ast::ExprKind::Member { .. }
            | ast::ExprKind::Index { .. }
            | ast::ExprKind::Paren(_) => self.field_path(object)?,
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

/// The path name of index key local `l` (`r[k]`): a name no property has.
fn key_name(l: LocalId) -> String {
    format!("\0{}", l.0)
}
