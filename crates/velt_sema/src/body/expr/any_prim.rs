//! JavaScript's `ToPrimitive(v, hint)` of an object held in an `any` (#924), the conversion
//! behind arithmetic, `+`, the relational operators and `==` on objects. The prelude's
//! `anyPrimitive(v, hint, at)` (std/lang/hooks/any_ops.vlt) ends with
//! `anyToPrimitive(v, hint, at)`, which sema writes here as a dispatch over the
//! program's classes, as `anyString` holds the dispatch of `String(x)` (any_text.rs):
//! - a class with a `[Symbol.toPrimitive]` is what that returns for `hint`;
//! - another one tries its `valueOf()` and `toString()` (`toString()` first for `"string"`),
//!   and the first primitive result is kept as it is (a number stays a number); a method whose
//!   result type is a primitive ends the search, another one is tested at run time;
//! - without a `toString()` of its own, `Object.prototype.toString` writes it last.
//!
//! An object result of the last method is Node's `TypeError`. Every other value goes to the
//! prelude's `anyPrimitivePlain` (arrays, object literals, functions).

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, DefId, ExprKind as H, TyId};

/// `name` as an identifier expression.
fn ident(name: &str, span: Span) -> ast::Expr {
    let id = ast::Ident {
        name: name.into(),
        span,
    };
    synth(ast::ExprKind::Ident(id), span)
}

/// `f(args)`.
fn call(f: &str, args: Vec<ast::Expr>, span: Span) -> ast::Expr {
    let kind = ast::ExprKind::Call {
        callee: Box::new(ident(f, span)),
        type_args: vec![],
        args,
        optional: false,
    };
    synth(kind, span)
}

/// `recv.name()`.
fn method0(recv: ast::Expr, name: &str, span: Span) -> ast::Expr {
    let prop = ast::Ident {
        name: name.into(),
        span,
    };
    let member = ast::ExprKind::Member {
        object: Box::new(recv),
        prop,
        optional: false,
    };
    let kind = ast::ExprKind::Call {
        callee: Box::new(synth(member, span)),
        type_args: vec![],
        args: vec![],
        optional: false,
    };
    synth(kind, span)
}

/// How an instance of a class converts.
enum PrimMethod {
    /// A `Proxy`, whose traps the conversion would run (not yet).
    Proxy,
    /// Its `[Symbol.toPrimitive]`, which takes the hint when it has a parameter.
    ToPrimitive(bool),
    /// `OrdinaryToPrimitive`: its `toString()` and `valueOf()` (each with whether its result
    /// type is a primitive), and whether `toString` is its own (else `Object.prototype`'s).
    Ordinary {
        to_string: Option<bool>,
        value_of: Option<bool>,
    },
}

impl FnCx<'_, '_> {
    /// `anyToPrimitive(v, hint, at)` in the hook `anyPrimitive`: the
    /// dispatch (module docs).
    pub(super) fn primitive_dispatch(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let (Some(any), [v, ..]) = (self.any_ty(), args) else {
            return self.error_expr(span);
        };
        let v = self.expr(v, None, Want::Borrow);
        let init = self.share_if_place(v);
        self.with_temp("<prim>", init, span, span, |lw, v| {
            let s = lw.expr(v, None, Want::Borrow);
            let fallback = vec![ident("hint", span), ident("at", span)];
            lw.dynamic_class_match_where(
                s,
                any,
                (velt_common::std_hook!(any_primitive_plain), fallback),
                |lw, c| lw.prim_method(c).is_some(),
                |lw, c, read| match lw.prim_method(c) {
                    Some(m) => lw.prim_method_call(&m, read, any, span),
                    None => lw.error_expr(span),
                },
                span,
            )
        })
    }

    /// The conversion of class `c`'s instances, if it has a method for it (a `Date` is the
    /// prelude's own).
    fn prim_method(&mut self, c: DefId) -> Option<PrimMethod> {
        if self.cx.prelude_adt("Proxy") == Some(c) {
            return Some(PrimMethod::Proxy);
        }
        let date = self.cx.prelude_adt("Date");
        if date.is_some_and(|d| self.cx.class_extends(c, d)) {
            return None;
        }
        let n = self.cx.adt(c).map_or(0, |a| a.generics.len());
        let params: Vec<_> = (0..n as u32).map(|i| self.cx.ty.param(i)).collect();
        let to_primitive = self.callable_method(c, &params, "[Symbol.toPrimitive]");
        if let Some((_, arity)) = to_primitive.filter(|&(_, arity)| arity <= 1) {
            return Some(PrimMethod::ToPrimitive(arity == 1));
        }
        let mut method = |name: &str| {
            let (ret, arity) = self.callable_method(c, &params, name)?;
            (arity == 0).then(|| self.primitive_ty(ret))
        };
        let to_string = method("toString");
        let value_of = method("valueOf");
        if to_string.is_none() && value_of.is_none() {
            return None;
        }
        Some(PrimMethod::Ordinary {
            to_string,
            value_of,
        })
    }

    /// The primitive of the class instance `read` as `m` says, an `any`.
    fn prim_method_call(
        &mut self,
        m: &PrimMethod,
        read: ast::Expr,
        any: TyId,
        span: Span,
    ) -> hir::Expr {
        let at = || ident("at", span);
        let e = match m {
            PrimMethod::Proxy => {
                let lit = synth(ast::ExprKind::Lit(ast::Lit::Str("Proxy".into())), span);
                call(
                    velt_common::std_hook!(any_text_unsupported),
                    vec![lit, at()],
                    span,
                )
            }
            PrimMethod::ToPrimitive(takes_hint) => {
                let args = match takes_hint {
                    true => vec![ident("hint", span)],
                    false => vec![],
                };
                let prop = ast::Ident {
                    name: "[Symbol.toPrimitive]".into(),
                    span,
                };
                let member = ast::ExprKind::Member {
                    object: Box::new(read),
                    prop,
                    optional: false,
                };
                let kind = ast::ExprKind::Call {
                    callee: Box::new(synth(member, span)),
                    type_args: vec![],
                    args,
                    optional: false,
                };
                call(
                    velt_common::std_hook!(any_prim_keep),
                    vec![synth(kind, span), at()],
                    span,
                )
            }
            &PrimMethod::Ordinary {
                to_string,
                value_of,
            } => {
                let ts = to_string.map(|p| ("toString", p));
                let vo = value_of.map(|p| ("valueOf", p));
                let inherited = to_string.is_none();
                let string: Vec<_> = ts.into_iter().chain(vo).collect();
                let default: Vec<_> = vo.into_iter().chain(ts).collect();
                // Without a `toString` of its own, `Object.prototype.toString` writes it first
                // for `"string"`.
                let string_chain = match inherited {
                    true => call(
                        velt_common::std_hook!(any_text),
                        vec![read.clone(), at()],
                        span,
                    ),
                    false => self.prim_chain(&string, false, &read, span),
                };
                let default_chain = self.prim_chain(&default, inherited, &read, span);
                if string == default && !inherited {
                    let h = self.expr(&default_chain, None, Want::Move);
                    return self.coerce(h, any);
                }
                let s = self.expr(&string_chain, None, Want::Move);
                let s = self.coerce(s, any);
                let d = self.expr(&default_chain, None, Want::Move);
                let d = self.coerce(d, any);
                let is_string = synth(
                    ast::ExprKind::Binary {
                        op: ast::BinaryOp::StrictEq,
                        lhs: Box::new(ident("hint", span)),
                        rhs: Box::new(synth(
                            ast::ExprKind::Lit(ast::Lit::Str("string".into())),
                            span,
                        )),
                    },
                    span,
                );
                let cond = self.expr(&is_string, None, Want::Borrow);
                let kind = H::If {
                    cond: Box::new(cond),
                    then: Box::new(s),
                    els: Box::new(d),
                };
                return self.mk(kind, any, span);
            }
        };
        let h = self.expr(&e, None, Want::Move);
        self.coerce(h, any)
    }

    /// The methods `ms` tried in turn on `read` (each with whether its result type is a
    /// primitive), then `Object.prototype.toString` when `inherited`.
    fn prim_chain(
        &mut self,
        ms: &[(&str, bool)],
        inherited: bool,
        read: &ast::Expr,
        span: Span,
    ) -> ast::Expr {
        let Some((&(name, primitive), rest)) = ms.split_first() else {
            return call(
                velt_common::std_hook!(any_text),
                vec![read.clone(), ident("at", span)],
                span,
            );
        };
        let c = method0(read.clone(), name, span);
        if primitive {
            return c;
        }
        if rest.is_empty() && !inherited {
            return call(
                velt_common::std_hook!(any_prim_keep),
                vec![c, ident("at", span)],
                span,
            );
        }
        let next = self.prim_chain(rest, inherited, read, span);
        let arrow = ast::ExprKind::Arrow {
            type_params: vec![],
            params: vec![],
            ret: None,
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(next)),
            is_async: false,
        };
        call(
            velt_common::std_hook!(any_prim_then),
            vec![c, synth(arrow, span)],
            span,
        )
    }
}
