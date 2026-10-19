//! `x[k]` in a generic body where `x` is a type parameter's value and `k` a key of it
//! (`source[key]` with `K extends keyof T`, #350; docs/internals/design/deferred-types.md
//! "`keyof T` and `T[K]`"): an `Intrinsic::KeyRead` of type `T[K]`, the form the signature
//! writes. At every instantiation `K` must be one key of `T`'s object type
//! (`Ctx::reduce_key_read`); lowering then reads that field.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::places::{is_place, set_place_mode};
use crate::body::{FnCx, Want};
use crate::hir::{self, Intrinsic, TyKind, TyOp, UseMode};

impl FnCx<'_, '_> {
    /// `object[index]` read where `object` is a type parameter's value and `index` a key known
    /// only per instantiation (see [`Self::generic_key_read`]).
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
        let key = self.expr(index, None, Want::Borrow);
        self.generic_key_read(object, t, key, span)
    }

    /// `object[index] = value` where `object` is a type parameter's value and `index` a key
    /// known only per instantiation: an `Intrinsic::KeyWrite`. `None` for any other target.
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
        let key = self.expr(index, None, Want::Borrow);
        let mut lets = vec![];
        let (parts, ty) = self.key_parts(object, t, key, true, span, &mut lets)?;
        let v = self.expr(value, Some(ty), Want::Move);
        let v = self.coerce(v, ty);
        let mut args = parts;
        args.push(v);
        let unit = self.cx.ty.unit;
        let write = self.intrinsic(Intrinsic::KeyWrite, args, unit, span);
        Some(self.with_lets(lets, write))
    }

    /// Is `object[key]` (with these types) a key access of a generic body: a type parameter's
    /// value by a key type known only per instantiation?
    fn generic_keyed(&self, t: hir::TyId, k: hir::TyId) -> bool {
        let deferred_key = matches!(
            self.cx.ty.kind(k),
            TyKind::Param(_) | TyKind::Op(TyOp::Inline(_), _)
        );
        matches!(self.cx.ty.kind(t), TyKind::Param(_)) && deferred_key
    }

    /// `object[key]` with `object` of type `t`, a type parameter, and `key` (checked already)
    /// of a key type known only per instantiation; `None` for any other index.
    pub(super) fn generic_key_read(
        &mut self,
        object: &ast::Expr,
        t: hir::TyId,
        key: hir::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        if !self.generic_keyed(t, key.ty) {
            return None;
        }
        let mut lets = vec![];
        let (parts, ty) = self.key_parts(object, t, key, false, span, &mut lets)?;
        let read = self.intrinsic(Intrinsic::KeyRead, parts, ty, span);
        Some(self.with_lets(lets, read))
    }

    /// The object and key of `object[key]` as borrowed places (`write`: the object mutably),
    /// with the type `T[K]` of the field, the form the signature writes, and its requirement
    /// recorded (`Ctx::key_read_requirement`).
    fn key_parts(
        &mut self,
        object: &ast::Expr,
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
                object: Box::new(named(name(self, i)?, object.span)),
                key: Box::new(key_ast),
            },
            span,
        };
        let env = self.env.clone();
        let ty = self.cx.resolve_type(&form, &env);
        if self.cx.ty.has_error(ty) {
            return None;
        }
        let (want, mode) = match write {
            true => (Want::BorrowMut, UseMode::BorrowMut),
            false => (Want::Borrow, UseMode::Borrow),
        };
        let obj = self.expr(object, None, want);
        let mut parts = vec![];
        for (h, mode) in [(obj, mode), (key, UseMode::Borrow)] {
            let mut h = match is_place(&h) {
                true => h,
                false => self.temp("<key>", h, lets),
            };
            set_place_mode(&mut h, mode);
            parts.push(h);
        }
        let req = self.cx.key_read_requirement(&form, &env, span);
        let reads = &mut self.cx.type_level.inline.reads;
        reads.insert((parts[0].ty, parts[1].ty), req);
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
