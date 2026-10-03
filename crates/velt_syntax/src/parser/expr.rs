//! Operator expressions: assignment, ternary, binary precedence climbing and prefix unary
//! operators. Postfix forms (member access, calls, `?`) are in `postfix`.
//!
//! Precedence follows JS (docs/reference/lexical.md); `as` and `instanceof` bind like the relational operators and `-x ** 2`
//! parses as `-(x ** 2)`.

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

// Binding powers, higher binds tighter.
const PREC_OR: u8 = 1; // || ??
const PREC_AND: u8 = 2;
const PREC_BITOR: u8 = 3;
const PREC_BITXOR: u8 = 4;
const PREC_BITAND: u8 = 5;
const PREC_EQ: u8 = 6;
const PREC_REL: u8 = 7; // < <= > >= as
const PREC_SHIFT: u8 = 8;
const PREC_ADD: u8 = 9;
const PREC_MUL: u8 = 10;
const PREC_POW: u8 = 11; // right associative

#[derive(Clone, Copy)]
enum BinTok {
    Op(BinaryOp),
    As,
    InstanceOf,
}

impl<'a> Parser<'a> {
    pub(super) fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_assign()
    }

    /// Assignment level: arrows, `a = b`, compound assignment (right associative).
    pub(super) fn parse_assign(&mut self) -> PResult<Expr> {
        if self.at_kw(Kw::Yield) {
            return self.parse_yield();
        }
        // Checking first keeps the common case, where no arrow can start, from copying the
        // large result of an arrow attempt on every expression.
        if self.may_start_arrow() {
            if let Some(arrow) = self.try_parse_arrow()? {
                return Ok(arrow);
            }
        }
        let lo = self.cur_lo();
        let lhs = self.parse_cond()?;
        let Some((op, ntoks)) = self.peek_assign_op() else {
            return Ok(lhs);
        };
        if !is_assignable(&lhs) {
            self.error("invalid assignment target", lhs.span);
        }
        for _ in 0..ntoks {
            self.bump();
        }
        let value = self.guarded(|p| p.parse_assign())?;
        let span = self.span_from(lo);
        Ok(self.mk_expr(
            ExprKind::Assign {
                op,
                target: Box::new(lhs),
                value: Box::new(value),
            },
            span,
        ))
    }

    /// `yield`, `yield expr` or `yield* expr` (assignment precedence, like JS). The operand is
    /// absent when the next token cannot start one (`yield;`, `f(yield)`).
    fn parse_yield(&mut self) -> PResult<Expr> {
        let lo = self.cur_lo();
        self.bump(); // yield
        let delegate = self.eat(Tok::Star);
        let ends = matches!(
            self.peek(),
            Tok::Semi
                | Tok::RParen
                | Tok::RBracket
                | Tok::RBrace
                | Tok::Comma
                | Tok::Colon
                | Tok::Eof
        );
        let arg = if ends && !delegate {
            None
        } else {
            Some(Box::new(self.guarded(|p| p.parse_assign())?))
        };
        let span = self.span_from(lo);
        Ok(self.mk_expr(ExprKind::Yield { arg, delegate }, span))
    }

    /// Is there a run of `n` directly adjacent `>` tokens at the cursor, followed by an adjacent `=`?
    fn at_glued_gt(&mut self, n: usize, then_eq: bool) -> bool {
        let gts = (0..n).all(|i| self.nth(i) == Tok::Gt && (i == 0 || self.adjacent(i - 1)));
        gts && then_eq == (self.nth(n) == Tok::Eq && self.adjacent(n - 1))
    }

    fn peek_assign_op(&mut self) -> Option<(Option<BinaryOp>, usize)> {
        use BinaryOp::*;
        let op = match self.peek() {
            Tok::Eq => None,
            Tok::PlusEq => Some(Add),
            Tok::MinusEq => Some(Sub),
            Tok::StarEq => Some(Mul),
            Tok::SlashEq => Some(Div),
            Tok::PercentEq => Some(Rem),
            Tok::StarStarEq => Some(Pow),
            Tok::ShlEq => Some(Shl),
            Tok::AmpEq => Some(BitAnd),
            Tok::PipeEq => Some(BitOr),
            Tok::CaretEq => Some(BitXor),
            Tok::AmpAmpEq => Some(And),
            Tok::PipePipeEq => Some(Or),
            Tok::QuestionQuestionEq => Some(Nullish),
            Tok::Gt if self.at_glued_gt(3, true) => return Some((Some(UShr), 4)),
            Tok::Gt if self.at_glued_gt(2, true) => return Some((Some(Shr), 3)),
            _ => return None,
        };
        Some((op, 1))
    }

    /// Ternary level.
    fn parse_cond(&mut self) -> PResult<Expr> {
        let lo = self.cur_lo();
        let cond = self.parse_binary(0)?;
        if !self.eat(Tok::Question) {
            return Ok(cond);
        }
        let then = self.guarded(|p| p.parse_assign())?;
        self.expect(Tok::Colon)?;
        let els = self.guarded(|p| p.parse_assign())?;
        let span = self.span_from(lo);
        Ok(self.mk_expr(
            ExprKind::Cond {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            },
            span,
        ))
    }

    /// Returns (operator, precedence, number of tokens).
    fn peek_binop(&mut self) -> Option<(BinTok, u8, usize)> {
        use BinaryOp::*;
        let (op, prec, n) = match self.peek() {
            Tok::PipePipe => (Or, PREC_OR, 1),
            Tok::QuestionQuestion => (Nullish, PREC_OR, 1),
            Tok::AmpAmp => (And, PREC_AND, 1),
            Tok::Pipe => (BitOr, PREC_BITOR, 1),
            Tok::Caret => (BitXor, PREC_BITXOR, 1),
            Tok::Amp => (BitAnd, PREC_BITAND, 1),
            Tok::EqEq | Tok::EqEqEq => (Eq, PREC_EQ, 1),
            Tok::BangEq | Tok::BangEqEq => (NotEq, PREC_EQ, 1),
            Tok::Lt => (Lt, PREC_REL, 1),
            Tok::LtEq => (LtEq, PREC_REL, 1),
            Tok::Kw(Kw::As) => return Some((BinTok::As, PREC_REL, 1)),
            Tok::Kw(Kw::Instanceof) => return Some((BinTok::InstanceOf, PREC_REL, 1)),
            Tok::Shl => (Shl, PREC_SHIFT, 1),
            Tok::Plus => (Add, PREC_ADD, 1),
            Tok::Minus => (Sub, PREC_ADD, 1),
            Tok::Star => (Mul, PREC_MUL, 1),
            Tok::Slash => (Div, PREC_MUL, 1),
            Tok::Percent => (Rem, PREC_MUL, 1),
            Tok::StarStar => (Pow, PREC_POW, 1),
            Tok::Gt => return self.peek_gt_binop(),
            _ => return None,
        };
        Some((BinTok::Op(op), prec, n))
    }

    /// `>`, `>=`, `>>`, `>>>` from adjacent single `>` tokens (`>>=`/`>>>=` are assignments).
    fn peek_gt_binop(&mut self) -> Option<(BinTok, u8, usize)> {
        use BinaryOp::*;
        let (op, prec, n) = if self.at_glued_gt(3, true) || self.at_glued_gt(2, true) {
            return None;
        } else if self.at_glued_gt(3, false) {
            (UShr, PREC_SHIFT, 3)
        } else if self.at_glued_gt(2, false) {
            (Shr, PREC_SHIFT, 2)
        } else if self.at_glued_gt(1, true) {
            (GtEq, PREC_REL, 2)
        } else {
            (Gt, PREC_REL, 1)
        };
        Some((BinTok::Op(op), prec, n))
    }

    /// Binary operators with precedence >= `min_prec`.
    fn parse_binary(&mut self, min_prec: u8) -> PResult<Expr> {
        let lo = self.cur_lo();
        let mut lhs = self.parse_unary()?;
        while let Some((op, prec, ntoks)) = self.peek_binop() {
            if prec < min_prec {
                break;
            }
            for _ in 0..ntoks {
                self.bump();
            }
            let kind = match op {
                BinTok::As => ExprKind::Cast {
                    expr: Box::new(lhs),
                    ty: self.parse_cast_type()?,
                },
                BinTok::InstanceOf => ExprKind::InstanceOf {
                    expr: Box::new(lhs),
                    ty: self.parse_instanceof_type()?,
                },
                BinTok::Op(op) => {
                    let next_min = if op == BinaryOp::Pow { prec } else { prec + 1 };
                    let rhs = self.guarded(|p| p.parse_binary(next_min))?;
                    ExprKind::Binary {
                        op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    }
                }
            };
            let span = self.span_from(lo);
            lhs = self.mk_expr(kind, span);
        }
        Ok(lhs)
    }

    pub(super) fn parse_unary(&mut self) -> PResult<Expr> {
        self.guarded(|p| p.parse_unary_inner())
    }

    fn parse_unary_inner(&mut self) -> PResult<Expr> {
        let lo = self.cur_lo();
        let kind = match self.peek() {
            Tok::Bang => self.finish_prefix(UnaryOp::Not)?,
            Tok::Tilde => self.finish_prefix(UnaryOp::BitNot)?,
            Tok::Minus => self.finish_prefix(UnaryOp::Neg)?,
            Tok::Plus => self.finish_prefix(UnaryOp::Plus)?,
            Tok::Kw(Kw::Typeof) => self.finish_prefix(UnaryOp::TypeOf)?,
            // `delete` is a contextual word: `delete r[k]`, `delete this.x`, and as in
            // TypeScript `delete (r[k])` (never a call of something named `delete`).
            Tok::Ident
                if self.at_word("delete")
                    && matches!(self.nth(1), Tok::Ident | Tok::Kw(Kw::This) | Tok::LParen) =>
            {
                self.delete_expr()?
            }
            Tok::Kw(Kw::Void) => self.void_expr()?,
            Tok::PlusPlus | Tok::MinusMinus => ExprKind::Update {
                op: self.bump_update_op(),
                prefix: true,
                target: Box::new(self.parse_unary()?),
            },
            Tok::Kw(Kw::Await) => {
                self.bump();
                ExprKind::Await(Box::new(self.parse_unary()?))
            }
            Tok::Kw(Kw::Yield) => {
                let span = self.cur_span();
                self.error(
                    "`yield` cannot be an operand here: it binds like an assignment, so wrap it in parentheses (`(yield x)`)",
                    span,
                );
                return self.parse_yield();
            }
            _ => return self.parse_postfix(),
        };
        let span = self.span_from(lo);
        Ok(self.mk_expr(kind, span))
    }

    /// `delete operand`. Parentheses around a member or index operand (`delete (r[k])`) change
    /// nothing and are dropped, so `velt fmt` prints `delete r[k]`.
    fn delete_expr(&mut self) -> PResult<ExprKind> {
        self.bump();
        let expr = self.parse_binary(PREC_POW)?;
        Ok(ExprKind::Unary {
            op: UnaryOp::Delete,
            expr: Box::new(unparen_place(expr)),
        })
    }

    /// Operand of a prefix operator binds up to `**`, so `-x ** 2` is `-(x ** 2)`.
    fn finish_prefix(&mut self, op: UnaryOp) -> PResult<ExprKind> {
        self.bump();
        let expr = self.parse_binary(PREC_POW)?;
        Ok(ExprKind::Unary {
            op,
            expr: Box::new(expr),
        })
    }
}

/// `e` without parentheses if they only enclose a member or index expression.
fn unparen_place(e: Expr) -> Expr {
    fn is_place(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Paren(inner) => is_place(inner),
            ExprKind::Member { .. } | ExprKind::Index { .. } => true,
            _ => false,
        }
    }
    match e.kind {
        ExprKind::Paren(inner) if is_place(&inner) => unparen_place(*inner),
        kind => Expr { kind, ..e },
    }
}

fn is_assignable(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Ident(_) | ExprKind::Member { .. } | ExprKind::Index { .. } => true,
        ExprKind::Array(_) | ExprKind::Object(_) => true,
        ExprKind::Paren(inner) => is_assignable(inner),
        _ => false,
    }
}
