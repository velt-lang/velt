//! C declarations (docs/internals/design/c-ffi.md): `declare c("<library>") function name(…): R;`,
//! a function of a native library, called through the C ABI from `unsafe` code; `declare c type`,
//! `declare c struct` and `declare c const`. `c` is a word only there.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// At `declare c(`.
    pub(super) fn at_c_decl(&mut self) -> bool {
        self.at_kw(Kw::Declare) && self.nth_word(1, "c") && self.nth(2) == Tok::LParen
    }

    /// At `declare c type`.
    pub(super) fn at_c_type(&mut self) -> bool {
        self.at_kw(Kw::Declare) && self.nth_word(1, "c") && self.nth(2) == Tok::Kw(Kw::Type)
    }

    /// At `declare c struct`.
    pub(super) fn at_c_struct(&mut self) -> bool {
        self.at_kw(Kw::Declare) && self.nth_word(1, "c") && self.nth(2) == Tok::Kw(Kw::Struct)
    }

    /// `declare c struct S { fields }` (the caller checked [`Self::at_c_struct`]): a struct with
    /// C's layout. It parses as any struct does; sema says what a C struct can't have.
    pub(super) fn parse_c_struct(&mut self) -> PResult<TypeDecl> {
        self.bump(); // declare
        self.bump(); // c
        let mut decl = self.parse_type_decl(false)?;
        decl.c_struct = true;
        Ok(decl)
    }

    /// At `declare c const`.
    pub(super) fn at_c_const(&mut self) -> bool {
        self.at_kw(Kw::Declare) && self.nth_word(1, "c") && self.nth(2) == Tok::Kw(Kw::Const)
    }

    /// `declare c const NAME: T;` (the caller checked [`Self::at_c_const`]): a constant whose
    /// value the package's C headers give, so it has a type and never an initializer.
    pub(super) fn parse_c_const(&mut self, lo: u32) -> PResult<VarDecl> {
        self.bump(); // declare
        self.bump(); // c
        self.bump(); // const
        let name = self.parse_binding_ident()?;
        let span = name.span;
        let pattern = self.mk_pat(PatternKind::Ident(name), span);
        if !self.at(Tok::Colon) {
            self.error(
                "a C constant says its type: `declare c const NAME: T;`",
                span,
            );
            return Err(Fail);
        }
        self.bump();
        let ty = self.parse_type()?;
        if self.at(Tok::Eq) {
            let at = self.cur_span();
            self.error(
                "a C constant's value comes from its package's C headers, per target: it has no initializer",
                at,
            );
            return Err(Fail);
        }
        if !self.eat_signature_end() {
            self.expect_semi()?;
        }
        Ok(VarDecl {
            kind: VarKind::Const,
            pattern,
            ty: Some(ty),
            init: None,
            span: self.span_from(lo),
            c_const: true,
        })
    }

    /// `declare c type name;` (the caller checked [`Self::at_c_type`]): an opaque C type, a
    /// class without members whose constructor is private, so nothing makes one.
    pub(super) fn parse_c_type(&mut self) -> PResult<TypeDecl> {
        self.bump(); // declare
        self.bump(); // c
        self.bump(); // type
        let name = self.parse_binding_ident()?;
        if !self.eat_signature_end() {
            self.expect_semi()?;
        }
        Ok(TypeDecl {
            name,
            generics: vec![],
            extends: None,
            implements: vec![],
            fields: vec![],
            constructor: None,
            ctor_visibility: CtorVisibility::Private,
            methods: vec![],
            is_abstract: false,
            c_type: true,
            c_struct: false,
        })
    }

    /// A C function's options: `{ errno: true }`, the only one, which is off when left out.
    fn parse_c_options(&mut self) -> PResult<bool> {
        self.expect(Tok::LBrace)?;
        if !self.at_word("errno") {
            self.error_expected("`errno`, the only option of a C function");
            return Err(Fail);
        }
        self.bump();
        self.expect(Tok::Colon)?;
        if !self.at(Tok::Kw(Kw::True)) {
            self.error_expected("`true`: a C function that doesn't ask for `errno` leaves it out");
            return Err(Fail);
        }
        self.bump();
        self.eat(Tok::Comma);
        self.expect(Tok::RBrace)?;
        self.eat(Tok::Comma);
        Ok(true)
    }

    /// `declare c("<library>"[, { errno: true }]) [async] function name(…): R;` (the caller
    /// checked [`Self::at_c_decl`]). `async` parses, so sema can say it is not supported yet.
    pub(super) fn parse_c_decl(&mut self, lo: u32) -> PResult<FnSig> {
        self.bump(); // declare
        self.bump(); // c
        self.expect(Tok::LParen)?;
        let Tok::Str(idx) = self.peek() else {
            self.error_expected("the C library's name as a string");
            return Err(Fail);
        };
        let mut c = CLibrary {
            library: self.payload_text(idx),
            errno: false,
            span: self.cur_span(),
        };
        self.bump();
        if self.eat(Tok::Comma) && !self.at(Tok::RParen) {
            c.errno = self.parse_c_options()?;
        }
        self.expect(Tok::RParen)?;
        let is_async = self.eat_kw(Kw::Async);
        let mut sig = self.parse_fn_sig(lo, is_async)?;
        sig.c = Some(c);
        if sig.is_generator {
            self.error("a C function cannot be a generator", sig.name.span);
        }
        if !self.eat_signature_end() {
            self.expect_semi()?;
        }
        Ok(sig)
    }
}
