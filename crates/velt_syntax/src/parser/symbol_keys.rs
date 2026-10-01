//! Computed member keys of well-known symbols: `[Symbol.dispose]` and `[Symbol.asyncDispose]`
//! as method names (`[Symbol.dispose]() {}`) and in member access (`x[Symbol.dispose]()`).
//! They become an [`Ident`] named [`SYMBOL_DISPOSE`] / [`SYMBOL_ASYNC_DISPOSE`], so later
//! stages treat them like any other member name.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::Tok;

impl<'a> Parser<'a> {
    /// Is the cursor at `[Symbol.<name>]`?
    pub(super) fn at_symbol_key(&mut self) -> bool {
        self.peek() == Tok::LBracket
            && self.nth_word(1, "Symbol")
            && self.nth(2) == Tok::Dot
            && Self::is_name(self.nth(3))
            && self.nth(4) == Tok::RBracket
    }

    /// `[Symbol.dispose]` / `[Symbol.asyncDispose]` (the caller checked [`Self::at_symbol_key`]).
    pub(super) fn parse_symbol_key(&mut self) -> PResult<Ident> {
        let lo = self.cur_lo();
        self.bump(); // [
        self.bump(); // Symbol
        self.bump(); // .
        let key = self.take_ident();
        self.bump(); // ]
        let span = self.span_from(lo);
        let name = match key.name.as_str() {
            "dispose" => SYMBOL_DISPOSE,
            "asyncDispose" => SYMBOL_ASYNC_DISPOSE,
            other => {
                self.error(
                    format!(
                        "`Symbol.{other}` is not supported: the only symbol keys are `[Symbol.dispose]` and `[Symbol.asyncDispose]`"
                    ),
                    span,
                );
                return Err(Fail);
            }
        };
        Ok(Ident {
            name: name.to_string(),
            span,
        })
    }

    /// A member name in a declaration: an identifier, a keyword, or a symbol key.
    pub(super) fn parse_member_name(&mut self) -> PResult<Ident> {
        if self.at_symbol_key() {
            return self.parse_symbol_key();
        }
        self.parse_prop_name()
    }

    /// Is token `pos + n` the plain identifier `word`?
    pub(super) fn nth_word(&mut self, n: usize, word: &str) -> bool {
        let t = self.tok(self.pos + n);
        t.kind == Tok::Ident && self.text(t.lo, t.hi) == word
    }
}
