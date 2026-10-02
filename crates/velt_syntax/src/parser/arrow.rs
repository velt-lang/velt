//! Arrow functions: `x => e`, `(a, b: T): R => { ... }`, `(a): R throws E => e`, `async (x) => e`,
//! and generic arrows `<T>(x: T) => x` (also `<T,>`, as `.tsx` requires). A `<` where an
//! expression starts is a generic arrow if `<T,`, `<T extends` or `<T =` follow, or if type
//! parameters, a parameter list and `=>` parse; otherwise the primary parser makes it JSX.
//!
//! `(`-started arrows are recognized by speculatively parsing a parameter list followed by `=>`;
//! on failure the parser rewinds and the `(` is parsed as a parenthesized expression.

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

/// Parameters, return type and `throws` clause of an arrow.
type ArrowHead = (Vec<ArrowParam>, Option<TypeExpr>, Option<TypeExpr>);

impl<'a> Parser<'a> {
    /// Can an arrow start at the cursor: `async`, `<` (type parameters), `(` or `name =>`? A
    /// cheap check before `try_parse_arrow`.
    #[inline]
    pub(super) fn may_start_arrow(&mut self) -> bool {
        match self.peek() {
            Tok::Lt | Tok::LParen | Tok::Kw(Kw::Async) => true,
            t => Self::is_ident_like(t) && self.nth(1) == Tok::FatArrow,
        }
    }

    /// Parses an arrow function if one starts here; otherwise consumes nothing and returns `None`.
    pub(super) fn try_parse_arrow(&mut self) -> PResult<Option<Expr>> {
        let lo = self.cur_lo();
        let async_kw = self.at_kw(Kw::Async);
        if self.nth(usize::from(async_kw)) == Tok::Lt {
            return self.try_generic_arrow(lo, async_kw);
        }
        let is_async = async_kw
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
                .finish_arrow(lo, vec![], (params, None, None), is_async)
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
            Some(head) => self.finish_arrow(lo, vec![], head, is_async).map(Some),
            None => Ok(None),
        }
    }

    /// `[async] <T, …>(params)[: R] => body` at the cursor, else nothing consumed and `None` (a
    /// JSX element, or `async < x`). `<T,`, `<T extends` and `<T =` can only be type parameters:
    /// those commit, so a broken arrow gets arrow diagnostics.
    fn try_generic_arrow(&mut self, lo: u32, is_async: bool) -> PResult<Option<Expr>> {
        let off = usize::from(is_async);
        let committed = Self::is_ident_like(self.nth(off + 1))
            && matches!(
                self.nth(off + 2),
                Tok::Comma | Tok::Kw(Kw::Extends) | Tok::Eq
            );
        if committed {
            if is_async {
                self.bump();
            }
            let type_params = self.parse_generic_params()?;
            let head = self.parse_arrow_head()?;
            return self.finish_arrow(lo, type_params, head, is_async).map(Some);
        }
        if !self.may_start_uncommitted_generic_arrow(off) {
            return Ok(None);
        }
        let parsed = self.speculate(|p| {
            if is_async {
                p.bump();
            }
            let type_params = p.parse_generic_params()?;
            if !p.may_start_arrow_params(0) {
                return Err(super::Fail);
            }
            Ok((type_params, p.parse_arrow_head()?))
        });
        match parsed {
            Some((type_params, head)) => {
                self.finish_arrow(lo, type_params, head, is_async).map(Some)
            }
            None => Ok(None),
        }
    }

    /// Cheap pre-check before speculating on a `<` at `pos + off` that did not commit: the only
    /// type parameter lists left are `<T>` and `<>`, and a parameter list must follow. So an
    /// element (`<p>text`, `<div class=…>`, `<a>{x}`) is ruled out without parsing anything.
    fn may_start_uncommitted_generic_arrow(&mut self, off: usize) -> bool {
        let gt = match self.nth(off + 1) {
            Tok::Gt => off + 1,
            t if Self::is_ident_like(t) && self.nth(off + 2) == Tok::Gt => off + 2,
            _ => return false,
        };
        self.nth(gt + 1) == Tok::LParen
    }

    /// Cheap pre-check before speculating: `()` or `(name`, and the matching `)` is followed by
    /// `=>` or a return type's `:` (so a parenthesized expression is not tried as an arrow).
    fn may_start_arrow_params(&mut self, off: usize) -> bool {
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
        type_params: Vec<GenericParam>,
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
                type_params,
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
