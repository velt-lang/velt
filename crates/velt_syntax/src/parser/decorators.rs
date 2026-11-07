//! Decorators (https://github.com/velt-lang/velt/issues/2203): `@x`, `@ns.x`, `@x(args)` and
//! `@(expr)` before a declaration, TC39's placement on classes and members, and Velt's extension
//! to plain functions, structs, enums and parameters. Which kind a decorator is, compile-time or
//! TC39 run-time, is name resolution's business (`velt_sema::decorators`), so both parse alike.

use super::members::Member;
use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// The decorators at the cursor, none in the usual case (which allocates nothing).
    pub(super) fn parse_decorators(&mut self) -> PResult<Vec<Decorator>> {
        let mut out = Vec::new();
        while self.at(Tok::At) {
            out.push(self.parse_decorator()?);
        }
        Ok(out)
    }

    /// At `@`. TC39's grammar: a member chain, a call of one, or a parenthesized expression, and
    /// TypeScript's type arguments on the call.
    fn parse_decorator(&mut self) -> PResult<Decorator> {
        let lo = self.cur_lo();
        self.bump(); // @
        let expr = if self.at(Tok::LParen) {
            self.parse_primary()?
        } else {
            self.decorator_chain()?
        };
        Ok(Decorator {
            span: self.span_from(lo),
            expr,
        })
    }

    fn decorator_chain(&mut self) -> PResult<Expr> {
        let lo = self.cur_lo();
        let head = self.parse_ident()?;
        let span = head.span;
        let mut expr = self.mk_expr(ExprKind::Ident(head), span);
        while self.eat(Tok::Dot) {
            let prop = self.parse_member_name()?;
            let kind = ExprKind::Member {
                object: Box::new(expr),
                prop,
                optional: false,
            };
            expr = self.mk_expr(kind, self.span_from(lo));
        }
        let type_args = match self.at(Tok::Lt) {
            true => self.parse_type_args()?,
            false => vec![],
        };
        if self.at(Tok::LParen) {
            let args = self.parse_args()?;
            let kind = ExprKind::Call {
                callee: Box::new(expr),
                type_args,
                args,
                optional: false,
            };
            expr = self.mk_expr(kind, self.span_from(lo));
        } else if let Some(t) = type_args.first() {
            // TypeScript's TS1146: type arguments only come with a call.
            self.error(
                "a decorator's type arguments must be followed by its arguments: `@x<T>(…)`",
                t.span,
            );
        }
        Ok(expr)
    }

    /// `export @y class C {}`: TC39 allows decorators on either side of `export`, not both.
    pub(super) fn decorators_after_export(&mut self, before: &mut Vec<Decorator>) -> PResult<()> {
        let after = self.parse_decorators()?;
        if let (Some(first), false) = (after.first(), before.is_empty()) {
            self.error(
                "decorators may come before `export` or after it, not both",
                first.span,
            );
        }
        before.extend(after);
        Ok(())
    }

    /// `@x const K = class {}`: a variable's decorators, though it holds a class (TypeScript's
    /// TS1206, as for any variable), report.
    pub(super) fn reject_class_binding_decorators(&mut self, ds: &[Decorator]) {
        if let Some(first) = ds.first() {
            self.report(
                velt_common::Diagnostic::error(
                    "a decorator cannot be applied to a variable, even one holding a class (TypeScript's TS1206)",
                    first.span,
                )
                .with_note("decorate a class declaration instead: `@x class K {}`"),
            );
        }
    }

    /// Give `kind` the decorators written before it; a declaration that can't have any reports.
    pub(super) fn attach_item_decorators(&mut self, kind: &mut ItemKind, ds: Vec<Decorator>) {
        let Some(first) = ds.first() else { return };
        let span = first.span;
        let slot = match kind {
            ItemKind::Function(f) => &mut f.sig.decorators,
            ItemKind::ExternFn(s) => &mut s.decorators,
            ItemKind::Struct(t) | ItemKind::Class(t) => &mut t.decorators,
            ItemKind::Enum(e) => &mut e.decorators,
            ItemKind::Import(_)
            | ItemKind::Interface(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::Var(_)
            | ItemKind::Extend(_)
            | ItemKind::Augment(_)
            | ItemKind::Namespace(_) => {
                self.report(
                    velt_common::Diagnostic::error(
                        "a decorator cannot be applied to this declaration",
                        span,
                    )
                    .with_note(
                        "decorators apply to functions, classes, structs, enums, their methods and fields, and parameters",
                    ),
                );
                return;
            }
        };
        // A function's implementation may have some of its own, written after its overloads.
        slot.splice(0..0, ds);
    }

    /// After an overload signature of `name`: the decorators before the next signature, which
    /// are the implementation's as all of a function's are, on either side of its `export`, and
    /// whether that `export` (with `default`, in `export default function`) is behind the cursor.
    pub(super) fn before_next_signature(
        &mut self,
        name: &Ident,
    ) -> PResult<(Vec<Decorator>, bool)> {
        let mut decorators = self.parse_decorators()?;
        let mut exported = self.eat_default_overload(name);
        if !exported && self.at_kw(Kw::Export) && self.nth(1) == Tok::At {
            self.bump();
            exported = true;
        }
        if exported {
            self.decorators_after_export(&mut decorators)?;
        }
        Ok((decorators, exported))
    }

    /// At `@` before a member of a `struct`, `class` or `extend` block.
    pub(super) fn parse_decorated_member(&mut self, lo: u32) -> PResult<Member> {
        let decorators = self.parse_decorators()?;
        let Some(first) = decorators.first().map(|d| d.span) else {
            return self.parse_member();
        };
        if self.at_static_block() {
            self.error("a decorator cannot be applied to a static block", first);
        }
        let mut member = self.parse_member()?;
        match &mut member {
            Member::Field(f) => {
                f.span.lo = lo;
                f.decorators.splice(0..0, decorators);
            }
            Member::Method(m) => {
                // Written before an overload signature, the member starts there; they belong to
                // the implementation either way.
                match m.decl.overloads.first_mut() {
                    Some(first) => first.span.lo = lo,
                    None => m.decl.sig.span.lo = lo,
                }
                m.decl.sig.decorators.splice(0..0, decorators);
            }
            Member::Constructor(..) => {
                self.error(
                    "a decorator cannot be applied to a constructor (TypeScript's TS1206)",
                    first,
                );
            }
        }
        Ok(member)
    }

    /// `p`, a parameter of a list that has no declaration to record them on (a function type, an
    /// interface method, a function expression): its decorators report.
    pub(super) fn reject_param_decorators(&mut self, p: &Param) {
        if let Some(first) = p.decorators.first() {
            self.error(
                "a decorator cannot be applied to this parameter: only the parameters of declared functions, methods and constructors may have decorators",
                first.span,
            );
        }
    }

    /// `sig`, an overload signature: no declaration of its own, so what its parameters' decorators
    /// say would go nowhere.
    pub(super) fn reject_overload_decorators(&mut self, sig: &FnSig) {
        for first in sig.params.iter().filter_map(|p| p.decorators.first()) {
            self.error(
                "a decorator cannot be applied to a parameter of an overload signature: write it on the implementation",
                first.span,
            );
        }
    }

    /// At `@` where an expression is expected: TC39 also allows `@x class {}` as an expression,
    /// which Velt has no form of; anything else is no JavaScript at all.
    pub(super) fn decorated_expression(&mut self) -> PResult<Expr> {
        let span = self.cur_span();
        self.report(velt_common::Diagnostic::error(TC39_NOT_SUPPORTED, span).with_note(
            "a decorator here would apply to a class expression: decorate a class declaration instead",
        ));
        Err(super::Fail)
    }
}
