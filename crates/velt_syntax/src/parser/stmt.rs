//! Statements and blocks, with statement-level error recovery.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// `{ stmts }`. A missing `}` at end of file is reported and the partial block is returned.
    pub(super) fn parse_block(&mut self) -> PResult<Block> {
        let lo = self.cur_lo();
        self.expect(Tok::LBrace)?;
        let mut stmts = Vec::new();
        loop {
            match self.peek() {
                Tok::RBrace => {
                    self.bump();
                    break;
                }
                Tok::Eof => {
                    self.error_expected("`}`");
                    break;
                }
                _ => {}
            }
            self.parse_stmt_recovering(&mut stmts);
        }
        Ok(Block {
            stmts,
            span: self.span_from(lo),
        })
    }

    /// One statement into `out`; on a syntax error, skips to the next statement boundary.
    pub(super) fn parse_stmt_recovering(&mut self, out: &mut Vec<Stmt>) {
        let start = self.pos;
        match self.parse_stmt() {
            Ok(s) => out.push(s),
            Err(Fail) => {
                self.sync_stmt(start);
                if self.pos == start {
                    self.bump();
                }
            }
        }
    }

    /// Body of `if`/`while`/`for`/`do`: a block, or a single statement wrapped in a block.
    pub(super) fn parse_body(&mut self) -> PResult<Block> {
        if self.at(Tok::LBrace) {
            return self.parse_block();
        }
        let s = self.parse_stmt()?;
        Ok(Block {
            span: s.span,
            stmts: vec![s],
        })
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        self.guarded(|p| {
            let lo = p.cur_lo();
            let kind = p.parse_stmt_kind()?;
            Ok(Stmt {
                kind,
                span: p.span_from(lo),
            })
        })
    }

    fn parse_stmt_kind(&mut self) -> PResult<StmtKind> {
        if self.at_using_decl() {
            let var = self.parse_using_decl()?;
            self.expect_semi()?;
            return Ok(StmtKind::Var(var));
        }
        let Some(kw) = self.cur_kw() else {
            return match self.peek() {
                Tok::LBrace => Ok(StmtKind::Block(self.parse_block()?)),
                Tok::Semi => {
                    self.bump();
                    Ok(StmtKind::Empty)
                }
                _ => self.parse_labeled_or_expr_stmt(),
            };
        };
        match kw {
            Kw::Let | Kw::Const => {
                let var = self.parse_var_decl()?;
                self.expect_semi()?;
                Ok(StmtKind::Var(var))
            }
            Kw::If => self.parse_if(),
            Kw::While => self.parse_while(),
            Kw::Do => self.parse_do_while(),
            Kw::For => self.parse_for(),
            Kw::Return => self.parse_return(),
            Kw::Break | Kw::Continue => self.parse_break_continue(kw),
            Kw::Throw => {
                self.bump();
                let e = self.parse_expr()?;
                self.expect_semi()?;
                Ok(StmtKind::Throw(e))
            }
            Kw::Try => self.parse_try(),
            Kw::Switch => self.parse_switch(),
            _ if self.at_nested_item() => Ok(StmtKind::Item(Box::new(self.parse_item(false)?))),
            _ => self.parse_labeled_or_expr_stmt(),
        }
    }

    /// Declarations allowed inside blocks (functions, types, ...).
    fn at_nested_item(&mut self) -> bool {
        match self.cur_kw() {
            Some(
                Kw::Function
                | Kw::Struct
                | Kw::Class
                | Kw::Interface
                | Kw::Enum
                | Kw::Import
                | Kw::Export,
            ) => true,
            Some(Kw::Async | Kw::Declare) => self.nth(1) == Tok::Kw(Kw::Function),
            Some(Kw::Type) => Self::is_ident_like(self.nth(1)),
            _ => false,
        }
    }

    fn parse_labeled_or_expr_stmt(&mut self) -> PResult<StmtKind> {
        if self.at_ident_like() && self.nth(1) == Tok::Colon {
            let label = self.take_ident();
            self.bump(); // :
            let is_for = self.at_kw(Kw::For);
            let mut body = self.parse_stmt()?;
            // A `for` with several declarations is a block around the loop (for_loop.rs): the
            // label belongs to the loop itself.
            if let (true, StmtKind::Block(b)) = (is_for, &mut body.kind) {
                if let Some(inner) = b.stmts.pop() {
                    b.stmts.push(Stmt {
                        span: inner.span,
                        kind: StmtKind::Labeled {
                            label,
                            body: Box::new(inner),
                        },
                    });
                }
                return Ok(body.kind);
            }
            return Ok(StmtKind::Labeled {
                label,
                body: Box::new(body),
            });
        }
        let e = self.parse_expr()?;
        self.expect_semi()?;
        Ok(StmtKind::Expr(e))
    }

    /// `( expr )` around conditions.
    pub(super) fn parse_paren_cond(&mut self) -> PResult<Expr> {
        self.expect(Tok::LParen)?;
        let cond = self.parse_expr()?;
        self.expect(Tok::RParen)?;
        Ok(cond)
    }

    fn parse_if(&mut self) -> PResult<StmtKind> {
        self.bump(); // if
        let cond = self.parse_paren_cond()?;
        let then = self.parse_body()?;
        if !self.eat_kw(Kw::Else) {
            return Ok(StmtKind::If {
                cond,
                then,
                els: None,
            });
        }
        let els = if self.at_kw(Kw::If) {
            self.parse_stmt()?
        } else {
            let b = self.parse_body()?;
            Stmt {
                span: b.span,
                kind: StmtKind::Block(b),
            }
        };
        Ok(StmtKind::If {
            cond,
            then,
            els: Some(Box::new(els)),
        })
    }

    fn parse_while(&mut self) -> PResult<StmtKind> {
        self.bump(); // while
        let cond = self.parse_paren_cond()?;
        let body = self.parse_body()?;
        Ok(StmtKind::While { cond, body })
    }

    fn parse_do_while(&mut self) -> PResult<StmtKind> {
        self.bump(); // do
        let body = self.parse_body()?;
        self.expect_kw(Kw::While, "while")?;
        let cond = self.parse_paren_cond()?;
        self.expect_semi()?;
        Ok(StmtKind::DoWhile { body, cond })
    }

    fn parse_return(&mut self) -> PResult<StmtKind> {
        self.bump(); // return
        let value = if self.at(Tok::Semi) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect_semi()?;
        Ok(StmtKind::Return(value))
    }

    fn parse_break_continue(&mut self, kw: Kw) -> PResult<StmtKind> {
        self.bump();
        let label = if self.at_ident_like() {
            Some(self.take_ident())
        } else {
            None
        };
        self.expect_semi()?;
        Ok(if kw == Kw::Break {
            StmtKind::Break(label)
        } else {
            StmtKind::Continue(label)
        })
    }

    fn parse_try(&mut self) -> PResult<StmtKind> {
        self.bump(); // try
        let body = self.parse_block()?;
        let catch = if self.eat_kw(Kw::Catch) {
            let binding = if self.eat(Tok::LParen) {
                let p = self.parse_binding_pattern()?;
                self.expect(Tok::RParen)?;
                Some(p)
            } else {
                None
            };
            Some((binding, self.parse_block()?))
        } else {
            None
        };
        let finally = if self.eat_kw(Kw::Finally) {
            Some(self.parse_block()?)
        } else {
            None
        };
        if catch.is_none() && finally.is_none() {
            self.error_expected("`catch` or `finally`");
        }
        Ok(StmtKind::Try {
            body,
            catch,
            finally,
        })
    }
}
