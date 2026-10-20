//! The elements of a tuple type: `T`, an optional element (`T?`, `name?: T`) and a rest element
//! (`...T`), with TypeScript's errors for their order.

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::Tok;

impl<'a> Parser<'a> {
    /// Tuple element types up to (not including) `]`: `T`, an optional element (`T?`) or a
    /// rest element (`...T`). Element labels (`[kind: string, b64: string]`, `[name?: T]`,
    /// `[...rest: T[]]`) are documentation in TypeScript and are dropped.
    pub(super) fn parse_tuple_elems(&mut self) -> PResult<Vec<TypeExpr>> {
        let mut out = Vec::new();
        let mut seen_optional = false;
        let mut seen_rest = false;
        while !self.at(Tok::RBracket) {
            let lo = self.cur_lo();
            let rest = self.eat(Tok::DotDotDot);
            let mut optional = false;
            if Self::is_name(self.peek()) && self.nth(1) == Tok::Colon {
                self.bump();
                self.bump();
            } else if Self::is_name(self.peek())
                && self.nth(1) == Tok::Question
                && self.nth(2) == Tok::Colon
            {
                self.bump();
                self.bump();
                self.bump();
                optional = true;
            }
            let mut ty = self.parse_type()?;
            if !optional && self.at(Tok::Question) {
                self.bump();
                optional = true;
            }
            let span = self.span_from(lo);
            if rest && optional {
                self.error("a tuple member cannot be both optional and rest", span);
            } else if optional && seen_rest {
                self.error("an optional element cannot follow a rest element", span);
            } else if !optional && !rest && seen_optional {
                self.error("a required element cannot follow an optional element", span);
            }
            seen_optional |= optional;
            seen_rest |= rest;
            if rest {
                ty = TypeExpr {
                    kind: TypeExprKind::TupleRest(Box::new(ty)),
                    span,
                };
            } else if optional {
                ty = TypeExpr {
                    kind: TypeExprKind::TupleOptional(Box::new(ty)),
                    span,
                };
            }
            out.push(ty);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        Ok(out)
    }
}
