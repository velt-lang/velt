//! C declarations (docs/internals/design/c-ffi.md): `declare c("<library>") function name(…): R;`,
//! a function of a native library, called through the C ABI from `unsafe` code. `c` is a word
//! only there.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// At `declare c(`.
    pub(super) fn at_c_decl(&mut self) -> bool {
        self.at_kw(Kw::Declare) && self.nth_word(1, "c") && self.nth(2) == Tok::LParen
    }

    /// `declare c("<library>") [async] function name(…): R;` (the caller checked
    /// [`Self::at_c_decl`]). `async` parses, so sema can say it is not supported yet.
    pub(super) fn parse_c_decl(&mut self, lo: u32) -> PResult<FnSig> {
        self.bump(); // declare
        self.bump(); // c
        self.expect(Tok::LParen)?;
        let Tok::Str(idx) = self.peek() else {
            self.error_expected("the C library's name as a string");
            return Err(Fail);
        };
        let c = CLibrary {
            library: self.payload_text(idx),
            span: self.cur_span(),
        };
        self.bump();
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
