//! Statements and blocks, with statement-level error recovery.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// `{ stmts }`. A missing `}` at end of file is reported and the partial block is returned.
    pub(super) fn parse_block(&mut self) -> PResult<Block> {
        let lo = self.cur_lo();
        let fn_body = std::mem::take(&mut self.fn_body_next);
        self.expect(Tok::LBrace)?;
        // A function body in a `for` head (`for (f = () => { … }; …)`) has its own `in`.
        self.allow_in(|p| p.parse_block_in(lo, fn_body))
    }

    /// The body of a function: a block in which `var` may be declared (`var_stmt`).
    pub(super) fn parse_fn_body(&mut self) -> PResult<Block> {
        self.fn_body_next = true;
        self.parse_block()
    }

    fn parse_block_in(&mut self, lo: u32, fn_body: bool) -> PResult<Block> {
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
            self.var_ok = fn_body;
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
            Ok(s) => {
                out.push(s);
                out.extend(self.more_var_stmts());
            }
            Err(Fail) => {
                self.more_vars.clear();
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
        let mut stmts = vec![s];
        stmts.extend(self.more_var_stmts());
        Ok(Block {
            span: stmts[0].span,
            stmts,
        })
    }

    /// The further declarators of the declaration just parsed (`let a = 1, b = 2;`), as
    /// statements.
    pub(super) fn more_var_stmts(&mut self) -> Vec<Stmt> {
        std::mem::take(&mut self.more_vars)
            .into_iter()
            .map(|v| Stmt {
                span: v.span,
                kind: StmtKind::Var(v),
            })
            .collect()
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        self.guarded(|p| {
            let lo = p.cur_lo();
            let outer = p.begin_statement();
            let kind = p.parse_stmt_kind();
            p.end_statement(outer);
            let kind = kind?;
            Ok(Stmt {
                kind,
                span: p.span_from(lo),
            })
        })
    }

    fn parse_stmt_kind(&mut self) -> PResult<StmtKind> {
        let var_ok = std::mem::take(&mut self.var_ok);
        if self.at_var_decl() {
            return self.var_stmt(var_ok);
        }
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
            Kw::Let | Kw::Const if self.at_class_binding() => {
                let lo = self.cur_span().lo;
                let kind = ItemKind::Class(self.parse_class_binding()?);
                Ok(StmtKind::Item(Box::new(Item {
                    kind,
                    exported: false,
                    span: self.span_from(lo),
                })))
            }
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
                if self.line_break_before() {
                    // TS1142; parsed on, so the rest of the statement is still checked.
                    let span = self.cur_span();
                    self.error(
                        "a line break is not permitted after `throw` (as in TypeScript): start the thrown value on the same line",
                        span,
                    );
                }
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
        if self.at_decl_word_before_line_break() {
            return false;
        }
        match self.cur_kw() {
            Some(Kw::Function | Kw::Struct | Kw::Class | Kw::Interface | Kw::Enum | Kw::Export) => {
                true
            }
            Some(Kw::Import) => !matches!(self.nth(1), Tok::Dot | Tok::LParen),
            Some(Kw::Async) => self.at_async_function(),
            Some(Kw::Declare) => self.nth(1) == Tok::Kw(Kw::Function),
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
        // A `;` is inserted after a `do`-`while` even on the same line (ECMAScript 2015).
        self.eat(Tok::Semi);
        Ok(StmtKind::DoWhile { body, cond })
    }

    /// `return [value]`: a restricted production, so a line break right after `return` ends
    /// the statement and the next line is not its value (as in JavaScript).
    fn parse_return(&mut self) -> PResult<StmtKind> {
        self.bump(); // return
        let value = if self.at(Tok::Semi) || self.at_inserted_semi() {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect_semi()?;
        Ok(StmtKind::Return(value))
    }

    fn parse_break_continue(&mut self, kw: Kw) -> PResult<StmtKind> {
        self.bump();
        // The label must be on the same line (a restricted production).
        let label = if self.at_ident_like() && !self.line_break_before() {
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

    /// `catch (e: unknown)` / `catch (e: any)`: TypeScript allows only these annotations on the
    /// catch binding (else TS1196). They are dropped: the binding has the caught value's type.
    fn catch_annotation(&mut self) -> PResult<()> {
        if !self.eat(Tok::Colon) {
            return Ok(());
        }
        let ty = self.parse_type()?;
        let ok = matches!(&ty.kind, TypeExprKind::Named { path, args }
            if args.is_empty() && path.len() == 1 && matches!(path[0].name.as_str(), "any" | "unknown"));
        if !ok {
            self.error(
                "a catch clause variable's type annotation must be `any` or `unknown` if specified",
                ty.span,
            );
        }
        Ok(())
    }

    fn parse_try(&mut self) -> PResult<StmtKind> {
        self.bump(); // try
        let body = self.parse_block()?;
        let catch = if self.eat_kw(Kw::Catch) {
            let binding = if self.eat(Tok::LParen) {
                let p = self.parse_binding_pattern()?;
                self.catch_annotation()?;
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
