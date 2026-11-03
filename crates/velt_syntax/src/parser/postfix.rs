//! Postfix expressions: member access and optional chaining (`.`, `?.`, `?.(`, `?.[`), calls
//! with optional explicit type arguments (`f<T>(x)`), indexing, `++`/`--`, and postfix `?` (try)
//! versus the ternary `?`.

use super::{Fail, PResult, Parser, Ternary};
use crate::ast::*;
use crate::lexer::{Kw, Tok, TplPart};

impl<'a> Parser<'a> {
    pub(super) fn parse_postfix(&mut self) -> PResult<Expr> {
        let lo = self.cur_lo();
        let mut e = self.parse_primary()?;
        loop {
            let kind = match self.peek() {
                Tok::Dot => {
                    self.bump();
                    let prop = match self.at(Tok::PrivateName) {
                        true => self.take_ident(),
                        false => self.parse_prop_name()?,
                    };
                    ExprKind::Member {
                        object: Box::new(e),
                        prop,
                        optional: false,
                    }
                }
                Tok::QuestionDot => self.parse_optional_chain(e)?,
                Tok::LParen => {
                    self.warn_continuation();
                    ExprKind::Call {
                        callee: Box::new(e),
                        type_args: vec![],
                        args: self.parse_args()?,
                        optional: false,
                    }
                }
                Tok::LBracket if self.at_well_known_key() => {
                    self.warn_continuation();
                    ExprKind::Member {
                        object: Box::new(e),
                        prop: self.parse_symbol_key()?,
                        optional: false,
                    }
                }
                Tok::LBracket => {
                    self.warn_continuation();
                    let (index, more) = self.parse_index()?;
                    ExprKind::Index {
                        index: Box::new(index),
                        more,
                        object: Box::new(e),
                        optional: false,
                    }
                }
                Tok::Template(_, TplPart::NoSub | TplPart::Head) => {
                    self.reject_tagged_template();
                    return Err(Fail);
                }
                Tok::Lt if matches!(e.kind, ExprKind::Ident(_) | ExprKind::Member { .. }) => {
                    match self.try_generic_call_args()? {
                        Some((type_args, args)) => ExprKind::Call {
                            callee: Box::new(e),
                            type_args,
                            args,
                            optional: false,
                        },
                        None => break,
                    }
                }
                // A restricted production: after a line break, `++` / `--` is the prefix
                // operator of the next line's statement.
                Tok::PlusPlus | Tok::MinusMinus if !self.line_break_before() => ExprKind::Update {
                    op: self.bump_update_op(),
                    prefix: false,
                    target: Box::new(e),
                },
                // `x!` (TS's non-null assertion), unless the `!` starts the next line's
                // statement.
                Tok::Bang
                    if !self.src[e.span.hi as usize..self.cur_lo() as usize].contains('\n') =>
                {
                    self.bump();
                    ExprKind::NonNull(Box::new(e))
                }
                Tok::Question if !self.question_is_ternary() => {
                    self.reject_question_operator();
                    continue;
                }
                _ => break,
            };
            let span = self.span_from(lo);
            e = self.mk_expr(kind, span);
        }
        Ok(e)
    }

    /// `[ index ]`
    /// `[index]` or `[index, more…]`: the keys of an operator indexer with several keys
    /// (`m[i, j]`, design operators.md). Velt has no comma operator, so a comma here always
    /// separates keys; a trailing comma and a spread are errors, as in a JS member access.
    fn parse_index(&mut self) -> PResult<(Expr, Vec<Expr>)> {
        self.bump(); // [
        let index = self.allow_in(|p| p.parse_expr())?;
        let mut more = vec![];
        while self.eat(Tok::Comma) {
            if self.at(Tok::RBracket) {
                let span = self.cur_span();
                self.error(
                    "expected an index after `,`: an index has no trailing comma",
                    span,
                );
                return Err(Fail);
            }
            if self.at(Tok::DotDotDot) {
                let span = self.cur_span();
                self.error("an index cannot be spread: write each key", span);
                return Err(Fail);
            }
            more.push(self.allow_in(|p| p.parse_expr())?);
        }
        self.expect(Tok::RBracket)?;
        Ok((index, more))
    }

    /// Consumes `++` / `--` and returns the matching operator.
    pub(super) fn bump_update_op(&mut self) -> UpdateOp {
        let op = if self.at(Tok::PlusPlus) {
            UpdateOp::Inc
        } else {
            UpdateOp::Dec
        };
        self.bump();
        op
    }

    /// `?.name`, `?.(args)` or `?.[index]` after `object`; the cursor is at `?.`.
    fn parse_optional_chain(&mut self, object: Expr) -> PResult<ExprKind> {
        self.bump(); // ?.
        let object = Box::new(object);
        match self.peek() {
            Tok::LParen => Ok(ExprKind::Call {
                callee: object,
                type_args: vec![],
                args: self.parse_args()?,
                optional: true,
            }),
            Tok::LBracket => {
                let (index, more) = self.parse_index()?;
                Ok(ExprKind::Index {
                    index: Box::new(index),
                    more,
                    object,
                    optional: true,
                })
            }
            Tok::PrivateName => {
                // TS18030.
                let span = self.cur_span();
                self.error("an optional chain cannot contain private names", span);
                Err(Fail)
            }
            t if Self::is_name(t) => Ok(ExprKind::Member {
                prop: self.take_ident(),
                object,
                optional: true,
            }),
            _ => {
                self.error_expected("property name, `(` or `[` after `?.`");
                Err(Fail)
            }
        }
    }

    /// `<T, U>(args)` after a callee: type arguments only if the matching `>` is followed by `(`.
    fn try_generic_call_args(&mut self) -> PResult<Option<(Vec<TypeExpr>, Vec<Expr>)>> {
        // Cheap pre-check: `i < n;`, `a < -b` etc. can never be type arguments. A literal type
        // (`f<"x" | null>()`, `f<1>()`, `f<-1>()`) must be followed by `>`, `|` or `,`, so
        // `i < 10` or `a < -1` in a loop condition is not tried.
        let negative = self.nth(1) == Tok::Minus;
        let at = 1 + usize::from(negative);
        let literal = match self.nth(at) {
            Tok::Int(_) | Tok::Float(_) => true,
            Tok::Str(_) | Tok::Kw(Kw::True | Kw::False) => !negative,
            _ => false,
        };
        if literal {
            if !matches!(self.nth(at + 1), Tok::Gt | Tok::Pipe | Tok::Comma) {
                return Ok(None);
            }
        } else if !Self::can_start_type(self.nth(1)) {
            return Ok(None);
        }
        let type_args = self.speculate(|p| {
            let args = p.parse_type_args()?;
            if p.at(Tok::LParen) {
                Ok(args)
            } else {
                Err(Fail)
            }
        });
        match type_args {
            Some(type_args) => Ok(Some((type_args, self.parse_args()?))),
            None => Ok(None),
        }
    }

    fn can_start_type(t: Tok) -> bool {
        Self::is_ident_like(t)
            || matches!(
                t,
                Tok::Gt
                    | Tok::LParen
                    | Tok::LBracket
                    | Tok::LBrace
                    | Tok::Pipe
                    | Tok::Kw(Kw::Null)
                    | Tok::Kw(Kw::Void)
            )
    }

    /// Postfix `e?` no longer exists (errors propagate by themselves): report it, skip the `?`.
    fn reject_question_operator(&mut self) {
        let span = self.cur_span();
        self.diags.push(
            velt_common::Diagnostic::error("the `?` operator was removed", span)
                .with_note("a call that throws propagates its error automatically: remove the `?`")
                .with_note(
                    "for errors as values, return a union (`User | NotFound`) and narrow it",
                ),
        );
        self.bump();
    }

    /// Decides whether the `?` at the cursor starts a ternary (vs. the removed postfix `?`): it
    /// is a ternary iff an expression and then `:` follow. Decisions are cached per token, with
    /// where that expression ends, so nested ternaries stay linear (`parse_cond`).
    fn question_is_ternary(&mut self) -> bool {
        if !Self::can_start_expr(self.nth(1)) {
            return false;
        }
        if let Some(cached) = self.ternary_cache.get(&self.pos) {
            return cached.is_ternary;
        }
        let key = self.pos;
        let snap = self.snapshot();
        let saved_flag = std::mem::replace(&mut self.hit_depth_limit, false);
        self.speculating += 1;
        self.bump();
        let is_ternary = self.allow_in(|p| p.parse_assign()).is_ok() && self.at(Tok::Colon);
        let then_end = (self.pos, self.prev_hi);
        self.speculating -= 1;
        let too_deep = self.hit_depth_limit;
        self.hit_depth_limit = saved_flag || too_deep;
        self.restore(snap);
        if too_deep {
            // Undecidable within the depth limit: take the ternary path so the real parse reports
            // the nesting error instead of a confusing follow-up error.
            return true;
        }
        let decision = Ternary {
            is_ternary,
            then_end,
            relexes: self.relexes,
        };
        self.ternary_cache.insert(key, decision);
        is_ternary
    }

    /// Can `t` be the first token of an expression? Every token `parse_unary_inner` and
    /// `parse_primary` accept (a contextual word such as `undefined`, `delete` or `super` is a
    /// `Tok::Ident`); keep the three in step.
    pub(super) fn can_start_expr(t: Tok) -> bool {
        match t {
            Tok::Ident
            | Tok::Int(_)
            | Tok::Float(_)
            | Tok::Str(_)
            | Tok::Template(..)
            | Tok::Regex(_)
            | Tok::PrivateName
            | Tok::Lt
            | Tok::JsxLt
            | Tok::LParen
            | Tok::LBracket
            | Tok::LBrace
            | Tok::Bang
            | Tok::Tilde
            | Tok::Minus
            | Tok::Plus
            | Tok::PlusPlus
            | Tok::MinusMinus => true,
            Tok::Kw(k) => {
                k.is_soft()
                    || matches!(
                        k,
                        Kw::True
                            | Kw::False
                            | Kw::Null
                            | Kw::This
                            | Kw::New
                            | Kw::Function
                            | Kw::Await
                            | Kw::Async
                            | Kw::Yield
                            | Kw::Import
                            | Kw::Typeof
                            | Kw::Void
                    )
            }
            _ => false,
        }
    }

    /// `( args )` with spread support.
    pub(super) fn parse_args(&mut self) -> PResult<Vec<Expr>> {
        self.expect(Tok::LParen)?;
        let args = self.parse_elems(Tok::RParen)?;
        self.expect(Tok::RParen)?;
        Ok(args)
    }

    /// Comma-separated expressions / `...spread` up to (not including) `close`.
    pub(super) fn parse_elems(&mut self, close: Tok) -> PResult<Vec<Expr>> {
        self.allow_in(|p| p.parse_elems_in(close))
    }

    fn parse_elems_in(&mut self, close: Tok) -> PResult<Vec<Expr>> {
        let mut out = Vec::new();
        while !self.at(close) {
            // A hole of an array literal (`[a, , b]`): an identifier with no name
            // (`crate::ARRAY_HOLE`), which prints as nothing.
            if close == Tok::RBracket && self.at(Tok::Comma) {
                let at = self.cur_lo();
                let id = Ident {
                    name: crate::ARRAY_HOLE.into(),
                    span: velt_common::Span::new(self.file, at, at),
                };
                out.push(self.mk_expr(
                    ExprKind::Ident(id),
                    velt_common::Span::new(self.file, at, at),
                ));
                self.bump();
                continue;
            }
            out.push(self.parse_elem()?);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        Ok(out)
    }

    fn parse_elem(&mut self) -> PResult<Expr> {
        if !self.at(Tok::DotDotDot) {
            return self.parse_assign();
        }
        let lo = self.cur_lo();
        self.bump();
        let inner = self.parse_assign()?;
        let span = self.span_from(lo);
        Ok(self.mk_expr(ExprKind::Spread(Box::new(inner)), span))
    }
}

/// An assignment target (`=` or `op=`) without its non-null assertions: `w[i]! -= n` and
/// `x! = v` assign to `w[i]` and `x`, as TypeScript emits them, so every analysis of assignment
/// targets sees the place (the assertion only changes the type that is read, and Velt types an
/// element read as the element type). Through a target whose type is nullable, the compound
/// operator is then checked on that type and reported. `++`/`--` keep their target as written:
/// sema reads `h[k]!++` through the assertion. Other targets are unchanged.
pub(super) fn unassert_target(e: Expr) -> Expr {
    match e.kind {
        ExprKind::NonNull(inner) if is_place(&inner) => unassert_target(*inner),
        ExprKind::Paren(inner) if is_place(&inner) => Expr {
            kind: ExprKind::Paren(Box::new(unassert_target(*inner))),
            ..e
        },
        kind => Expr { kind, ..e },
    }
}

/// A variable, member or element, under any parentheses and non-null assertions.
fn is_place(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Ident(_) | ExprKind::Member { .. } | ExprKind::Index { .. } => true,
        ExprKind::Paren(inner) | ExprKind::NonNull(inner) => is_place(inner),
        _ => false,
    }
}
