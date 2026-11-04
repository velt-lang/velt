//! The `declare` forms with an argument: `declare host("<module>") function` and
//! `declare generated("<spec>") function`.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use velt_common::Span;

impl<'a> Parser<'a> {
    /// `declare host("<module>") [async] function name(...): R;`: a function the JS host provides
    /// (`host` is a word only there). `async` parses, so sema can say why it is not allowed.
    pub(super) fn parse_host_decl(&mut self, lo: u32) -> PResult<FnSig> {
        self.bump(); // declare
        self.bump(); // host
        self.expect(Tok::LParen)?;
        let Tok::Str(idx) = self.peek() else {
            self.error_expected("the host's import module as a string");
            return Err(Fail);
        };
        let host = HostModule {
            module: self.payload_text(idx),
            span: self.cur_span(),
        };
        self.bump();
        self.expect(Tok::RParen)?;
        let is_async = self.eat_kw(Kw::Async);
        let mut sig = self.parse_fn_sig(lo, is_async)?;
        sig.host = Some(host);
        if sig.is_generator {
            self.error("a host function cannot be a generator", sig.name.span);
        }
        if !self.eat_signature_end() {
            self.expect_semi()?;
        }
        Ok(sig)
    }

    /// `declare generated("<spec>") [async] function name<T>(...): R;`: a function whose body a
    /// source generator writes per instance (`generated` is a word only there). Parsed as a
    /// function with an empty body; sema checks what it may be.
    pub(super) fn parse_generated_decl(&mut self, lo: u32) -> PResult<FnDecl> {
        self.bump(); // declare
        self.bump(); // generated
        self.expect(Tok::LParen)?;
        let Tok::Str(idx) = self.peek() else {
            self.error_expected("the generator module as a string");
            return Err(Fail);
        };
        let spec = GeneratorSpec {
            spec: self.payload_text(idx),
            span: self.cur_span(),
        };
        self.bump();
        self.expect(Tok::RParen)?;
        let is_async = self.eat_kw(Kw::Async);
        let mut sig = self.parse_fn_sig(lo, is_async)?;
        sig.generator = Some(spec);
        if sig.is_generator {
            self.error(
                "a generated function cannot be a generator function",
                sig.name.span,
            );
        }
        if !self.eat_signature_end() {
            self.expect_semi()?;
        }
        let span = Span::new(sig.span.file, sig.span.hi, sig.span.hi);
        Ok(FnDecl {
            sig,
            body: Block {
                stmts: vec![],
                span,
            },
            overloads: vec![],
        })
    }
}
