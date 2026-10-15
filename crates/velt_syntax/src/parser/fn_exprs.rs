//! Function expressions (`function (x) { ... }`, `async function name() { ... }`) and `this`
//! parameters (`function f(this: T, x: A)`).
//!
//! A function expression that is not a generator is parsed as the arrow function with the same
//! parameters, result and body (`ExprKind::Arrow`): without `this` in its body the two behave
//! the same. Its span starts at `function` (or `async`), which tells `velt fmt` to print it as
//! written. `this` in the body is TypeScript's TS2683 without a `this` parameter, and not
//! supported yet with one; the expression's own name used inside its body (recursion) is not
//! supported yet either; so is `this` in one that is an object-literal property's value, where
//! TypeScript types `this` as the literal. Generators stay `ExprKind::Function` (sema's
//! `gen_closure`), with the same rules for `this`.
//!
//! A `this: T` parameter declares the type of `this` and is no parameter: it is parsed and
//! dropped (`first_this_param` remembers where the first one is, for `velt fmt`). With a `this`
//! parameter, `this` in the body is not supported yet.

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
            let mut sig = self.parse_sig_rest(lo, name, is_async)?;
            sig.is_generator = true;
            let this_param = self.this_param.take();
            let body = self.parse_fn_body()?;
            self.check_fn_this(this_param, &body, in_object);
            return Ok(ExprKind::Function(Box::new(FnDecl {
                sig,
                body,
                overloads: vec![],
            })));
        }
        let type_params = self.parse_generic_params()?;
        let params = self.parse_arrow_params()?;
        let this_param = self.this_param.take();
        let (ret, throws) = if self.eat(Tok::Colon) {
            (Some(self.parse_ret_type()?), self.parse_throws_clause()?)
        } else {
            (None, None)
        };
        let body = self.parse_fn_body()?;
        self.check_fn_this(this_param, &body, in_object);
        if !name.name.is_empty() {
            self.no_own_name(&name, &body);
        }
        Ok(ExprKind::Arrow {
            type_params,
            params,
            ret,
            throws,
            body: ArrowBody::Block(body),
            is_async,
        })
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
        let void = matches!(ty.kind, TypeExprKind::Void);
        if self.first_this_param.is_none() {
            let at = self.span_from(lo);
            self.first_this_param = Some(at.lo..at.hi);
        }
        self.this_param = Some((self.span_from(lo), void));
        if !self.at(Tok::RParen) {
            self.expect(Tok::Comma)?;
        }
        Ok(())
    }

    /// `this` in the body of a function expression (or, with a `this` parameter `this_param`, a
    /// function declaration); `in_object`: the expression is the value of an object-literal
    /// property (TypeScript types `this` as the literal there).
    pub(super) fn check_fn_this(
        &mut self,
        this_param: Option<(Span, bool)>,
        body: &Block,
        in_object: bool,
    ) {
        let Some(at) = own_this(body) else {
            return;
        };
        let d = match this_param {
            None if in_object => Diagnostic::error(
                "`this` in a function expression that is an object-literal property is not supported yet",
                at,
            )
            .with_note("TypeScript allows this (`this` is the object the function is called on, typed as the literal); Velt doesn't yet because a function value has no receiver; use the object's variable, or declare a class"),
            None => Diagnostic::error(
                "'this' implicitly has type 'any' because it does not have a type annotation (TypeScript's TS2683)",
                at,
            )
            .with_note("a function's `this` is the object it is called on; use a parameter for that object, an arrow function inside a method (whose `this` is the method's), or a class method"),
            Some((_, true)) => Diagnostic::error(
                "`this` is `void` here: the function declares `this: void`",
                at,
            ),
            Some((p, false)) => Diagnostic::error(
                "`this` in a function with a `this` parameter is not supported yet",
                at,
            )
            .with_note(format!(
                "TypeScript allows this (`this` is the object the function is called on, typed by `{}`); Velt doesn't yet because a function value has no receiver; take the object as an ordinary parameter, or make the function a class method",
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

/// The first `this` of `body` outside the classes and functions declared in it.
fn own_this(body: &Block) -> Option<Span> {
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
            match e.kind {
                ExprKind::This => self.this.push(e.span),
                ExprKind::Function(_) => self.items.push(e.span),
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
