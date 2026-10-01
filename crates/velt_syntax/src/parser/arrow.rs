//! Arrow functions: `x => e`, `(a, b: T): R => { ... }`, `(a): R throws E => e`, `async (x) => e`.
//!
//! `(`-started arrows are recognized by speculatively parsing a parameter list followed by `=>`;
//! on failure the parser rewinds and the `(` is parsed as a parenthesized expression.

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

/// Parameters, return type and `throws` clause of an arrow.
type ArrowHead = (Vec<ArrowParam>, Option<TypeExpr>, Option<TypeExpr>);

impl<'a> Parser<'a> {
    /// Parses an arrow function if one starts here; otherwise consumes nothing and returns `None`.
    pub(super) fn try_parse_arrow(&mut self) -> PResult<Option<Expr>> {
        let lo = self.cur_lo();
        let is_async = self.at_kw(Kw::Async)
            && (self.nth(1) == Tok::LParen
                || (Self::is_ident_like(self.nth(1)) && self.nth(2) == Tok::FatArrow));
        let off = usize::from(is_async);
        if Self::is_ident_like(self.nth(off)) && self.nth(off + 1) == Tok::FatArrow {
            if is_async {
                self.bump();
            }
            let name = self.take_ident();
            self.bump(); // =>
            let params = vec![ArrowParam { name, ty: None }];
            return self
                .finish_arrow(lo, (params, None, None), is_async)
                .map(Some);
        }
        if !self.may_start_arrow_params(off) {
            return Ok(None);
        }
        let head = self.speculate(|p| {
            if is_async {
                p.bump();
            }
            p.parse_arrow_head()
        });
        match head {
            Some(head) => self.finish_arrow(lo, head, is_async).map(Some),
            None => Ok(None),
        }
    }

    /// Cheap pre-check before speculating: `()` or `(name`, and the matching `)` is followed by
    /// `=>` or a return type's `:` (so a parenthesized expression is not tried as an arrow).
    fn may_start_arrow_params(&self, off: usize) -> bool {
        if self.nth(off) != Tok::LParen {
            return false;
        }
        let first = self.nth(off + 1);
        (first == Tok::RParen || Self::is_ident_like(first))
            && matches!(
                self.after_matching_paren(off),
                Some(Tok::FatArrow | Tok::Colon)
            )
    }

    /// `( params ) [: Ret [throws E]] =>`
    fn parse_arrow_head(&mut self) -> PResult<ArrowHead> {
        self.expect(Tok::LParen)?;
        let mut params = Vec::new();
        while !self.at(Tok::RParen) {
            self.reject_mut_modifier();
            let name = self.parse_ident()?;
            let ty = if self.eat(Tok::Colon) {
                Some(self.parse_type()?)
            } else {
                None
            };
            params.push(ArrowParam { name, ty });
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        let (ret, throws) = if self.eat(Tok::Colon) {
            (Some(self.parse_type()?), self.parse_throws_clause()?)
        } else {
            (None, None)
        };
        self.expect(Tok::FatArrow)?;
        Ok((params, ret, throws))
    }

    /// Body after `=>`: a block, or an assignment-level expression.
    fn finish_arrow(
        &mut self,
        lo: u32,
        (params, ret, throws): ArrowHead,
        is_async: bool,
    ) -> PResult<Expr> {
        let body = if self.at(Tok::LBrace) {
            ArrowBody::Block(self.guarded(|p| p.parse_block())?)
        } else {
            ArrowBody::Expr(Box::new(self.guarded(|p| p.parse_assign())?))
        };
        let span = self.span_from(lo);
        Ok(self.mk_expr(
            ExprKind::Arrow {
                params,
                ret,
                throws,
                body,
                is_async,
            },
            span,
        ))
    }
}
