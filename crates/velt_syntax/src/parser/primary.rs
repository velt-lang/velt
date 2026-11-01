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
        if let Some(lit) = self.define_at_cursor() {
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
            Tok::Kw(Kw::Import) if self.nth(1) == Tok::Dot => self.parse_import_meta()?,
            Tok::Kw(Kw::Import) if self.nth(1) == Tok::LParen => {
                self.dynamic_import();
                return Err(Fail);
            }
            Tok::Kw(Kw::Function) => self.parse_fn_expr(lo, false)?,
            Tok::Kw(Kw::Async) if self.at_async_function() => {
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

    /// The build-time constant the identifier at the cursor stands for (`define` in
    /// package.vlt, as Vite's `define`): a reference that nothing in its scope declares
    /// (`crate::scope::free_values`, found by a first parse).
    pub(super) fn define_at_cursor(&mut self) -> Option<Lit> {
        let t = self.tok(self.pos);
        if t.kind != Tok::Ident {
            return None;
        }
        self.define_at?.get(&t.lo).cloned()
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
        self.type_args_any_line = true;
        let class = self.parse_named_type();
        self.type_args_any_line = false;
        let class = class?;
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
        while !self.at(Tok::RBrace) {
            props.push(self.parse_object_prop()?);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(props)
    }

    /// After an error in an object literal's property: past the literal's `}`, so the
    /// statement around it recovers at its own end.
    fn skip_rest_of_object(&mut self) {
        let mut depth = 0u32;
        loop {
            match self.peek() {
                Tok::Eof => break,
                Tok::LBrace | Tok::LParen | Tok::LBracket => depth += 1,
                Tok::RBrace if depth == 0 => {
                    self.bump();
                    break;
                }
                Tok::RBrace | Tok::RParen | Tok::RBracket => depth = depth.saturating_sub(1),
                _ => {}
            }
            self.bump();
        }
    }

    fn parse_object_prop(&mut self) -> PResult<ObjectProp> {
        if self.eat(Tok::DotDotDot) {
            return Ok(ObjectProp::Spread(self.parse_assign()?));
        }
        if self.at_inline_symbol_key() {
            let fail = self.reject_inline_symbol_key();
            self.skip_rest_of_object();
            return Err(fail);
        }
        if self.at_literal_name() {
            let len = match self.peek() {
                Tok::LBracket => self.member_key_len(0, false),
                _ => 1,
            };
            if self.nth(len) == Tok::LParen {
                let name = self.parse_pattern_key()?;
                self.reject_literal_method_name(&name);
                self.skip_rest_of_object();
                return Err(Fail);
            }
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
            // A numeric property name, as in TypeScript: `{ 7: v }`, `{ 0x10: v }`, `{ 1.50: v }`
            // and `{ 1e3: v }` have the keys "7", "16", "1.5" and "1000" (`String` of the number).
            Tok::Int(_) | Tok::Float(_) => {
                let Some(name) = self.literal_at_cursor().as_ref().and_then(number_key) else {
                    self.error_expected("property name");
                    return Err(Fail);
                };
                let key = Ident {
                    name,
                    span: self.cur_span(),
                };
                self.bump();
                self.expect(Tok::Colon)?;
                return Ok(ObjectProp::KeyValue(key, self.parse_assign()?));
            }
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
            Tok::LBracket => return self.parse_computed_prop(),
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
        // `{ a = 1 }`: a default, which only an assignment pattern may have (`({ a = 1 } = o)`;
        // sema rejects it elsewhere). It is `a: a = 1` with the assignment starting at the key.
        if self.eat(Tok::Eq) {
            let value = self.parse_assign()?;
            let target = self.mk_expr(ExprKind::Ident(key.clone()), key.span);
            let assign = ExprKind::Assign {
                op: None,
                target: Box::new(target),
                value: Box::new(value),
            };
            let span = self.span_from(key.span.lo);
            let assign = self.mk_expr(assign, span);
            return Ok(ObjectProp::KeyValue(key, assign));
        }
        // `{ __DEV__ }` with a `define`: `{ __DEV__: false }`.
        if let Some(lit) = self.define_at.and_then(|d| d.get(&key.span.lo)).cloned() {
            let value = self.mk_expr(ExprKind::Lit(lit), key.span);
            return Ok(ObjectProp::KeyValue(key, value));
        }
        Ok(ObjectProp::Shorthand(key))
    }

    /// `[key]: value` with any expression as the key (a symbol key or a module constant,
    /// `[KEY]`, is [`Self::parse_symbol_key`]'s). A string or integer literal key is the plain
    /// key (`["a"]: 1` is `a: 1`). Any other key is known only at run time: the property is
    /// written as a spread of a call of the prelude's [`COMPUTED_KEY_FN`], `...computedKey(key,
    /// value)`, which sema checks where a `Record` is built (the object has string keys).
    fn parse_computed_prop(&mut self) -> PResult<ObjectProp> {
        let lo = self.cur_lo();
        self.bump(); // [
        let key = self.allow_in(|p| p.parse_assign())?;
        self.expect(Tok::RBracket)?;
        let key_span = self.span_from(lo);
        if self.at(Tok::LParen) {
            self.error(
                "a method name in an object literal must be a well-known symbol (`[Symbol.iterator]`) or a module constant (`[KEY]`), not an expression",
                key_span,
            );
            return Err(Fail);
        }
        self.expect(Tok::Colon)?;
        let value = self.parse_prop_value()?;
        let plain = match &key.kind {
            ExprKind::Lit(Lit::Str(s)) => Some(s.clone()),
            ExprKind::Lit(lit) => number_key(lit),
            // `[-1]`: the key "-1" (and `[-0]` is "0", as `String(-0)` is).
            ExprKind::Unary {
                op: UnaryOp::Neg,
                expr,
            } => match &expr.kind {
                ExprKind::Lit(lit) => number_key(lit).map(|n| match n.as_str() {
                    "0" => n,
                    _ => format!("-{n}"),
                }),
                _ => None,
            },
            _ => None,
        };
        if let Some(name) = plain {
            return Ok(ObjectProp::KeyValue(
                Ident {
                    name,
                    span: key.span,
                },
                value,
            ));
        }
        let callee = self.mk_expr(
            ExprKind::Ident(Ident {
                name: COMPUTED_KEY_FN.into(),
                span: key_span,
            }),
            key_span,
        );
        let span = self.span_from(lo);
        let call = ExprKind::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args: vec![key, value],
            optional: false,
        };
        Ok(ObjectProp::Spread(self.mk_expr(call, span)))
    }

    /// The value of an object-literal property, after `:`.
    fn parse_prop_value(&mut self) -> PResult<Expr> {
        self.fn_in_object = self.at_kw(Kw::Function) || self.at_async_function();
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
            && (Self::is_name(self.nth(1))
                || matches!(self.nth(1), Tok::Str(_) | Tok::Int(_) | Tok::Float(_)))
            && self.nth(2) == Tok::LParen
        {
            let kw = self.cur_span();
            let setter = self.at_word("set");
            self.bump();
            let literal = self.at_literal_name();
            let name = self.parse_prop_key()?;
            if literal {
                self.reject_literal_method_name(&name);
                self.skip_rest_of_object();
                return Err(Fail);
            }
            let ObjectProp::KeyValue(name, arrow) = self.finish_method_prop(lo, name, false)?
            else {
                unreachable!("ICE: a method property is a key and a value");
            };
            self.check_accessor_params(&arrow, setter, kw);
            let f = if setter { OBJECT_SET_FN } else { OBJECT_GET_FN };
            let callee = self.mk_expr(
                ExprKind::Ident(Ident {
                    name: f.into(),
                    span: kw,
                }),
                kw,
            );
            let span = arrow.span;
            let call = ExprKind::Call {
                callee: Box::new(callee),
                type_args: vec![],
                args: vec![arrow],
                optional: false,
            };
            return Ok(Some(ObjectProp::KeyValue(name, self.mk_expr(call, span))));
        }
        // `async` [no LineTerminator here] the method name.
        let async_kw = self.at_kw(Kw::Async) && !self.line_break_after(0);
        let is_async = async_kw && matches!(self.nth(1), Tok::Star | Tok::LBracket)
            || (async_kw
                && Self::is_name(self.nth(1))
                && matches!(self.nth(2), Tok::LParen | Tok::Lt));
        let off = usize::from(is_async);
        let is_generator = self.nth(off) == Tok::Star;
        let at_name = |p: &mut Self, k: usize| {
            // `[Symbol.name](…)`: a method; `[Symbol.name]: v` is a property.
            (p.nth(k) == Tok::LBracket
                && p.nth_word(k + 1, "Symbol")
                && matches!(p.nth(k + 5), Tok::LParen | Tok::Lt))
                || (p.nth(k) == Tok::LBracket
                    && p.nth(k + 1) == Tok::Ident
                    && p.nth(k + 2) == Tok::RBracket
                    && p.nth(k + 3) == Tok::LParen)
                || (Self::is_name(p.nth(k)) && p.nth(k + 1) == Tok::LParen)
                // `name<T>(…) { … }`: a generic method.
                || (Self::is_name(p.nth(k)) && p.nth(k + 1) == Tok::Lt)
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
        let sig = self.parse_sig_rest(lo, name, is_async, is_generator)?;
        let body = self.parse_fn_body()?;
        Ok(Some(ObjectProp::Method(Box::new(FnDecl {
            sig,
            body,
            overloads: vec![],
        }))))
    }

    /// A getter takes no parameters and a setter one (as in TypeScript).
    fn check_accessor_params(&mut self, arrow: &Expr, setter: bool, kw: Span) {
        let ExprKind::Arrow {
            params,
            ret,
            type_params,
            ..
        } = &arrow.kind
        else {
            return;
        };
        let own = params.iter().filter(|p| !is_method_this(&p.name)).count();
        let msg = match (setter, own) {
            (false, 0) | (true, 1) => None,
            (false, _) => Some("a getter cannot have parameters"),
            (true, _) => Some("a setter must have exactly one parameter"),
        };
        if let Some(msg) = msg {
            self.error(msg, kw);
        } else if setter && ret.is_some() {
            self.error("a setter cannot have a return type annotation", kw);
        } else if !type_params.is_empty() {
            self.error("an accessor cannot have type parameters", kw);
        }
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
        let mut sig = self.parse_sig_rest(lo, anonymous, is_async, true)?;
        let body = self.parse_fn_body()?;
        self.method_this_param(lo, &mut sig.params, &body);
        let span = self.span_from(lo);
        let f = FnDecl {
            sig,
            body,
            overloads: vec![],
        };
        let f = self.mk_expr(ExprKind::Function(Box::new(f)), span);
        Ok(ObjectProp::KeyValue(name, f))
    }

    /// [`METHOD_THIS`] in front of `params` of a generator method (or `function*` property
    /// value) starting at `lo` whose `body` uses `this`. Its type is a placeholder sema
    /// replaces (`infer this`, which is no type outside a conditional type).
    pub(super) fn method_this_param(&mut self, lo: u32, params: &mut Vec<Param>, body: &Block) {
        if super::fn_exprs::own_this(body).is_none() {
            return;
        }
        let at = Span::new(self.file, lo, lo);
        let name = Ident {
            name: METHOD_THIS.into(),
            span: at,
        };
        let ty = TypeExpr {
            kind: TypeExprKind::Infer(name.clone()),
            span: at,
        };
        params.insert(
            0,
            Param {
                name,
                pattern: None,
                ty,
                default: None,
                optional: false,
                rest: false,
                span: at,
            },
        );
    }

    /// [`METHOD_THIS`] in front of `params` of a method (or getter, setter, or `function`
    /// property value) starting at `lo` whose `body` uses `this`.
    pub(super) fn method_this_arrow_param(
        &mut self,
        lo: u32,
        params: &mut Vec<ArrowParam>,
        body: &Block,
    ) {
        if super::fn_exprs::own_this(body).is_none() {
            return;
        }
        let name = Ident {
            name: METHOD_THIS.into(),
            span: Span::new(self.file, lo, lo),
        };
        params.insert(
            0,
            ArrowParam {
                name,
                pattern: None,
                ty: None,
                default: None,
                optional: false,
                rest: false,
            },
        );
    }

    /// `name(params) { body }` in an object literal, after the name: a property holding the
    /// function, `name: (params) => { body }`. The parameter types may be left out: they come
    /// from the object's context, as an arrow's do (a `ProxyHandler`'s `get(obj, prop) {}`).
    /// A body that uses `this` gets the [`METHOD_THIS`] parameter (an arrow's `this` is the
    /// enclosing one).
    fn finish_method_prop(&mut self, lo: u32, name: Ident, is_async: bool) -> PResult<ObjectProp> {
        let type_params = self.parse_generic_params()?;
        let mut params = self.parse_arrow_params()?;
        let (ret, throws) = if self.eat(Tok::Colon) {
            (Some(self.parse_ret_type()?), self.parse_throws_clause()?)
        } else {
            (None, None)
        };
        let body = self.parse_fn_body()?;
        self.method_this_arrow_param(lo, &mut params, &body);
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

/// The property name a number literal key stands for: the number as `String` spells it (`7`,
/// `1.5`, `1000` for `1e3`; `9007199254740992` for `9007199254740993`, which is not a double).
/// `None` for a literal with a type suffix (`1u8`).
pub(super) fn number_key(lit: &Lit) -> Option<String> {
    match lit {
        Lit::Int {
            value,
            suffix: None,
        } if *value < 1 << 53 => Some(value.to_string()),
        Lit::Int {
            value,
            suffix: None,
        } => Some(crate::js_number(*value as f64)),
        Lit::Float {
            value,
            suffix: None,
        } => Some(crate::js_number(*value)),
        _ => None,
    }
}
