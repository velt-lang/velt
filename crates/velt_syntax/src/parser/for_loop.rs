//! `for` statements: C-style `for (init; cond; update)`, `for (const x of xs)` and
//! `for await (const x of xs)`.
//!
//! Comma lists in the head (`for (let i = 0, j = n; i < j; i++, j--)`) have no AST node of
//! their own; the parser desugars them into constructs the rest of the compiler knows:
//! - several declarations / init expressions become statements in a block around the loop
//!   (`{ let i = 0; let j = n; for (; …; …) … }`); a label on the loop moves onto the inner
//!   `for` (`parse_labeled_or_expr_stmt`), so `continue label` still runs the update;
//! - several update expressions become an immediately called arrow `(() => { i++; j--; })()`
//!   (captured variables are borrowed, so the updates act on the loop variables).
//!
//! Both desugarings are recognizable, so `velt fmt` prints the comma form back: the block
//! starts at the `for` keyword (a source block starts at `{`), and the synthesized arrow and
//! its parentheses have empty spans (a source expression's span is never empty).

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// C-style `for (init; cond; update)`, `for (const x of xs)` or `for await (const x of xs)`.
    pub(super) fn parse_for(&mut self) -> PResult<StmtKind> {
        let lo = self.cur_lo();
        self.bump(); // for
        let await_span = self.at_kw(Kw::Await).then(|| self.cur_span());
        if await_span.is_some() {
            self.bump();
        }
        self.expect(Tok::LParen)?;
        let init = match self.parse_for_init()? {
            ForInit::Of(kind, pattern) => {
                return self.finish_for_of(kind, pattern, await_span.is_some())
            }
            ForInit::Stmts(stmts) => stmts,
        };
        if let Some(span) = await_span {
            self.error(
                "`for await` needs `of`: write `for await (const x of source)`",
                span,
            );
            return Err(Fail);
        }
        let cond = if self.at(Tok::Semi) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect_semi()?;
        let update = if self.at(Tok::RParen) {
            None
        } else {
            Some(self.parse_for_update()?)
        };
        self.expect(Tok::RParen)?;
        let body = self.parse_body()?;
        Ok(self.finish_for(lo, init, cond, update, body))
    }

    /// The loop, or a block holding the init statements and then the loop.
    fn finish_for(
        &mut self,
        lo: u32,
        mut init: Vec<Stmt>,
        cond: Option<Expr>,
        update: Option<Expr>,
        body: Block,
    ) -> StmtKind {
        let single = init.len() <= 1;
        let kind = StmtKind::For {
            init: if single {
                init.pop().map(Box::new)
            } else {
                None
            },
            cond,
            update,
            body,
        };
        if single {
            return kind;
        }
        let span = self.span_from(lo);
        init.push(Stmt { kind, span });
        StmtKind::Block(Block { stmts: init, span })
    }

    /// Everything up to and including the first `;`, or the `x of` of a `for...of`.
    fn parse_for_init(&mut self) -> PResult<ForInit> {
        if self.eat(Tok::Semi) {
            return Ok(ForInit::Stmts(vec![]));
        }
        let mut stmts = vec![];
        if self.at_kw(Kw::Let) || self.at_kw(Kw::Const) {
            let kind = if self.at_kw(Kw::Const) {
                VarKind::Const
            } else {
                VarKind::Let
            };
            self.bump();
            loop {
                let lo = self.cur_lo();
                let pattern = self.parse_binding_pattern()?;
                if stmts.is_empty() && self.eat_kw(Kw::Of) {
                    return Ok(ForInit::Of(kind, pattern));
                }
                let decl = self.finish_var_decl(lo, kind, pattern)?;
                stmts.push(Stmt {
                    span: decl.span,
                    kind: StmtKind::Var(decl),
                });
                if !self.eat(Tok::Comma) {
                    break;
                }
            }
        } else {
            loop {
                let e = self.parse_expr()?;
                if self.at_kw(Kw::Of) {
                    let span = self.cur_span();
                    self.error(
                        "`for...of` requires `const` or `let` before the loop variable",
                        span,
                    );
                    return Err(Fail);
                }
                stmts.push(Stmt {
                    span: e.span,
                    kind: StmtKind::Expr(e),
                });
                if !self.eat(Tok::Comma) {
                    break;
                }
            }
        }
        self.expect_semi()?;
        Ok(ForInit::Stmts(stmts))
    }

    /// `update` or `u1, u2, …` (desugared to `(() => { u1; u2; … })()`).
    fn parse_for_update(&mut self) -> PResult<Expr> {
        let first = self.parse_expr()?;
        if !self.at(Tok::Comma) {
            return Ok(first);
        }
        let mut stmts = vec![first];
        while self.eat(Tok::Comma) {
            stmts.push(self.parse_expr()?);
        }
        let lo = stmts[0].span.lo;
        let span = self.span_from(lo);
        let empty = velt_common::Span::new(span.file, lo, lo);
        let stmts = stmts
            .into_iter()
            .map(|e| Stmt {
                span: e.span,
                kind: StmtKind::Expr(e),
            })
            .collect();
        let arrow = ExprKind::Arrow {
            type_params: vec![],
            params: vec![],
            ret: None,
            throws: None,
            body: ArrowBody::Block(Block { stmts, span }),
            is_async: false,
        };
        let arrow = self.mk_expr(arrow, empty);
        let callee = self.mk_expr(ExprKind::Paren(Box::new(arrow)), empty);
        let call = ExprKind::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args: vec![],
            optional: false,
        };
        Ok(self.mk_expr(call, span))
    }

    fn finish_for_of(
        &mut self,
        kind: VarKind,
        pattern: Pattern,
        is_await: bool,
    ) -> PResult<StmtKind> {
        let iter = self.parse_expr()?;
        self.expect(Tok::RParen)?;
        let body = self.parse_body()?;
        Ok(StmtKind::ForOf {
            kind,
            pattern,
            iter,
            body,
            is_await,
        })
    }
}

/// What precedes the condition of a `for`.
enum ForInit {
    /// `for (<kind> <pattern> of …`
    Of(VarKind, Pattern),
    /// C-style init statements (none, one, or a comma list), `;` consumed.
    Stmts(Vec<Stmt>),
}
