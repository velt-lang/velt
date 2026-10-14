//! Primary (atomic) expressions: literals, identifiers, `this`, `super`, `new`, function
//! expressions (`function* () {}`), parenthesized expressions, array/object/struct literals, template literals and regular expression literals.
//! JSX elements are parsed in `jsx`: a `<` directly followed by a name or `>` starts one here.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Payload, Tok, TplPart};
use velt_common::Span;

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
            Tok::Kw(Kw::Function) => self.parse_fn_expr(lo, false)?,
            Tok::Kw(Kw::Async) if self.nth(1) == Tok::Kw(Kw::Function) => {
                self.bump(); // async
                self.parse_fn_expr(lo, true)?
            }
            Tok::Ident if self.at_word("super") => self.parse_super(),
            Tok::Ident if self.at_word("match") && self.is_removed_match() => {
                self.diags.push(
                    velt_common::Diagnostic::error("`match` is not supported", span).with_note(
                        "use `switch` (it narrows discriminated unions and `typeof` tests) or `if`",
                    ),
                );
                return Err(Fail);
            }
            Tok::Ident if self.at_word("undefined") => {
                self.bump();
                ExprKind::Lit(Lit::Undefined)
            }
            Tok::LParen => {
                self.bump();
                let inner = self.allow_in(|p| p.parse_expr())?;
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
            Tok::PrivateName => {
                self.error(
                    "private names are only allowed in class bodies and may only be used as part of a class member declaration, property access, or on the left-hand side of an `in` expression",
                    span,
                );
                self.bump();
                return Err(Fail);
            }
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
        self.allow_in(|p| p.parse_object_props_in())
    }

    fn parse_object_props_in(&mut self) -> PResult<Vec<ObjectProp>> {
        self.expect(Tok::LBrace)?;
        let mut props = Vec::new();
        // `value: undefined` (its position).
        let mut undefined_value = None;
        while !self.at(Tok::RBrace) {
            let prop = self.parse_object_prop()?;
            if let ObjectProp::KeyValue(k, v) = &prop {
                if k.name == "value" && matches!(v.kind, ExprKind::Lit(Lit::Undefined)) {
                    undefined_value = Some((props.len(), v.span));
                }
            }
            props.push(prop);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBrace)?;
        if let Some((i, span)) = undefined_value {
            self.finished_result_value(&mut props, i, span);
        }
        Ok(props)
    }

    /// `{ done: true, value: undefined }`, TypeScript's finished `IteratorResult`: Velt's has no
    /// `value`. Says so, and drops the property.
    fn finished_result_value(&mut self, props: &mut Vec<ObjectProp>, i: usize, span: Span) {
        let done = props.iter().any(|p| {
            matches!(p, ObjectProp::KeyValue(k, Expr { kind: ExprKind::Lit(Lit::Bool(true)), .. })
                if k.name == "done")
        });
        if !done {
            return;
        }
        if self.speculating == 0 {
            self.diags.push(
                velt_common::Diagnostic::error(
                    "`value: undefined` in a finished iterator result is not supported",
                    span,
                )
                .with_note(
                    "TypeScript allows this; a finished `IteratorResult` in Velt has no `value`; write `{ done: true }`",
                ),
            );
        }
        props.remove(i);
    }

    fn parse_object_prop(&mut self) -> PResult<ObjectProp> {
        if self.eat(Tok::DotDotDot) {
            return Ok(ObjectProp::Spread(self.parse_assign()?));
        }
        if let Some(m) = self.try_object_method()? {
            return Ok(m);
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
            Tok::LBracket if self.at_symbol_key() => {
                let key = self.parse_symbol_key()?;
                self.expect(Tok::Colon)?;
                return Ok(ObjectProp::KeyValue(key, self.parse_prop_value()?));
            }
            Tok::PrivateName => {
                let span = self.cur_span();
                self.error("private names are only allowed in class bodies", span);
                return Err(Fail);
            }
            _ => {
                self.error_expected("property name");
                return Err(Fail);
            }
        };
        if self.eat(Tok::Colon) {
            return Ok(ObjectProp::KeyValue(key, self.parse_prop_value()?));
        }
        if !shorthand_ok {
            self.error_expected("`:`");
            return Err(Fail);
        }
        Ok(ObjectProp::Shorthand(key))
    }

    /// The value of an object-literal property, after `:`.
    fn parse_prop_value(&mut self) -> PResult<Expr> {
        self.fn_in_object = self.at_kw(Kw::Function)
            || (self.at_kw(Kw::Async) && self.nth(1) == Tok::Kw(Kw::Function));
        let value = self.parse_assign();
        self.fn_in_object = false;
        value
    }

    /// A method in an object literal: `[async] [*]name(...) { ... }`, where the name may be a
    /// symbol key (`*[Symbol.iterator]() { ... }`). `None` (nothing consumed) for a property.
    fn try_object_method(&mut self) -> PResult<Option<ObjectProp>> {
        let lo = self.cur_lo();
        // `get name() { … }` / `set name(v) { … }`: accessors (not `get(…)`, a method named `get`).
        if (self.at_word("get") || self.at_word("set"))
            && (Self::is_name(self.nth(1)) || matches!(self.nth(1), Tok::Str(_)))
            && self.nth(2) == Tok::LParen
        {
            let kw = self.cur_span();
            self.bump();
            let name = self.parse_prop_key()?;
            self.diags.push(
                velt_common::Diagnostic::error(
                    "getters and setters in object literals are not supported yet",
                    Span::new(self.file, kw.lo, name.span.hi),
                )
                .with_note("TypeScript allows this; Velt doesn't yet because an object literal is plain data: its fields hold values; store the value in a field, or declare a class with the accessor"),
            );
            return self.finish_method_prop(lo, name, false).map(Some);
        }
        let is_async = self.at_kw(Kw::Async) && matches!(self.nth(1), Tok::Star | Tok::LBracket)
            || (self.at_kw(Kw::Async) && Self::is_name(self.nth(1)) && self.nth(2) == Tok::LParen);
        let off = usize::from(is_async);
        let is_generator = self.nth(off) == Tok::Star;
        let at_name = |p: &mut Self, k: usize| {
            (p.nth(k) == Tok::LBracket && p.nth_word(k + 1, "Symbol"))
                || (p.nth(k) == Tok::LBracket
                    && p.nth(k + 1) == Tok::Ident
                    && p.nth(k + 2) == Tok::RBracket
                    && p.nth(k + 3) == Tok::LParen)
                || (Self::is_name(p.nth(k)) && p.nth(k + 1) == Tok::LParen)
        };
        if !is_generator && !at_name(self, off) {
            return Ok(None);
        }
        if is_async {
            self.bump();
        }
        if is_generator {
            self.bump();
        }
        let name = self.parse_member_name()?;
        self.reject_private_name(&name);
        if !is_generator && !name.name.starts_with("[Symbol.") {
            return self.finish_method_prop(lo, name, is_async).map(Some);
        }
        if !name.name.starts_with('[') {
            return self.generator_method_prop(lo, name, is_async).map(Some);
        }
        let mut sig = self.parse_sig_rest(lo, name, is_async)?;
        sig.is_generator = is_generator;
        let body = self.parse_block()?;
        Ok(Some(ObjectProp::Method(Box::new(FnDecl {
            sig,
            body,
            overloads: vec![],
        }))))
    }

    /// `*name(params) { body }` (`async *name`) in an object literal, after the name: a property
    /// holding the generator function, `name: function* (params) { body }`.
    fn generator_method_prop(
        &mut self,
        lo: u32,
        name: Ident,
        is_async: bool,
    ) -> PResult<ObjectProp> {
        let anonymous = Ident {
            name: String::new(),
            span: name.span,
        };
        let mut sig = self.parse_sig_rest(lo, anonymous, is_async)?;
        sig.is_generator = true;
        let body = self.parse_block()?;
        self.reject_this_in_method(&name, &body);
        let span = self.span_from(lo);
        let f = FnDecl {
            sig,
            body,
            overloads: vec![],
        };
        let f = self.mk_expr(ExprKind::Function(Box::new(f)), span);
        Ok(ObjectProp::KeyValue(name, f))
    }

    /// Reports `this` in the body of object-literal method `name` (not supported yet).
    fn reject_this_in_method(&mut self, name: &Ident, body: &Block) {
        if let Some(at) = this_in_block(body) {
            self.diags.push(
                velt_common::Diagnostic::error(
                    format!(
                        "`this` in the object-literal method `{}` is not supported yet",
                        name.name
                    ),
                    at,
                )
                .with_note("TypeScript allows this (`this` is the object the method is called on); Velt doesn't yet because the method is a function stored in a field, which sees the variables around it but not the object; use those variables, or declare a class"),
            );
        }
    }

    /// `name(params) { body }` in an object literal, after the name: a property holding the
    /// function, `name: (params) => { body }`. The parameter types may be left out: they come
    /// from the object's context, as an arrow's do (a `ProxyHandler`'s `get(obj, prop) {}`).
    /// `this` in the body is not supported yet (an arrow's `this` is the enclosing one).
    fn finish_method_prop(&mut self, lo: u32, name: Ident, is_async: bool) -> PResult<ObjectProp> {
        let type_params = self.parse_generic_params()?;
        let params = self.parse_arrow_params()?;
        if let Some(p) = params.iter().find(|p| p.rest) {
            self.diags.push(
                velt_common::Diagnostic::error(
                    format!(
                        "rest parameters of object-literal methods (`{}(...xs)`) are not supported yet",
                        name.name
                    ),
                    p.name.span,
                )
                .with_note("take an array (`sum(xs: number[])`) and pass one, or declare a class with the method"),
            );
        }
        let (ret, throws) = if self.eat(Tok::Colon) {
            (Some(self.parse_ret_type()?), self.parse_throws_clause()?)
        } else {
            (None, None)
        };
        let body = self.parse_block()?;
        self.reject_this_in_method(&name, &body);
        let span = self.span_from(lo);
        let arrow = self.mk_expr(
            ExprKind::Arrow {
                type_params,
                params,
                ret,
                throws,
                body: ArrowBody::Block(body),
                is_async,
            },
            span,
        );
        Ok(ObjectProp::KeyValue(name, arrow))
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
                exprs.push(self.allow_in(|p| p.parse_expr())?);
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

/// Where `body` uses `this` (in nested arrows too, which see the same `this`).
fn this_in_block(body: &Block) -> Option<Span> {
    struct Find(Option<Span>);
    impl<'a> crate::visit::Visit<'a> for Find {
        fn expr(&mut self, e: &'a Expr) {
            if matches!(e.kind, ExprKind::This) && self.0.is_none() {
                self.0 = Some(e.span);
            }
        }
    }
    let mut f = Find(None);
    for s in &body.stmts {
        crate::visit::walk_stmt(s, &mut f);
    }
    f.0
}
