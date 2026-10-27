//! Narrowing `any` and `unknown` (phase 1b of #924; hir_encodings.md "any and unknown") where
//! the tested type has no single static type: `Array.isArray(x)` (`any[]` in TypeScript),
//! `x instanceof C` for a generic class `C` (`C<any>`) and `typeof x === "function"`. The
//! value keeps its dynamic representation, so its members are read through the descriptor of
//! the value held (an `unknown` reads as an `any` there). `typeof` tests for primitives and
//! `instanceof` of a class without type parameters narrow to the static type instead
//! (`body::narrow`, `downcast`).

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
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
        if self.uses_proxy() && !self.cx.scopes[self.module].is_std {
            // A proxy of an array is one too.
            return Some(self.prelude_hir_call("__anyIsArrayProxy", vec![x], b, span));
        }
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
            (Some(Dynamic::Unknown | Dynamic::Object), Some(any)) => vec![Fact::Class(l, any)],
            _ => vec![],
        }
    }

    /// `x` (an `unknown` variable) reads as an `object` where `typeof x === "object"` holds
    /// (TypeScript's `object | null`; issue #926).
    pub(crate) fn dynamic_to_object_fact(&mut self, e: &ast::Expr) -> Vec<Fact> {
        let Some(l) = self.own_dynamic_local(e) else {
            return vec![];
        };
        let t = self.local_ty(l);
        let Some(defs) = self.cx.any else {
            return vec![];
        };
        if self.cx.dynamic(t) != Some(Dynamic::Unknown) {
            return vec![];
        }
        let object = self.cx.ty.intern(hir::TyKind::Dyn(defs.object, vec![]));
        vec![Fact::Class(l, object)]
    }

    /// `x` reads as a `Function` where `typeof x === "function"` holds, as in TypeScript, when
    /// it is an `unknown` (issue #926); an `object` reads as an `any` there, and an `any` stays.
    pub(crate) fn dynamic_to_function_fact(&mut self, e: &ast::Expr) -> Vec<Fact> {
        let Some(l) = self.own_dynamic_local(e) else {
            return vec![];
        };
        let t = self.local_ty(l);
        match (self.cx.dynamic(t), self.cx.any) {
            (Some(Dynamic::Unknown), Some(defs)) => {
                let function = self.cx.ty.intern(hir::TyKind::Dyn(defs.function, vec![]));
                vec![Fact::Class(l, function)]
            }
            _ => self.dynamic_to_any_fact(e),
        }
    }

    /// Facts of `typeof x.f === tag` on field path `x.f` (its token `token`) of type `u`, when
    /// that is `any` or `unknown`: the path reads as the type the tag names where the test
    /// holds, as a variable does (`typeof_split`; reads: `FnCx::narrowed_field`). `None`
    /// when `u` is no dynamic type.
    pub(crate) fn dynamic_field_typeof(
        &mut self,
        token: hir::LocalId,
        u: TyId,
        tag: &str,
    ) -> Option<(Vec<Fact>, Vec<Fact>)> {
        let kind = self.cx.dynamic(u)?;
        let defs = self.cx.any?;
        let t = match (tag, kind) {
            ("number", _) => self.cx.ty.f64,
            ("string", _) => self.cx.ty.str_,
            ("boolean", _) => self.cx.ty.bool_,
            ("function", Dynamic::Unknown) => {
                self.cx.ty.intern(hir::TyKind::Dyn(defs.function, vec![]))
            }
            ("object", Dynamic::Unknown) => {
                self.cx.ty.intern(hir::TyKind::Dyn(defs.object, vec![]))
            }
            _ => return Some((vec![], vec![])),
        };
        Some((vec![Fact::Class(token, t)], vec![]))
    }

    /// Facts of a call `f(x)` of a type predicate `v is T` (`target`) whose argument `x` is a
    /// variable or field path of type `unknown` (`object`, `Function`; not `any`): `x` reads as a `T` where the call
    /// returned `true`, converted from the value held, which checks it (`downcast_narrowed`).
    pub(crate) fn dynamic_predicate_facts(
        &mut self,
        arg: &ast::Expr,
        target: TyId,
    ) -> Option<(Vec<Fact>, Vec<Fact>)> {
        // An `any` reads its members as it is (it needs no narrowing to use them).
        let typed = |s: &Self, t: TyId| s.cx.dynamic(t).is_some_and(|d| d != Dynamic::Any);
        if let Some(l) = self.own_dynamic_local(arg) {
            let ok = typed(self, self.local_ty(l));
            return ok.then(|| (vec![Fact::Class(l, target)], vec![]));
        }
        if !matches!(arg.kind, ast::ExprKind::Member { .. }) {
            return None;
        }
        let (token, _, u, _) = self.field_with_members(arg)?;
        typed(self, u).then(|| (vec![Fact::Class(token, target)], vec![]))
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

    /// `Number.isFinite(x)`, `Number.isNaN(x)`, `Number.isInteger(x)` and
    /// `Number.isSafeInteger(x)` of a dynamic `x` (TypeScript's parameter is `unknown`): `false`
    /// unless `x` holds a number, as in JavaScript (`typeof x === "number" && Number.isNaN(x)`).
    pub(crate) fn dynamic_number_test(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &callee.kind
        else {
            return None;
        };
        let ast::ExprKind::Ident(ns) = &object.kind else {
            return None;
        };
        let tests = ["isFinite", "isNaN", "isInteger", "isSafeInteger"];
        if ns.name != "Number"
            || self.is_local_name("Number")
            || !tests.contains(&prop.name.as_str())
        {
            return None;
        }
        let [arg] = args else {
            return None;
        };
        let t = self.trial_type(arg)?;
        self.cx.dynamic(t)?;
        Some(self.operand_once(arg, span, |lw, v| {
            let of = synth(
                ast::ExprKind::Unary {
                    op: ast::UnaryOp::TypeOf,
                    expr: Box::new(v.clone()),
                },
                span,
            );
            let number = synth(ast::ExprKind::Lit(ast::Lit::Str("number".into())), span);
            let is_number = synth(
                ast::ExprKind::Binary {
                    op: ast::BinaryOp::StrictEq,
                    lhs: Box::new(of),
                    rhs: Box::new(number),
                },
                span,
            );
            let test = synth(
                ast::ExprKind::Call {
                    callee: Box::new(callee.clone()),
                    type_args: vec![],
                    args: vec![v.clone()],
                    optional: false,
                },
                span,
            );
            let both = synth(
                ast::ExprKind::Binary {
                    op: ast::BinaryOp::And,
                    lhs: Box::new(is_number),
                    rhs: Box::new(test),
                },
                span,
            );
            lw.expr(&both, None, Want::Move)
        }))
    }

    /// The variable of this function `e` names, when its type is `any` or `unknown`.
    pub(crate) fn own_dynamic_local(&mut self, e: &ast::Expr) -> Option<hir::LocalId> {
        use ast::ExprKind as A;
        let name = match &e.kind {
            A::Paren(x) => return self.own_dynamic_local(x),
            A::Ident(i) => i.name.as_str(),
            _ => return None,
        };
        let l = self
            .f
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.names.get(name).copied())?;
        self.cx.dynamic(self.local_ty(l)).map(|_| l)
    }

    /// `x.prop` where `x`, a variable or field path of type `unknown`, is narrowed to an object
    /// type `T` (a type predicate): the member read from the value held, as an `any` reads it,
    /// converted to the type `T` gives it. The value itself is not converted to `T`, so a
    /// function with properties held there (`isComponent(t)`, then `t.__setup`) reads too.
    /// `None` for any other read.
    pub(crate) fn narrowed_dynamic_member(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        // Most reads are in scopes no test narrowed to a type.
        if want == Want::BorrowMut || self.f.scopes.iter().all(|s| s.classes.is_empty()) {
            return None;
        }
        let (token, path) = match &object.kind {
            ast::ExprKind::Ident(_) => (self.own_dynamic_local(object)?, false),
            ast::ExprKind::Member { .. } => (self.existing_field_token(object)?, true),
            _ => return None,
        };
        let target = self.narrowed_class(token)?;
        let plain = self.cx.class_of(target).is_none() && self.cx.dynamic(target).is_none();
        if !plain || !self.cx.is_object_type(target) {
            return None;
        }
        if path {
            // The field's own type, read as it is (`dynamic_raw`).
            let saved = self.dynamic_raw.replace(object.span);
            let declared = self.peek_ty(object);
            self.dynamic_raw = saved;
            declared.and_then(|t| self.cx.dynamic(t))?;
        }
        let fields = self.cx.public_fields(target);
        let f = fields.into_iter().find(|f| f.name == prop.name)?;
        let fty = match f.optional && self.cx.dynamic(f.ty).is_none() {
            true => self.cx.ty.option_of(f.ty, hir::Nothing::Undefined),
            false => f.ty,
        };
        let any = self.any_ty()?;
        let saved = self.dynamic_raw.replace(object.span);
        let obj = self.expr(object, None, Want::Borrow);
        self.dynamic_raw = saved;
        let boxed = match self.coerce_dynamic(obj, any, true) {
            Ok(h) | Err(h) => h,
        };
        match self.dynamic_get(boxed, &prop.name, want, span) {
            Ok(read) => Some(self.cast_dynamic(read, fty)),
            Err(_) => Some(self.error_expr(span)),
        }
    }
}
