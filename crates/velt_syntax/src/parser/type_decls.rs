//! Nominal type declarations and their headers: `struct`/`class` (generics, `extends`,
//! `implements`), `interface`, `extend` blocks and `enum` (numeric or string members).
//! The members inside the bodies are parsed by `members`.

use super::members::Member;
use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// Loops over `{ member* }`, skipping `;`/`,` separators; `member` parses one member.
    fn parse_members(&mut self, mut member: impl FnMut(&mut Self) -> PResult<()>) -> PResult<()> {
        self.expect(Tok::LBrace)?;
        loop {
            match self.peek() {
                Tok::RBrace => {
                    self.bump();
                    return Ok(());
                }
                Tok::Eof => {
                    self.error_expected("`}`");
                    return Ok(());
                }
                Tok::Semi | Tok::Comma => self.bump(),
                _ => {
                    self.member_with_recovery(&mut member);
                }
            }
        }
    }

    /// `struct|class Name<T> [extends Base<T>] [implements A, B] { members }`
    pub(super) fn parse_type_decl(&mut self, is_class: bool) -> PResult<TypeDecl> {
        self.bump(); // struct / class
        let name = self.parse_ident()?;
        let generics = self.parse_generic_params()?;
        let extends = self.parse_class_extends(is_class)?;
        let mut implements = Vec::new();
        if self.eat_kw(Kw::Implements) {
            implements = self.parse_type_list()?;
        }
        let mut decl = TypeDecl {
            name,
            generics,
            extends,
            implements,
            fields: vec![],
            constructor: None,
            methods: vec![],
        };
        self.parse_members(|p| {
            let member = p.parse_member()?;
            p.add_type_member(&mut decl, member);
            Ok(())
        })?;
        Ok(decl)
    }

    /// `extends Base<T>`: one base class. On a struct, or with several bases, the clause is
    /// reported and (for structs) dropped, but still parsed so the declaration is checked.
    fn parse_class_extends(&mut self, is_class: bool) -> PResult<Option<TypeExpr>> {
        if !self.at_kw(Kw::Extends) {
            return Ok(None);
        }
        let span = self.cur_span();
        self.bump();
        let base = self.parse_type_no_union()?;
        if self.at(Tok::Comma) {
            let span = self.cur_span();
            self.error(
                "a class can extend at most one class (use `implements` for interfaces)",
                span,
            );
            self.bump();
            self.parse_type_list()?;
        }
        if !is_class {
            self.error(
                "a struct cannot use `extends` (structs have no inheritance; use `implements`)",
                span,
            );
            return Ok(None);
        }
        Ok(Some(base))
    }

    fn add_type_member(&mut self, decl: &mut TypeDecl, member: Member) {
        match member {
            Member::Field(f) => decl.fields.push(f),
            Member::Method(m) => decl.methods.push(m),
            Member::Constructor(c, fields) => {
                if decl.constructor.is_some() {
                    self.error("duplicate constructor", c.sig.name.span);
                }
                decl.constructor = Some(c);
                decl.fields.extend(fields);
            }
        }
    }

    /// `A, B<T>, C` (no unions).
    fn parse_type_list(&mut self) -> PResult<Vec<TypeExpr>> {
        let mut out = vec![self.parse_type_no_union()?];
        while self.eat(Tok::Comma) {
            out.push(self.parse_type_no_union()?);
        }
        Ok(out)
    }

    /// `interface Name<T> extends A, B { field: T; method(): R; withDefault(): R { ... } }`
    pub(super) fn parse_interface(&mut self) -> PResult<InterfaceDecl> {
        self.bump(); // interface
        let name = self.parse_ident()?;
        let generics = self.parse_generic_params()?;
        let mut extends = Vec::new();
        if self.eat_kw(Kw::Extends) {
            extends = self.parse_type_list()?;
        }
        let mut decl = InterfaceDecl {
            name,
            generics,
            extends,
            fields: vec![],
            methods: vec![],
        };
        self.parse_members(|p| p.parse_interface_member(&mut decl))?;
        Ok(decl)
    }

    /// Is the cursor at an `extend` block? `extend` is contextual: a type name or `<` must follow.
    pub(super) fn at_extend(&self) -> bool {
        self.at_word("extend") && (self.nth(1) == Tok::Lt || Self::is_ident_like(self.nth(1)))
    }

    /// `extend<T> Target<T> { methods }` — only methods may be added.
    pub(super) fn parse_extend(&mut self) -> PResult<ExtendDecl> {
        self.bump(); // extend
        let generics = self.parse_generic_params()?;
        let target = self.parse_type_no_union()?;
        let mut methods = Vec::new();
        self.parse_members(|p| {
            let span = match p.parse_member()? {
                Member::Method(m) => {
                    methods.push(m);
                    return Ok(());
                }
                Member::Field(f) => f.span,
                Member::Constructor(c, _) => c.sig.name.span,
            };
            p.error("`extend` blocks can only add methods", span);
            Ok(())
        })?;
        Ok(ExtendDecl {
            generics,
            target,
            methods,
        })
    }

    /// `enum Name { A, B = 5, C }` / `enum Dir { Up = "UP", Down = "DOWN" }`
    pub(super) fn parse_enum(&mut self) -> PResult<EnumDecl> {
        self.bump(); // enum
        let name = self.parse_ident()?;
        if self.at(Tok::Lt) {
            let span = self.cur_span();
            self.error("enums cannot be generic", span);
            return Err(Fail);
        }
        let mut variants = Vec::new();
        self.parse_members(|p| {
            variants.push(p.parse_variant()?);
            Ok(())
        })?;
        Ok(EnumDecl { name, variants })
    }

    fn parse_variant(&mut self) -> PResult<Variant> {
        let lo = self.cur_lo();
        let name = self.parse_ident()?;
        if self.at(Tok::LParen) {
            let span = self.cur_span();
            self.diags.push(
                velt_common::Diagnostic::error("enum members cannot have payloads", span)
                    .with_note("use a discriminated union: `type Shape = { kind: \"circle\"; r: f64 } | { kind: \"empty\" };`"),
            );
            return Err(Fail);
        }
        let discriminant = if self.eat(Tok::Eq) {
            Some(self.parse_assign()?)
        } else {
            None
        };
        let span = self.span_from(lo);
        if !self.eat(Tok::Comma) && !self.at(Tok::RBrace) {
            self.error_expected("`,` or `}`");
            return Err(Fail);
        }
        Ok(Variant {
            name,
            discriminant,
            span,
        })
    }
}
