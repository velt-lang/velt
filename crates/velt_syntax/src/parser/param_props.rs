//! Constructor parameter properties (TS): `constructor(private readonly mass: f64) {}` declares
//! the field `mass` and assigns the parameter to it. Desugared here into an ordinary field
//! plus `this.mass = mass;` at the start of the constructor body, or right after its
//! top-level `super(...)` call (sema requires that call to come first in such a class).

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

/// The field a parameter property declares (`private` / `readonly` as written; `public` and
/// `protected` give a public field, since Velt has no `protected`).
struct PropMods {
    any: bool,
    readonly: bool,
    is_private: bool,
}

impl<'a> Parser<'a> {
    /// Constructor parameters, with the fields their property modifiers declare.
    pub(super) fn parse_ctor_params(&mut self) -> PResult<(Vec<Param>, Vec<Field>)> {
        self.expect(Tok::LParen)?;
        let (mut params, mut fields) = (vec![], vec![]);
        while !self.at(Tok::RParen) {
            let lo = self.cur_lo();
            let mods = self.parse_prop_mods();
            let mut p = self.parse_param()?;
            // The span covers the modifiers, so `velt fmt` keeps them.
            p.span.lo = lo;
            if mods.any {
                fields.push(Field {
                    name: p.name.clone(),
                    ty: p.ty.clone(),
                    default: None,
                    readonly: mods.readonly,
                    optional: false,
                    is_private: mods.is_private,
                    is_static: false,
                    span: p.span,
                });
            }
            params.push(p);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        Ok((params, fields))
    }

    /// `public` / `private` / `protected` / `readonly` before a parameter name.
    fn parse_prop_mods(&mut self) -> PropMods {
        let mut m = PropMods {
            any: false,
            readonly: false,
            is_private: false,
        };
        while Self::is_name(self.nth(1)) {
            if self.at_word("private") {
                m.is_private = true;
            } else if self.at_kw(Kw::Readonly) {
                m.readonly = true;
            } else if !self.at_word("public") && !self.at_word("protected") {
                break;
            }
            m.any = true;
            self.bump();
        }
        m
    }

    /// Insert `this.<field> = <field>;` for each parameter property into the constructor body:
    /// right after its root-level `super(...);` (statements before it may not use `this`), else
    /// at the start.
    pub(super) fn store_param_props(&mut self, body: &mut Block, fields: &[Field]) {
        let at = body
            .stmts
            .iter()
            .position(is_super_call)
            .map_or(0, |i| i + 1);
        let stores: Vec<Stmt> = fields.iter().map(|f| self.store_prop(f)).collect();
        body.stmts.splice(at..at, stores);
    }

    fn store_prop(&mut self, f: &Field) -> Stmt {
        let span = f.name.span;
        let this = self.mk_expr(ExprKind::This, span);
        let target = self.mk_expr(
            ExprKind::Member {
                object: Box::new(this),
                prop: f.name.clone(),
                optional: false,
            },
            span,
        );
        let value = self.mk_expr(ExprKind::Ident(f.name.clone()), span);
        let assign = self.mk_expr(
            ExprKind::Assign {
                op: None,
                target: Box::new(target),
                value: Box::new(value),
            },
            span,
        );
        Stmt {
            kind: StmtKind::Expr(assign),
            span,
        }
    }
}

/// `super(...);`
fn is_super_call(s: &Stmt) -> bool {
    matches!(&s.kind, StmtKind::Expr(e) if matches!(&e.kind,
        ExprKind::Call { callee, .. } if matches!(callee.kind, ExprKind::Super)))
}
