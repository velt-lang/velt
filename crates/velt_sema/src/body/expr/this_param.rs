//! Calls of functions declared with a `this` parameter (`function f(this: T, a: A)`, #942).
//!
//! `this` is the function's leading parameter (`FnInfo::this`, as a method's receiver), which
//! the call passes: `f.call(x, a)` and `f.apply(x, [a])` pass `x`, and `f(a)` passes
//! `undefined` (JavaScript's `this` of a plain call in a module), which `T` must accept as
//! `void` (`any` or `unknown`; TypeScript's TS2684 otherwise, even for `T | undefined`). A receiver of another
//! type than `T` is a mismatch, as TypeScript reports it (TS2345). Functions without a `this`
//! parameter are called as before.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::setters::synth;
use crate::any::Dynamic;
use crate::body::{FnCx, Want};
use crate::defs::ParamSig;
use crate::hir::{self, Callee, DefId, ExprKind as H, TyId};

impl FnCx<'_, '_> {
    /// Does function `d` declare a `this` parameter (not a method's receiver)?
    pub(crate) fn takes_this(&self, d: DefId) -> bool {
        self.cx.try_fn(d).is_some_and(|f| f.has_this_param())
    }

    /// The call of function `d` (named `name`), which declares a `this` parameter, with
    /// `this_arg` as `this` (`None`: a plain call, `f(a)`).
    #[allow(clippy::too_many_arguments)] // the call's parts and its receiver
    pub(crate) fn this_fn_call(
        &mut self,
        d: DefId,
        name: &str,
        this_arg: Option<&ast::Expr>,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let mut c = self.fn_callable(d, format!("function `{name}`"), span);
        let n = c.slot_names.len();
        let mut slots = vec![None; n];
        self.explicit_type_args(&mut slots, n, type_args, &c.defaults, span);
        let this = self
            .cx
            .fn_info(d)
            .this
            .clone()
            .expect("ICE: this parameter");
        // A generic function infers its type arguments from the receiver too
        // (`function at<T>(this: T[], i: number): T`): it is checked as the first argument.
        if let (Some(t), false) = (this_arg, c.slot_names.is_empty()) {
            c.params.insert(
                0,
                ParamSig {
                    name: "this".into(),
                    span,
                    ty: this.ty,
                    mode: this.mode,
                    default: None,
                },
            );
            let all: Vec<ast::Expr> = std::iter::once(t).chain(args).cloned().collect();
            let ck = self.check_call(&c, slots, &all, exp, span);
            self.note_async_args(d, &ck.args);
            self.call_throws(d, &ck.type_args, ck.ret, span);
            let kind = H::Call {
                callee: Callee::Def(d, ck.type_args),
                args: ck.args,
            };
            return self.mk(kind, ck.ret, span);
        }
        let ck = self.check_call(&c, slots, args, exp, span);
        let this_ty = self.cx.subst(this.ty, &ck.type_args);
        let recv = match this_arg {
            Some(t) => {
                let before = self.cx.diags.len();
                let h = self.expr(t, Some(this_ty), Want::Borrow);
                let h = self.coerce(h, this_ty);
                if self.cx.diags.len() != before {
                    return self.error_expr(span);
                }
                h
            }
            None if self.accepts_undefined(this_ty) => {
                let u = synth(ast::ExprKind::Lit(ast::Lit::Undefined), span);
                let h = self.expr(&u, Some(this_ty), Want::Move);
                self.coerce(h, this_ty)
            }
            None => {
                self.cx.error(
                    Diagnostic::error(
                        format!(
                            "the 'this' context of type 'void' is not assignable to method's 'this' of type '{}' (TypeScript's TS2684)",
                            self.cx.display(this_ty)
                        ),
                        span,
                    )
                    .with_note(format!(
                        "`{name}` declares the type of `this`; call it with one: `{name}.call(obj, ...)`, or as a method of an object"
                    )),
                );
                return self.error_expr(span);
            }
        };
        self.note_async_args(d, &ck.args);
        let recv = self.receiver(recv, Some(this_ty), this.mode, self.is_async_fn(d));
        let mut all = vec![recv];
        all.extend(ck.args);
        self.call_throws(d, &ck.type_args, ck.ret, span);
        let kind = H::Call {
            callee: Callee::Def(d, ck.type_args),
            args: all,
        };
        self.mk(kind, ck.ret, span)
    }

    /// May a plain call's `this` (`void`) be passed where type `t` is declared?
    fn accepts_undefined(&self, t: TyId) -> bool {
        matches!(self.cx.dynamic(t), Some(Dynamic::Any | Dynamic::Unknown))
    }

    /// `f` used as a value, when it is a function with a `this` parameter: not supported yet
    /// (a function value has no receiver to pass).
    pub(crate) fn this_fn_value(&mut self, d: DefId, name: &str, span: Span) -> Option<hir::Expr> {
        if !self.takes_this(d) {
            return None;
        }
        self.cx.error(
            Diagnostic::error(
                format!("`{name}`, a function with a `this` parameter, as a value is not supported yet"),
                span,
            )
            .with_note(format!(
                "TypeScript allows this; Velt doesn't yet because a function value has no receiver; call it: `{name}.call(obj, ...)`"
            )),
        );
        Some(self.error_expr(span))
    }
}
