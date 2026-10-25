//! Function expressions (`function (x) { ... }`, `async function name() { ... }`) and `this`
//! parameters (`function f(this: T, x: A)`).
//!
//! A function expression that is not a generator is parsed as the arrow function with the same
//! parameters, result and body (`ExprKind::Arrow`): without `this` in its body the two behave
//! the same. Its span starts at `function` (or `async`), which tells `velt fmt` to print it as
//! written. `this` in the body is TypeScript's TS2683 without a `this` parameter, except in one
//! that is an object-literal property's value, where TypeScript types `this` as the literal: it
//! gets the `METHOD_THIS` parameter, as a method does. The expression's own name used inside its
//! body (recursion) is not supported yet. Generators stay `ExprKind::Function` (sema's
//! `gen_closure`), with the same rules for `this`.
//!
//! A `this: T` parameter declares the type of `this` and is no parameter: callers pass the
//! receiver. A function declaration keeps `T` in `FnSig::this_param` (unless it is `void`); so
//! does a function expression whose body uses `this`, which is then an `ExprKind::Function`
//! (its parameters must have written types). `first_this_param` remembers where the first
//! `this` parameter is, for `velt fmt`.

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use velt_common::{Diagnostic, Span};

impl<'a> Parser<'a> {
    /// A function expression at `function` (after `async`, with `is_async`): `function[*]
    /// [name]<T>(params)[: R] { ... }`; the name may be left out (empty).
    pub(super) fn parse_fn_expr(&mut self, lo: u32, is_async: bool) -> PResult<ExprKind> {
        let in_object = std::mem::take(&mut self.fn_in_object);
        self.expect_kw(Kw::Function, "function")?;
        let is_generator = self.eat(Tok::Star);
        let name = match Self::is_ident_like(self.peek()) {
            true => self.parse_binding_ident()?,
            false => Ident {
                name: String::new(),
                span: self.span_from(lo),
            },
        };
        if is_generator {
            let mut sig = self.parse_sig_rest(lo, name, is_async, true)?;
            sig.this_param = None;
            let this_param = self.this_param.take();
            let body = self.parse_fn_body()?;
            if in_object && this_param.is_none() {
                self.method_this_param(lo, &mut sig.params, &body);
            }
            self.check_fn_this(this_param, &body, in_object);
            return Ok(ExprKind::Function(Box::new(FnDecl {
                sig,
                body,
                overloads: vec![],
            })));
        }
        let type_params = self.parse_generic_params()?;
        let mut params = self.parse_arrow_params()?;
        let this_param = self.this_param.take();
        let (ret, throws) = if self.eat(Tok::Colon) {
            (Some(self.parse_ret_type()?), self.parse_throws_clause()?)
        } else {
            (None, None)
        };
        let body = self.parse_fn_body()?;
        if !name.name.is_empty() {
            self.no_own_name(&name, &body);
        }
        // With a `this` parameter that the body uses, the function keeps its `this` type, so
        // its parameters must be written (sema checks it).
        if let Some((p, ty)) = &this_param {
            let typed: Option<Vec<Param>> = params.iter().map(|a| typed_param(a, p)).collect();
            if let (false, Some(typed)) = (is_void(ty), typed) {
                if own_this(&body).is_some() {
                    let sig = FnSig {
                        name,
                        generics: type_params,
                        params: typed,
                        ret,
                        throws,
                        is_async,
                        is_generator: false,
                        this_param: Some(Box::new(ty.clone())),
                        span: self.span_from(lo),
                    };
                    return Ok(ExprKind::Function(Box::new(FnDecl {
                        sig,
                        body,
                        overloads: vec![],
                    })));
                }
            }
        }
        if in_object && this_param.is_none() {
            self.method_this_arrow_param(lo, &mut params, &body);
        }
        self.check_fn_this(this_param, &body, in_object);
        Ok(ExprKind::Arrow {
            type_params,
            params,
            ret,
            throws,
            body: ArrowBody::Block(body),
            is_async,
        })
    }

    /// The type of `this` declared by the `this` parameter of the parameter list parsed last,
    /// unless it is `void` (`None` without one).
    pub(super) fn this_type(&self) -> Option<Box<TypeExpr>> {
        let (_, ty) = self.this_param.as_ref()?;
        (!is_void(ty)).then(|| Box::new(ty.clone()))
    }

    /// `this: T` as the first parameter (the cursor after `(`): parsed with its `,` and
    /// remembered in `this_param` (cleared otherwise).
    pub(super) fn parse_this_param(&mut self) -> PResult<()> {
        self.this_param = None;
        if !(self.at(Tok::Kw(Kw::This)) && self.nth(1) == Tok::Colon) {
            return Ok(());
        }
        let lo = self.cur_lo();
        self.bump(); // this
        self.bump(); // :
        let ty = self.parse_type()?;
        if self.first_this_param.is_none() {
            let at = self.span_from(lo);
            self.first_this_param = Some(at.lo..at.hi);
        }
        self.this_param = Some((self.span_from(lo), ty));
        if !self.at(Tok::RParen) {
            self.expect(Tok::Comma)?;
        }
        Ok(())
    }

    /// `this` in the body of a function expression (or, with a `this: void` parameter, a
    /// function declaration); `in_object`: the expression is the value of an object-literal
    /// property (TypeScript types `this` as the literal there). The type checker handles a
    /// `this` of another type.
    pub(super) fn check_fn_this(
        &mut self,
        this_param: Option<(Span, TypeExpr)>,
        body: &Block,
        in_object: bool,
    ) {
        let Some(at) = own_this(body) else {
            return;
        };
        let d = match this_param {
            // The object the function is called on: a `METHOD_THIS` parameter.
            None if in_object => return,
            None => Diagnostic::error(
                "'this' implicitly has type 'any' because it does not have a type annotation (TypeScript's TS2683)",
                at,
            )
            .with_note("a function's `this` is the object it is called on; use a parameter for that object, an arrow function inside a method (whose `this` is the method's), or a class method"),
            Some((_, ty)) if is_void(&ty) => Diagnostic::error(
                "`this` is `void` here: the function declares `this: void`",
                at,
            ),
            // A generator, a function with overloads or a function expression whose parameters
            // are not all written.
            Some((p, _)) => Diagnostic::error(
                "`this` in a generator, or in a function with overloads or parameters without a written type, that has a `this` parameter is not supported yet",
                at,
            )
            .with_note(format!(
                "TypeScript allows this (`this` is typed by `{}`); take the object as an ordinary parameter, or write a single signature and the type of each parameter",
                self.text(p.lo, p.hi)
            )),
        };
        self.diags.push(d);
    }

    /// Reports the name of function expression `name` used inside its own body.
    fn no_own_name(&mut self, name: &Ident, body: &Block) {
        struct Find<'n>(&'n str, Option<Span>);
        impl<'a> crate::visit::Visit<'a> for Find<'_> {
            fn expr(&mut self, e: &'a Expr) {
                if let ExprKind::Ident(i) = &e.kind {
                    if i.name == self.0 && self.1.is_none() {
                        self.1 = Some(i.span);
                    }
                }
            }
        }
        let mut f = Find(&name.name, None);
        for s in &body.stmts {
            crate::visit::walk_stmt(s, &mut f);
        }
        if let Some(at) = f.1 {
            self.diags.push(
                Diagnostic::error(
                    format!(
                        "a function expression's own name (`{}`) inside its body is not supported yet",
                        name.name
                    ),
                    at,
                )
                .with_note("TypeScript allows this (the name refers to the function itself, for recursion); declare the function with `function name(...) { ... }` before its use instead"),
            );
        }
    }
}

/// Is `ty` `void`?
fn is_void(ty: &TypeExpr) -> bool {
    matches!(ty.kind, TypeExprKind::Void)
}

/// `a` as the parameter of a declaration, if its type is written (`this_at`: the `this`
/// parameter, for the spans of the rest).
fn typed_param(a: &ArrowParam, this_at: &Span) -> Option<Param> {
    let ty = a.ty.clone()?;
    let span = Span::new(this_at.file, a.name.span.lo, ty.span.hi.max(a.name.span.hi));
    Some(Param {
        name: a.name.clone(),
        pattern: a.pattern.clone(),
        ty,
        default: a.default.clone(),
        optional: a.optional,
        rest: a.rest,
        span,
    })
}

/// The first `this` of `body` outside the classes and functions declared in it (object-literal
/// methods included).
pub(super) fn own_this(body: &Block) -> Option<Span> {
    #[derive(Default)]
    struct Find {
        this: Vec<Span>,
        items: Vec<Span>,
    }
    impl<'a> crate::visit::Visit<'a> for Find {
        fn stmt(&mut self, s: &'a Stmt) {
            if let StmtKind::Item(_) = s.kind {
                self.items.push(s.span);
            }
        }
        fn expr(&mut self, e: &'a Expr) {
            match &e.kind {
                ExprKind::This => self.this.push(e.span),
                ExprKind::Function(_) => self.items.push(e.span),
                // A nested object-literal method: its `this` is its own object.
                ExprKind::Arrow { params, .. }
                    if params.first().is_some_and(|p| is_method_this(&p.name)) =>
                {
                    self.items.push(e.span)
                }
                _ => {}
            }
        }
    }
    let mut f = Find::default();
    for s in &body.stmts {
        crate::visit::walk_stmt(s, &mut f);
    }
    let inside = |at: &Span| f.items.iter().any(|i| i.lo <= at.lo && at.hi <= i.hi);
    f.this.iter().copied().find(|at| !inside(at))
}
