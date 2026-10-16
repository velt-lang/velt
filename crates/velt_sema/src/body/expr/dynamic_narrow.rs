//! Narrowing `any` and `unknown` (phase 1b of #924; hir_encodings.md "any and unknown") where
//! the tested type has no single static type: `Array.isArray(x)` (`any[]` in TypeScript),
//! `x instanceof C` for a generic class `C` (`C<any>`) and `typeof x === "function"`. The
//! value keeps its dynamic representation, so its members are read through the descriptor of
//! the value held (an `unknown` reads as an `any` there). `typeof` tests for primitives and
//! `instanceof` of a class without type parameters narrow to the static type instead
//! (`body::narrow`, `downcast`).

use velt_common::Span;
use velt_syntax::ast;

use crate::any::Dynamic;
use crate::body::narrow::Fact;
use crate::body::{FnCx, Want};
use crate::hir::{self, Intrinsic, TyId};

impl FnCx<'_, '_> {
    /// The argument of `Array.isArray(arg)` (the built-in: no local or item named `Array`)
    /// when it is a variable or path of type `any` / `unknown`.
    fn dynamic_is_array_arg<'a>(
        &mut self,
        callee: &ast::Expr,
        args: &'a [ast::Expr],
    ) -> Option<&'a ast::Expr> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &callee.kind
        else {
            return None;
        };
        let (ast::ExprKind::Ident(ns), [arg]) = (&object.kind, args) else {
            return None;
        };
        if ns.name != "Array" || prop.name != "isArray" || self.is_local_name("Array") {
            return None;
        }
        if self
            .cx
            .lookup_item_at(self.module, "Array", ns.span)
            .is_some()
        {
            return None;
        }
        let t = self.peek_ty(arg)?;
        self.cx.dynamic(t).map(|_| arg)
    }

    /// `Array.isArray(x)` on an `any` / `unknown` `x`: whether the value held is an array
    /// (`Intrinsic::AnyIsArray`).
    pub(crate) fn dynamic_is_array_call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let arg = self.dynamic_is_array_arg(callee, args)?;
        let x = self.expr(arg, None, Want::Borrow);
        let b = self.cx.ty.bool_;
        Some(self.intrinsic(Intrinsic::AnyIsArray, vec![x], b, span))
    }

    /// Facts of `Array.isArray(x)` on a dynamic variable `x`: an `unknown` reads as an `any`
    /// where it holds (TypeScript's `any[]`).
    pub(crate) fn dynamic_is_array_facts(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
    ) -> Option<(Vec<Fact>, Vec<Fact>)> {
        let arg = self.dynamic_is_array_arg(callee, args)?;
        Some((self.dynamic_to_any_fact(arg), vec![]))
    }

    /// `x` (an `unknown` variable) reads as an `any` where a test that leaves its type open
    /// holds; nothing for an `any` variable, which already does.
    pub(crate) fn dynamic_to_any_fact(&mut self, e: &ast::Expr) -> Vec<Fact> {
        let Some(l) = self.own_dynamic_local(e) else {
            return vec![];
        };
        let t = self.local_ty(l);
        match (self.cx.dynamic(t), self.any_ty()) {
            (Some(Dynamic::Unknown), Some(any)) => vec![Fact::Class(l, any)],
            _ => vec![],
        }
    }

    /// The type an `any` / `unknown` value `t` narrows to by `instanceof` of the generic class
    /// `class`: `any` (TypeScript's `C<any>`), since the type arguments of the instance held are
    /// known only at run time.
    pub(crate) fn dynamic_generic_instance(&mut self, t: TyId, generic: bool) -> Option<TyId> {
        if !generic || self.cx.dynamic(t).is_none() {
            return None;
        }
        self.any_ty()
    }

    /// Two narrowings of the dynamic local `l` to different types joined by `||`: it reads as an
    /// `any` (an `unknown`) where either holds.
    pub(crate) fn dynamic_join(&mut self, l: hir::LocalId) -> Option<TyId> {
        let t = self.local_ty(l);
        self.cx.dynamic(t)?;
        self.any_ty()
    }
}
