//! Generator function expressions (docs/reference/functions.md "Generator function
//! expressions"): `function* (params): Generator<T> { body }` and `async function* (...):
//! AsyncGenerator<T> { body }`, optionally named. Arrows cannot be generators, so this is the
//! one form of `function` expression Velt has; any other is an error asking for an arrow.
//!
//! A generator expression is a closure (`closure.rs`) whose call creates a generator, like a
//! call of a `function*` declaration (hir_encodings.md "Generators"): its `FnDef` has
//! `is_generator` (and `is_async` for an async generator) and captures. It always escapes —
//! the generator outlives the call that creates it — so it captures by value like any escaping
//! closure: each generator shares the captured objects, and a variable assigned after the
//! capture (or by a generator, `crate::moves`) lives in a shared cell that the generators hold
//! (velt_vir async_fn/ctor.rs `take_capture`). The value's type is `(params) => R`, where `R`
//! is the declared result with the body's error type as `E` (`Generator<T, E>`); the call
//! itself never throws. Its parameters are owned, like a generator function's.
//!
//! Not supported: the expression's name inside its body (recursion), type parameters and rest
//! parameters.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::closure::Checked;
use crate::body::{FnCx, Frame, LocalKind};
use crate::defs::FnKind;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `function* (...) { ... }` / `async function* (...) { ... }` as a value.
    pub(super) fn function_expr(&mut self, d: &ast::FnDecl, span: Span) -> hir::Expr {
        let sig = &d.sig;
        if !sig.is_generator {
            self.cx.error(
                Diagnostic::error("function expressions are not supported", span).with_note(
                    "TypeScript allows this (`function (x) { ... }` as a value); Velt doesn't because arrow functions do the same without their own `this`; write an arrow function: `(x: T): R => { ... }` (a generator is written `function* (...)`)",
                ),
            );
            return self.error_expr(span);
        }
        if !self.gen_closure_sig_ok(sig) {
            return self.error_expr(span);
        }
        let ret = match &sig.ret {
            Some(t) => self.resolve(t),
            None => self.cx.ty.unit,
        };
        let Some((t, e, written)) = crate::collect::expr_result_args(self.cx, ret, sig) else {
            return self.error_expr(span);
        };
        let mut declared = sig.throws.as_ref().map(|t| self.resolve(t));
        if written {
            declared = self.cx.join_errors(declared, Some(e));
        }
        self.gen_closure(d, (ret, t), declared, span)
    }

    /// Type parameters and rest parameters are errors (module docs).
    fn gen_closure_sig_ok(&mut self, sig: &ast::FnSig) -> bool {
        if let Some(g) = sig.generics.first() {
            self.cx.error(
                Diagnostic::error(
                    "a generator function expression cannot have type parameters",
                    g.name.span,
                )
                .with_note("TypeScript allows this; Velt doesn't because a function value has one type; write a generic `function*` declaration and call it"),
            );
            return false;
        }
        if let Some(p) = sig.params.iter().find(|p| p.rest) {
            self.cx.error(
                Diagnostic::error(
                    "a generator function expression cannot have a rest parameter",
                    p.span,
                )
                .with_note("TypeScript allows this; Velt doesn't because calls through a function value pass each argument as written; take an array parameter (`xs: T[]`), or declare a `function*`"),
            );
            return false;
        }
        true
    }

    /// The closure of a generator expression yielding `t` with declared result `ret` and the
    /// declared error type `declared` (`throws`, or `E` in the result).
    pub(super) fn gen_closure(
        &mut self,
        d: &ast::FnDecl,
        (ret, t): (TyId, TyId),
        declared: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let sig = &d.sig;
        let def = self.alloc_closure(span);
        let mut frame = Frame::new(FnKind::Closure, Some(self.cx.ty.unit));
        frame.scopes[0].hi = span.hi;
        frame.escaping = true;
        frame.is_async = sig.is_async;
        frame.yield_ty = Some(t);
        frame.fn_expr_name = Some(sig.name.name.clone()).filter(|n| !n.is_empty());
        let saved = std::mem::replace(&mut self.f, frame);
        self.outer.push(saved);
        let mut params = vec![];
        let mut ptys = vec![];
        for p in &sig.params {
            let ty = self.resolve(&p.ty);
            params.push(self.declare_local_mut(&p.name, ty, LocalKind::Param, false));
            ptys.push(ty);
        }
        let mut stmts = vec![];
        self.stmts_into(&d.body.stmts, &mut stmts);
        let block = hir::Block {
            stmts,
            value: None,
            span: d.body.span,
        };
        self.rec_frame_scopes();
        let parent = self.outer.pop().expect("ICE: closure frame");
        self.finish_using_shares();
        let frame = std::mem::replace(&mut self.f, parent);
        self.no_captured_generators(&frame);
        let captures = self.capture_modes(&frame, span);
        let clause = sig
            .throws
            .as_ref()
            .or(sig.ret.as_ref())
            .map_or(span, |t| t.span);
        let err = self.closure_error(def, declared, &frame, clause);
        let never = self.cx.ty.never;
        let result = self.cx.with_generator_error(ret, err.unwrap_or(never));
        let fn_ty = self.cx.ty.intern(TyKind::FnPtr {
            params: ptys.clone(),
            ret: result,
            throws: never,
        });
        self.cx.fn_info_mut(def).escaping = true;
        let ret = self.cx.with_generator_error(ret, never);
        self.finish_closure(Checked {
            def,
            frame,
            block,
            declared: params,
            ptys,
            ret,
            captures,
            is_async: false,
            generator: Some(sig.is_async),
            span,
        });
        let aparams: Vec<ast::ArrowParam> = sig.params.iter().map(arrow_param).collect();
        crate::body::defaults::arrow_defaults(self.cx, self.module, def, &aparams);
        self.mk(H::Closure(def), fn_ty, span)
    }
}

impl FnCx<'_, '_> {
    /// `name` is not in scope but names an enclosing generator function expression (a
    /// recursive call): reported with the fix. Whether it was.
    pub(super) fn fn_expr_self_ref(&mut self, name: &str, span: Span) -> bool {
        let named = |f: &Frame| f.fn_expr_name.as_deref() == Some(name);
        if !named(&self.f) && !self.outer.iter().any(named) {
            return false;
        }
        self.cx.error(
            Diagnostic::error(
                format!("`{name}` cannot be used inside the generator function expression it names"),
                span,
            )
            .with_note("TypeScript allows this (the name of a function expression is in scope in its body); Velt doesn't because a function value cannot refer to itself; declare the generator with `function* name(...)` and refer to that"),
        );
        true
    }
}

/// A declared parameter as an arrow parameter (for its default, `defaults::arrow_defaults`).
fn arrow_param(p: &ast::Param) -> ast::ArrowParam {
    ast::ArrowParam {
        name: p.name.clone(),
        ty: Some(p.ty.clone()),
        default: p.default.clone(),
        optional: p.optional,
    }
}
