//! Primary (atomic) expressions: literals, identifiers, `this`, `super`, `new`, parenthesized
//! expressions, array/object/struct literals, template literals and regular expression literals.
//! JSX elements are parsed in `jsx`: a `<` directly followed by a name or `>` starts one here.

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
            Tok::Lt if self.jsx_starts_here() => {
                if self.plain_ts {
                    if let Some(operand) = self.try_type_assertion()? {
                        return Ok(operand);
                    }
                }
                self.relex_jsx();
                ExprKind::Jsx(Box::new(self.parse_jsx_element()?))
            }
            // Already re-lexed, by a speculative parse that came this way before.
            Tok::JsxLt => ExprKind::Jsx(Box::new(self.parse_jsx_element()?)),
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
                // Parentheses around a JSX element are layout only (a formatter adds them to
                // multi-line elements), so they leave no `Paren` node.
                if matches!(inner.kind, ExprKind::Jsx(_)) {
                    return Ok(inner);
                }
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
    pub(super) fn literal_at_cursor(&mut self) -> Option<Lit> {
        Some(match self.peek() {
            Tok::Int(i) => match self.payload(i)? {
                Payload::Int { value, suffix } => Lit::Int {
                    value: *value,
                    suffix: suffix.clone(),
                },
                _ => return None,
            },
            Tok::Float(i) => match self.payload(i)? {
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
    fn at_struct_lit_body(&mut self) -> bool {
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
        // `value: undefined` (its position, and its diagnostic's).
        let mut undefined_value = None;
        while !self.at(Tok::RBrace) {
            let ndiags = self.diags.len();
            let prop = self.parse_object_prop()?;
            if let ObjectProp::KeyValue(k, v) = &prop {
                if k.name == "value"
                    && self.text(v.span.lo, v.span.hi) == "undefined"
                    && self.diags.len() > ndiags
                {
                    undefined_value = Some((props.len(), ndiags));
                }
            }
            props.push(prop);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBrace)?;
        if let Some((i, d)) = undefined_value {
            self.finished_result_value(&mut props, i, d);
        }
        Ok(props)
    }

    /// `{ done: true, value: undefined }`, TypeScript's finished `IteratorResult`: Velt's has no
    /// `value`. Says so instead of "use `null`" (which a `{ done: true }` result can't hold),
    /// and drops the property.
    fn finished_result_value(&mut self, props: &mut Vec<ObjectProp>, i: usize, d: usize) {
        let done = props.iter().any(|p| {
            matches!(p, ObjectProp::KeyValue(k, Expr { kind: ExprKind::Lit(Lit::Bool(true)), .. })
                if k.name == "done")
        });
        if !done {
            return;
        }
        let span = self.diags[d].labels[0].span;
        self.diags[d] = velt_common::Diagnostic::error("`undefined` is not part of Velt", span)
            .with_note(
                "TypeScript allows this; Velt doesn't because it has no `undefined`, and a finished `IteratorResult` has no `value`; write `{ done: true }`",
            );
        props.remove(i);
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
    fn is_removed_match(&mut self) -> bool {
        if self.nth(1) != Tok::LParen {
            return false;
        }
        let mut depth = 0usize;
        for i in self.pos + 1.. {
            match self.tok(i).kind {
                Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return self.tok(i + 1).kind == Tok::LBrace;
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
        let (source, flags) = match self.payload(idx) {
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
