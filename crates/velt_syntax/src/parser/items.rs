//! Module and item parsing: items with `export`, functions and their signatures,
//! `declare function`, type aliases and top-level variables. Struct/class/interface/enum bodies
//! are in `type_decls`; imports and re-exports in `imports`.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use velt_common::Span;

impl<'a> Parser<'a> {
    /// Parses the whole file, recovering from errors item by item.
    pub(crate) fn parse_module(&mut self) -> Module {
        let mut items = Vec::new();
        let mut stmts = Vec::new();
        while !self.at(Tok::Eof) {
            let start = self.pos;
            match self.peek() {
                Tok::Semi => self.bump(),
                Tok::RBrace => {
                    let span = self.cur_span();
                    self.error("unexpected `}`", span);
                    self.bump();
                }
                _ if !self.at_item_start() => self.parse_stmt_recovering(&mut stmts),
                _ => match self.parse_item(true) {
                    Ok(item) => items.push(item),
                    Err(Fail) => self.sync_item(start),
                },
            }
            if self.pos == start {
                self.bump();
            }
        }
        if !stmts.is_empty() {
            self.finish_script(&mut items, stmts);
        }
        Module {
            items,
            span: Span::new(self.file, 0, self.src.len() as u32),
            jsx_import_source: self.lx.jsx_import_source.take(),
        }
    }

    /// One item, with optional `export`. `top` is false for items nested in blocks.
    pub(super) fn parse_item(&mut self, top: bool) -> PResult<Item> {
        let lo = self.cur_lo();
        let mut exported = self.at_kw(Kw::Export);
        if exported {
            if !top {
                let span = self.cur_span();
                self.error("`export` is only allowed at the top level", span);
            }
            self.bump();
            if self.at_export_list() {
                let kind = ItemKind::Import(self.parse_export_list()?);
                return Ok(Item {
                    kind,
                    exported,
                    span: self.span_from(lo),
                });
            }
            if self.at_kw(Kw::Default) {
                self.export_default()?;
            }
        }
        let kind = self.parse_item_kind(lo, top && !exported)?;
        if exported && matches!(kind, ItemKind::Extend(_)) {
            let span = Span::new(self.file, lo, lo + "export".len() as u32);
            self.error(
                "`extend` blocks cannot be exported (they apply wherever the module is loaded)",
                span,
            );
            exported = false;
        }
        Ok(Item {
            kind,
            exported,
            span: self.span_from(lo),
        })
    }

    fn parse_item_kind(&mut self, lo: u32, import_ok: bool) -> PResult<ItemKind> {
        let kind = match self.cur_kw() {
            Some(Kw::Import) => {
                if !import_ok {
                    let span = self.cur_span();
                    self.error("`import` is only allowed at the top level", span);
                }
                ItemKind::Import(self.parse_import()?)
            }
            Some(Kw::Function) => ItemKind::Function(self.parse_fn_decl(lo, false)?),
            Some(Kw::Async) if self.nth(1) == Tok::Kw(Kw::Function) => {
                self.bump();
                ItemKind::Function(self.parse_fn_decl(lo, true)?)
            }
            Some(Kw::Struct) => ItemKind::Struct(self.parse_type_decl(false)?),
            Some(Kw::Class) => ItemKind::Class(self.parse_type_decl(true)?),
            Some(Kw::Interface) => ItemKind::Interface(self.parse_interface()?),
            Some(Kw::Enum) => ItemKind::Enum(self.parse_enum()?),
            Some(Kw::Type) if Self::is_ident_like(self.nth(1)) => {
                ItemKind::TypeAlias(self.parse_type_alias()?)
            }
            Some(Kw::Const | Kw::Let) if self.at_class_binding() => {
                ItemKind::Class(self.parse_class_binding()?)
            }
            Some(Kw::Const | Kw::Let) => {
                let var = self.parse_var_decl()?;
                self.expect_semi()?;
                ItemKind::Var(var)
            }
            Some(Kw::Declare) => ItemKind::ExternFn(self.parse_declare(lo)?),
            _ if self.at_using_decl() => {
                let span = self.cur_span();
                self.error(
                    "`using` declarations are only allowed inside a block (a module's values are never disposed)",
                    span,
                );
                return Err(Fail);
            }
            None if self.at_extend() => ItemKind::Extend(self.parse_extend()?),
            _ => {
                self.error_expected("item");
                return Err(Fail);
            }
        };
        Ok(kind)
    }

    /// `declare [async] function name(...): R;`
    fn parse_declare(&mut self, lo: u32) -> PResult<FnSig> {
        self.bump(); // declare
        let is_async = self.eat_kw(Kw::Async);
        let sig = self.parse_fn_sig(lo, is_async)?;
        if sig.is_generator {
            self.error("a `declare function` cannot be a generator", sig.name.span);
        }
        self.expect_semi()?;
        Ok(sig)
    }

    /// At `function`. `lo` is the start of the item (may include `export`/`async`). A
    /// signature ending in `;` is a TypeScript-form overload: the declarations of the same name
    /// that follow it, up to the one with a body, are parsed with it (`FnDecl::overloads`).
    fn parse_fn_decl(&mut self, lo: u32, is_async: bool) -> PResult<FnDecl> {
        let mut sig = self.parse_fn_sig(lo, is_async)?;
        let mut exported = self.text(lo, self.src.len() as u32).starts_with("export");
        let mut sigs: Vec<(FnSig, bool)> = vec![];
        while self.at(Tok::Semi) {
            self.bump(); // ;
            self.signature_defaults(&sig);
            let (next_exported, next_async) = match self.at_function_after(&sig.name.name) {
                Some(Ok(next)) => next,
                Some(Err(other)) if self.next_fn_has_body() => {
                    self.wrong_implementation_name(&sig.name, other);
                    return Err(Fail);
                }
                Some(Err(_)) | None => {
                    self.missing_implementation(&sig.name);
                    return Err(Fail);
                }
            };
            let next_lo = self.cur_lo();
            if next_exported {
                self.bump();
            }
            if next_async {
                self.bump();
            }
            sigs.push((sig, exported));
            exported = next_exported;
            sig = self.parse_fn_sig(next_lo, next_async)?;
        }
        let body = self.parse_block()?;
        // TypeScript's TS2383, at each signature exported differently from the implementation.
        for (s, e) in &sigs {
            if *e != exported {
                self.error(
                    "overload signatures must all be exported or non-exported (TypeScript's TS2383)",
                    s.name.span,
                );
            }
        }
        Ok(FnDecl {
            sig,
            body,
            overloads: sigs.into_iter().map(|(s, _)| s).collect(),
        })
    }

    /// After an overload signature of `name`: is the next item `[export] [async] function`,
    /// `Ok` with whether it is exported and `async` when it is named `name`, `Err` with its name
    /// when it has another?
    fn at_function_after(&mut self, name: &str) -> Option<Result<(bool, bool), Span>> {
        let mut k = 0;
        let exported = self.nth(k) == Tok::Kw(Kw::Export);
        k += usize::from(exported);
        let is_async = self.nth(k) == Tok::Kw(Kw::Async);
        k += usize::from(is_async);
        if self.nth(k) != Tok::Kw(Kw::Function) {
            return None;
        }
        k += 1;
        if self.nth(k) == Tok::Star {
            k += 1;
        }
        if !Self::is_ident_like(self.nth(k)) {
            return None;
        }
        let t = self.tok(self.pos + k);
        match self.text(t.lo, t.hi) == name {
            true => Some(Ok((exported, is_async))),
            false => Some(Err(Span::new(self.file, t.lo, t.hi))),
        }
    }

    /// At `[export] [async] function`: does the function have a body (rather than being a
    /// signature)? Looks ahead without consuming anything.
    fn next_fn_has_body(&mut self) -> bool {
        let snap = self.snapshot();
        self.speculating += 1;
        while matches!(self.nth(0), Tok::Kw(Kw::Export | Kw::Async)) {
            self.bump();
        }
        let lo = self.cur_lo();
        let body = self.parse_fn_sig(lo, false).is_ok() && self.at(Tok::LBrace);
        self.speculating -= 1;
        self.restore(snap);
        body
    }

    /// TS2391 for an overload signature that no implementation follows.
    pub(super) fn missing_implementation(&mut self, name: &Ident) {
        self.error(
            format!("function implementation is missing or not immediately following the declaration of `{}`", name.name),
            name.span,
        );
    }

    /// TS2389: the declaration after overload signatures of `name` is named otherwise (at
    /// `other`, its name).
    pub(super) fn wrong_implementation_name(&mut self, name: &Ident, other: Span) {
        self.error(
            format!(
                "function implementation name must be `{}` (TypeScript's TS2389)",
                name.name
            ),
            other,
        );
    }

    /// TS2371: a default value in an overload signature (an optional parameter `p?: T` is
    /// fine).
    pub(super) fn signature_defaults(&mut self, sig: &FnSig) {
        for p in &sig.params {
            if p.default.is_some() && !p.optional && !p.rest {
                self.error(
                    "a parameter default is only allowed in a function or method implementation, not in an overload signature (TypeScript's TS2371)",
                    p.name.span,
                );
            }
        }
    }

    /// A function expression at `function` (after `async`, with `is_async`): `function*
    /// [name](...)[: R] { ... }`; the name may be left out (empty). Sema allows only generators.
    pub(super) fn parse_fn_expr(&mut self, lo: u32, is_async: bool) -> PResult<ExprKind> {
        self.expect_kw(Kw::Function, "function")?;
        let is_generator = self.eat(Tok::Star);
        let name = match Self::is_ident_like(self.peek()) {
            true => self.parse_binding_ident()?,
            false => Ident {
                name: String::new(),
                span: self.span_from(lo),
            },
        };
        let mut sig = self.parse_sig_rest(lo, name, is_async)?;
        sig.is_generator = is_generator;
        let body = self.parse_block()?;
        Ok(ExprKind::Function(Box::new(FnDecl {
            sig,
            body,
            overloads: vec![],
        })))
    }

    /// `function name(...)` or `function* name(...)` (a generator).
    fn parse_fn_sig(&mut self, lo: u32, is_async: bool) -> PResult<FnSig> {
        self.expect_kw(Kw::Function, "function")?;
        let is_generator = self.eat(Tok::Star);
        let name = self.parse_binding_ident()?;
        let mut sig = self.parse_sig_rest(lo, name, is_async)?;
        sig.is_generator = is_generator;
        Ok(sig)
    }

    /// After the function/method name: generics, parameters, optional return type and
    /// `throws` clause.
    pub(super) fn parse_sig_rest(
        &mut self,
        lo: u32,
        name: Ident,
        is_async: bool,
    ) -> PResult<FnSig> {
        let generics = self.parse_generic_params()?;
        let params = self.parse_params()?;
        let ret = if self.eat(Tok::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let throws = self.parse_throws_clause()?;
        Ok(FnSig {
            name,
            generics,
            params,
            ret,
            throws,
            is_async,
            is_generator: false,
            span: self.span_from(lo),
        })
    }

    /// `<T, U extends A & B = U>` (empty when there is no `<`) of a function, method, arrow,
    /// class, struct, interface or type alias: parameters may have defaults (`T = Default`), and
    /// a parameter without one may not follow one.
    pub(super) fn parse_generic_params(&mut self) -> PResult<Vec<GenericParam>> {
        let mut out: Vec<GenericParam> = Vec::new();
        if !self.eat(Tok::Lt) {
            return Ok(out);
        }
        while !self.at(Tok::Gt) {
            let name = self.parse_ident()?;
            let mut bounds = Vec::new();
            if self.eat_kw(Kw::Extends) {
                bounds.push(self.parse_type_no_union()?);
                while self.eat(Tok::Amp) {
                    bounds.push(self.parse_type_no_union()?);
                }
            }
            let default = self.type_param_default(&out, &name)?;
            out.push(GenericParam {
                name,
                bounds,
                default,
            });
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::Gt)?;
        Ok(out)
    }

    /// `mut` before a parameter / method name: not part of the language (mutation is
    /// inferred). Reported and skipped so the rest parses normally. Only when a name follows,
    /// so `mut` stays usable as an identifier.
    /// Optional `throws A | B` (a contextual keyword after a return type or parameter list).
    pub(super) fn parse_throws_clause(&mut self) -> PResult<Option<TypeExpr>> {
        if !self.at_word("throws") {
            return Ok(None);
        }
        self.bump();
        Ok(Some(self.parse_type()?))
    }

    pub(super) fn reject_mut_modifier(&mut self) {
        if self.at_word("mut") && Self::is_name(self.nth(1)) {
            // Reported even while speculating (arrow heads): a failed attempt rewinds it.
            let span = self.cur_span();
            self.diags.push(
                velt_common::Diagnostic::error("`mut` is not needed: mutation is inferred", span)
                    .with_note("remove `mut`"),
            );
            self.bump();
        }
    }

    /// `= Default` after a type parameter (`prev`: the parameters before).
    fn type_param_default(
        &mut self,
        prev: &[GenericParam],
        name: &Ident,
    ) -> PResult<Option<TypeExpr>> {
        if self.eat(Tok::Eq) {
            return Ok(Some(self.parse_type()?));
        }
        if prev.iter().any(|g| g.default.is_some()) {
            self.diags.push(
                velt_common::Diagnostic::error(
                    format!(
                        "type parameter `{}` needs a default: it follows one that has a default",
                        name.name
                    ),
                    name.span,
                )
                .with_note("move the parameters with defaults to the end"),
            );
        }
        Ok(None)
    }

    /// `( name: Type [= default], ... )`
    pub(super) fn parse_params(&mut self) -> PResult<Vec<Param>> {
        self.expect(Tok::LParen)?;
        let mut params = Vec::new();
        while !self.at(Tok::RParen) {
            let p = self.parse_param()?;
            if params.last().is_some_and(|q: &Param| q.rest) {
                self.error("a rest parameter must be the last parameter", p.span);
            }
            params.push(p);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        Ok(params)
    }

    pub(super) fn parse_param(&mut self) -> PResult<Param> {
        let lo = self.cur_lo();
        self.reject_mut_modifier();
        let rest = self.eat(Tok::DotDotDot);
        if self.at(Tok::PrivateName) {
            let span = self.cur_span();
            self.error("private names cannot be parameters", span);
            self.bump();
            return Err(Fail);
        }
        let name = self.parse_binding_ident()?;
        let optional = self.eat(Tok::Question);
        if !self.eat(Tok::Colon) {
            let msg = format!(
                "expected `:`, found {} (parameter types are required)",
                self.found()
            );
            let span = self.cur_span();
            self.error(msg, span);
            return Err(Fail);
        }
        let mut ty = self.parse_type()?;
        let mut default = if self.eat(Tok::Eq) {
            Some(self.parse_assign()?)
        } else {
            None
        };
        if optional {
            if let Some(d) = &default {
                let msg = format!(
                    "parameter `{}` cannot be optional and have a default value",
                    name.name
                );
                self.error(msg, d.span);
            }
            // `name?: T` is `name: T | null = null`.
            let span = Span::new(self.file, ty.span.hi, ty.span.hi);
            default = Some(self.mk_expr(ExprKind::Lit(Lit::Null), span));
            ty = super::types::or_null(ty);
        }
        if rest {
            if !matches!(ty.kind, TypeExprKind::Array(_)) {
                self.error(
                    "a rest parameter must have an array type (`...xs: T[]`)",
                    ty.span,
                );
            }
            if let Some(d) = &default {
                self.error("a rest parameter cannot have a default value", d.span);
            }
            // No arguments left over: an empty array.
            let span = Span::new(self.file, ty.span.hi, ty.span.hi);
            default = Some(self.mk_expr(ExprKind::Array(vec![]), span));
        }
        Ok(Param {
            name,
            ty,
            default,
            optional,
            rest,
            span: self.span_from(lo),
        })
    }

    fn parse_type_alias(&mut self) -> PResult<TypeAlias> {
        self.bump(); // type
        let name = self.parse_ident()?;
        let generics = self.parse_generic_params()?;
        self.expect(Tok::Eq)?;
        let ty = self.parse_type()?;
        self.expect_semi()?;
        Ok(TypeAlias { name, generics, ty })
    }

    /// `let|const pattern [: T] [= init]` (without the trailing `;`).
    pub(super) fn parse_var_decl(&mut self) -> PResult<VarDecl> {
        let lo = self.cur_lo();
        let kind = if self.eat_kw(Kw::Const) {
            VarKind::Const
        } else {
            self.expect_kw(Kw::Let, "let")?;
            VarKind::Let
        };
        let pattern = self.parse_binding_pattern()?;
        self.finish_var_decl(lo, kind, pattern)
    }

    /// Is the cursor at `using x` or `await using x` (a declaration, not an expression that
    /// mentions a variable named `using`)?
    pub(super) fn at_using_decl(&mut self) -> bool {
        let at =
            |p: &mut Self, n: usize| p.nth_word(n, "using") && Self::is_ident_like(p.nth(n + 1));
        at(self, 0) || (self.at_kw(Kw::Await) && at(self, 1))
    }

    /// `[await] using name [: T] = init` (without the trailing `;`; the caller checked
    /// [`Self::at_using_decl`]). Like TS, the binding is a plain name and the initializer is
    /// required.
    pub(super) fn parse_using_decl(&mut self) -> PResult<VarDecl> {
        let lo = self.cur_lo();
        let kind = if self.eat_kw(Kw::Await) {
            VarKind::AwaitUsing
        } else {
            VarKind::Using
        };
        self.bump(); // using
        let name = self.parse_binding_ident()?;
        let span = name.span;
        let pattern = self.mk_pat(PatternKind::Ident(name), span);
        let decl = self.finish_var_decl(lo, kind, pattern)?;
        if decl.init.is_none() {
            self.error("a `using` declaration must be initialized", decl.span);
        }
        Ok(decl)
    }

    /// Type annotation and initializer after the pattern of a variable declaration.
    pub(super) fn finish_var_decl(
        &mut self,
        lo: u32,
        kind: VarKind,
        pattern: Pattern,
    ) -> PResult<VarDecl> {
        let ty = if self.eat(Tok::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let init = if self.eat(Tok::Eq) {
            Some(self.parse_assign()?)
        } else {
            None
        };
        Ok(VarDecl {
            kind,
            pattern,
            ty,
            init,
            span: self.span_from(lo),
        })
    }
}
