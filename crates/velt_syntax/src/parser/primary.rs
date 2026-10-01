//! Primary (atomic) expressions: literals, identifiers, `this`, `super`, `new`, parenthesized
//! expressions, array/object/struct literals, template literals and regular expression literals.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Payload, Tok, TplPart};

impl<'a> Parser<'a> {
    pub(super) fn parse_primary(&mut self) -> PResult<Expr> {
        let lo = self.cur_lo();
        let span = self.cur_span();
        if let Some(lit) = self.literal_at_cursor() {
            self.bump();
            return Ok(self.mk_expr(ExprKind::Lit(lit), span));
        }
        let kind = match self.peek() {
            Tok::Template(..) => self.parse_template()?,
            Tok::Regex(i) => {
                self.bump();
                self.regex_literal(i, span)
            }
            Tok::Kw(Kw::This) => {
                self.bump();
                ExprKind::This
            }
            Tok::Kw(Kw::New) => self.parse_new()?,
            Tok::Ident if self.at_word("super") => self.parse_super(),
            Tok::Ident if self.at_word("match") && self.is_removed_match() => {
                self.diags.push(
                    velt_common::Diagnostic::error("`match` is not supported", span).with_note(
                        "use `switch` (it narrows discriminated unions and `typeof` tests) or `if`",
                    ),
                );
                return Err(Fail);
            }
            Tok::Ident if self.at_word("undefined") => self.undefined_expr(span),
            Tok::LParen => {
                self.bump();
                let inner = self.parse_expr()?;
                self.expect(Tok::RParen)?;
                ExprKind::Paren(Box::new(inner))
            }
            Tok::LBracket => {
                self.bump();
                let elems = self.parse_elems(Tok::RBracket)?;
                self.expect(Tok::RBracket)?;
                ExprKind::Array(elems)
            }
            Tok::LBrace => ExprKind::Object(self.parse_object_props()?),
            t if Self::is_ident_like(t) => self.parse_ident_or_struct_lit()?,
            _ => {
                self.error_expected("expression");
                return Err(Fail);
            }
        };
        let span = self.span_from(lo);
        Ok(self.mk_expr(kind, span))
    }

    /// Literal value of the current token (numbers, strings, `true`/`false`/`null`), if any.
    pub(super) fn literal_at_cursor(&self) -> Option<Lit> {
        let payload = |idx: u32| self.payloads.get(idx as usize);
        Some(match self.peek() {
            Tok::Int(i) => match payload(i)? {
                Payload::Int { value, suffix } => Lit::Int {
                    value: *value,
                    suffix: suffix.clone(),
                },
                _ => return None,
            },
            Tok::Float(i) => match payload(i)? {
                Payload::Float { value, suffix } => Lit::Float {
                    value: *value,
                    suffix: suffix.clone(),
                },
                _ => return None,
            },
            Tok::Str(i) => Lit::Str(self.payload_text(i)),
            Tok::Kw(Kw::True) => Lit::Bool(true),
            Tok::Kw(Kw::False) => Lit::Bool(false),
            Tok::Kw(Kw::Null) => Lit::Null,
            _ => return None,
        })
    }

    /// `new Foo<T>(args)`; the argument list is optional (`new Foo`).
    fn parse_new(&mut self) -> PResult<ExprKind> {
        self.bump(); // new
        let class = self.parse_named_type()?;
        let args = if self.at(Tok::LParen) {
            self.parse_args()?
        } else {
            vec![]
        };
        Ok(ExprKind::New { class, args })
    }

    /// `super` is contextual (not in the keyword list); it is only meaningful as `super(args)` or
    /// `super.method`, so anything else is reported here rather than as an unknown name later.
    fn parse_super(&mut self) -> ExprKind {
        let span = self.cur_span();
        self.bump();
        if !matches!(self.peek(), Tok::LParen | Tok::Dot) {
            self.error("`super` must be followed by `(...)` or `.name`", span);
        }
        ExprKind::Super
    }

    fn parse_ident_or_struct_lit(&mut self) -> PResult<ExprKind> {
        let ident = self.take_ident();
        if !self.at_struct_lit_body() {
            return Ok(ExprKind::Ident(ident));
        }
        let name = TypeExpr {
            span: ident.span,
            kind: TypeExprKind::Named {
                path: vec![ident],
                args: vec![],
            },
        };
        let props = self.parse_object_props()?;
        Ok(ExprKind::StructLit { name, props })
    }

    /// After a name: does `{` start a struct literal body (`{}`, `{ ...`, `{ a: `, `{ a, `, `{ a }`)?
    /// Statement bodies never follow a bare name (conditions are parenthesized), so this is safe.
    fn at_struct_lit_body(&self) -> bool {
        if self.peek() != Tok::LBrace {
            return false;
        }
        match self.nth(1) {
            Tok::RBrace | Tok::DotDotDot => true,
            t if Self::is_name(t) => matches!(self.nth(2), Tok::Colon | Tok::Comma | Tok::RBrace),
            _ => false,
        }
    }

    /// `{ key: value, shorthand, "quoted": v, ...spread }` — current token is `{`.
    fn parse_object_props(&mut self) -> PResult<Vec<ObjectProp>> {
        self.expect(Tok::LBrace)?;
        let mut props = Vec::new();
        while !self.at(Tok::RBrace) {
            props.push(self.parse_object_prop()?);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(props)
    }

    fn parse_object_prop(&mut self) -> PResult<ObjectProp> {
        if self.eat(Tok::DotDotDot) {
            return Ok(ObjectProp::Spread(self.parse_assign()?));
        }
        let shorthand_ok = self.at_ident_like();
        let key = match self.peek() {
            Tok::Str(idx) => {
                let key = Ident {
                    name: self.payload_text(idx),
                    span: self.cur_span(),
                };
                self.bump();
                key
            }
            t if Self::is_name(t) => self.take_ident(),
            _ => {
                self.error_expected("property name");
                return Err(Fail);
            }
        };
        if self.eat(Tok::Colon) {
            return Ok(ObjectProp::KeyValue(key, self.parse_assign()?));
        }
        if !shorthand_ok {
            self.error_expected("`:`");
            return Err(Fail);
        }
        Ok(ObjectProp::Shorthand(key))
    }

    /// `` `a ${x} b` `` — quasis come pre-cooked from the lexer.
    fn parse_template(&mut self) -> PResult<ExprKind> {
        let Tok::Template(idx, part) = self.peek() else {
            self.error_expected("template literal");
            return Err(Fail);
        };
        self.bump();
        let mut quasis = vec![self.payload_text(idx)];
        let mut exprs = Vec::new();
        if part == TplPart::Head {
            loop {
                exprs.push(self.parse_expr()?);
                let Tok::Template(idx, part @ (TplPart::Middle | TplPart::Tail)) = self.peek()
                else {
                    self.error_expected("`}`");
                    return Err(Fail);
                };
                quasis.push(self.payload_text(idx));
                self.bump();
                if part == TplPart::Tail {
                    break;
                }
            }
        }
        Ok(ExprKind::Template { quasis, exprs })
    }

    /// `match (x) {` — the removed `match` expression (`match(x)` alone is an ordinary call).
    fn is_removed_match(&self) -> bool {
        if self.nth(1) != Tok::LParen {
            return false;
        }
        let mut depth = 0usize;
        for (i, t) in self.toks.iter().enumerate().skip(self.pos + 1) {
            match t.kind {
                Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return self.toks.get(i + 1).map(|t| t.kind) == Some(Tok::LBrace);
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
        }
        false
    }
}

impl Parser<'_> {
    /// `/body/flags` → `new RegExp("body", "flags")` (std/regex's class, which must be imported).
    fn regex_literal(&mut self, idx: u32, span: velt_common::Span) -> ExprKind {
        let (source, flags) = match self.payloads.get(idx as usize) {
            Some(Payload::Regex { source, flags }) => (source.clone(), flags.clone()),
            _ => (String::new(), String::new()),
        };
        let class = TypeExpr {
            kind: TypeExprKind::Named {
                path: vec![Ident {
                    name: "RegExp".into(),
                    span,
                }],
                args: vec![],
            },
            span,
        };
        let args = vec![
            self.mk_expr(ExprKind::Lit(Lit::Str(source)), span),
            self.mk_expr(ExprKind::Lit(Lit::Str(flags)), span),
        ];
        ExprKind::New { class, args }
    }
}
