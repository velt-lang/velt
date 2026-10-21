//! `x[k]` in a generic body where `x` is a type parameter's value and `k` a key of it
//! (`source[key]` with `K extends keyof T`, #350; docs/internals/design/deferred-types.md
//! "`keyof T` and `T[K]`"): an `Intrinsic::KeyRead` of type `T[K]`, the form the signature
//! writes. At every instantiation `K` must be one key of `T`'s object type
//! (`Ctx::reduce_key_read`); lowering then reads that field.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::places::{is_place, set_place_mode};
use crate::body::{FnCx, Want};
use crate::hir::{self, Intrinsic, TyKind, TyOp, UseMode};

impl FnCx<'_, '_> {
    /// `obj[index]` with `obj` (checked) a type parameter's value and `index` a key expression
    /// (`obj(os)[key(ks, 0)]`): its key read, or an error.
    pub(super) fn param_key_read(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let key = self.expr(index, None, Want::Borrow);
        match self.key_read_of(obj, key, span) {
            Ok(h) => h,
            Err(parts) => {
                let (obj, key) = *parts;
                if !self.cx.ty.has_error(key.ty) {
                    self.cannot_index(&obj);
                }
                self.error_expr(span)
            }
        }
    }

    /// `object[index]` read where `object` is a type parameter's value and `index` a key known
    /// only per instantiation, both paths (see [`Self::key_read_of`]).
    pub(super) fn generic_key_index(
        &mut self,
        object: &ast::Expr,
        index: &ast::Expr,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        if matches!(want, Want::BorrowMut) {
            return None;
        }
        let (t, k) = (self.peek_ty(object)?, self.peek_ty(index)?);
        if !self.generic_keyed(t, k) {
            return None;
        }
        let obj = self.expr(object, None, Want::Borrow);
        let key = self.expr(index, None, Want::Borrow);
        self.key_read_of(obj, key, span).ok()
    }

    /// `object[index] = value` where `object` is a type parameter's value and `index` a key
    /// known only per instantiation, both paths: an `Intrinsic::KeyWrite`. `None` for any
    /// other target.
    pub(super) fn generic_key_write(
        &mut self,
        object: &ast::Expr,
        index: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        let (t, k) = (self.peek_ty(object)?, self.peek_ty(index)?);
        if !self.generic_keyed(t, k) {
            return None;
        }
        let obj = self.expr(object, None, Want::BorrowMut);
        let key = self.expr(index, None, Want::Borrow);
        Some(self.key_write_of(obj, key, None, value, span))
    }

    /// `obj[index]` as an assignment target, `obj` (checked) of a type parameter: `Ok(Some)`
    /// with the object and the key (checked) when the key is known only per instantiation,
    /// `Ok(None)` (reported) for any other key, `Err` (`obj` back) when `obj` is no type
    /// parameter's value or the key is a constant.
    pub(super) fn generic_key_target(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
    ) -> Result<Option<(hir::Expr, hir::Expr)>, hir::Expr> {
        let constant = super::member::literal_key(index).is_some();
        if !matches!(self.cx.ty.kind(obj.ty), TyKind::Param(_)) || constant {
            return Err(obj);
        }
        let key = self.expr(index, None, Want::Borrow);
        if self.generic_keyed(obj.ty, key.ty) {
            return Ok(Some((obj, key)));
        }
        if !self.cx.ty.has_error(key.ty) {
            self.cannot_index(&obj);
        }
        Ok(None)
    }

    /// `obj[key] = value` (`op=`: not supported) for a target of [`Self::generic_key_target`].
    pub(super) fn key_write_of(
        &mut self,
        obj: hir::Expr,
        key: hir::Expr,
        op: Option<ast::BinaryOp>,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        if op.is_some() {
            self.cannot_index(&obj);
            self.expr(value, None, Want::Borrow);
            return self.error_expr(span);
        }
        // Any field of any object type the parameter is may be written (`crate::object_copies`:
        // a value written through an alias is shared, not copied).
        let write = crate::object_copies::Write {
            ty: None,
            name: String::new(),
            span,
        };
        self.cx.object_copies.writes.push(write);
        let t = obj.ty;
        let mut lets = vec![];
        let Some((mut parts, ty)) = self.key_parts(obj, t, key, true, span, &mut lets) else {
            self.expr(value, None, Want::Borrow);
            return self.error_expr(span);
        };
        let v = self.expr(value, Some(ty), Want::Move);
        parts.push(self.coerce(v, ty));
        let unit = self.cx.ty.unit;
        let write = self.intrinsic(Intrinsic::KeyWrite, parts, unit, span);
        self.with_lets(lets, write)
    }

    /// "cannot index a value of type `T`", at `obj`.
    pub(super) fn cannot_index(&mut self, obj: &hir::Expr) {
        let tn = self.cx.display(obj.ty);
        let mut d = Diagnostic::error(format!("cannot index a value of type `{tn}`"), obj.span);
        if self.cx.class_of(obj.ty).is_some() {
            d = d.with_note("use a method such as `m.get(key)`");
        }
        self.cx.error(d);
    }

    /// Is `object[key]` (with these types) a key access of a generic body: a type parameter's
    /// value by a key type known only per instantiation?
    pub(super) fn generic_keyed(&self, t: hir::TyId, k: hir::TyId) -> bool {
        let deferred_key = matches!(
            self.cx.ty.kind(k),
            TyKind::Param(_) | TyKind::Op(TyOp::Inline(_), _)
        );
        matches!(self.cx.ty.kind(t), TyKind::Param(_)) && deferred_key
    }

    /// `obj[key]` read, both checked, when `obj` is a type parameter's value and `key` of a
    /// key type known only per instantiation: an `Intrinsic::KeyRead` of type `T[K]`. `Err`
    /// gives both back for any other access.
    pub(super) fn key_read_of(
        &mut self,
        obj: hir::Expr,
        key: hir::Expr,
        span: Span,
    ) -> Result<hir::Expr, Box<(hir::Expr, hir::Expr)>> {
        let t = obj.ty;
        if !self.generic_keyed(t, key.ty) {
            return Err(Box::new((obj, key)));
        }
        let mut lets = vec![];
        let Some((parts, ty)) = self.key_parts(obj, t, key, false, span, &mut lets) else {
            return Ok(self.error_expr(span));
        };
        let read = self.intrinsic(Intrinsic::KeyRead, parts, ty, span);
        Ok(self.with_lets(lets, read))
    }

    /// The object (checked, of type `t`, a type parameter) and key of `obj[key]` as borrowed
    /// places (`write`: the object mutably), bound to temporaries in that order when they are
    /// no places, with the type `T[K]` of the field, the form the signature writes, and its
    /// requirement recorded (`Ctx::key_read_requirement`). `None` (reported) when `T[K]` is
    /// no type.
    fn key_parts(
        &mut self,
        obj: hir::Expr,
        t: hir::TyId,
        key: hir::Expr,
        write: bool,
        span: Span,
        lets: &mut Vec<hir::Stmt>,
    ) -> Option<(Vec<hir::Expr>, hir::TyId)> {
        let TyKind::Param(i) = *self.cx.ty.kind(t) else {
            return None;
        };
        let name = |s: &Self, i: u32| s.env.params.get(i as usize).cloned();
        let key_ast = match self.cx.ty.kind(key.ty).clone() {
            TyKind::Param(j) => named(name(self, j)?, key.span),
            TyKind::Op(TyOp::Inline(f), _) => self.cx.inline_form_ast(f),
            _ => return None,
        };
        let form = ast::TypeExpr {
            kind: ast::TypeExprKind::Indexed {
                object: Box::new(named(name(self, i)?, obj.span)),
                key: Box::new(key_ast),
            },
            span,
        };
        let env = self.env.clone();
        let ty = self.cx.resolve_type(&form, &env);
        if self.cx.ty.has_error(ty) {
            return None;
        }
        let mode = match write {
            true => UseMode::BorrowMut,
            false => UseMode::Borrow,
        };
        let mut parts = vec![];
        for (h, mode) in [(obj, mode), (key, UseMode::Borrow)] {
            let mut h = match is_place(&h) {
                true => h,
                false => self.temp("<key>", h, lets),
            };
            set_place_mode(&mut h, mode);
            parts.push(h);
        }
        let req = self.cx.key_read_requirement(&form, &env, write, span);
        let at = (parts[0].ty, parts[1].ty, span);
        self.cx.type_level.inline.reads.insert(at, req);
        Some((parts, ty))
    }
}

fn named(name: String, span: Span) -> ast::TypeExpr {
    ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![ast::Ident { name, span }],
            args: vec![],
        },
        span,
    }
}
