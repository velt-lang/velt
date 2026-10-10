//! Computed member keys that name a symbol: a well-known symbol (`[Symbol.iterator]`,
//! `[Symbol.dispose]`, `[Symbol.toStringTag]`, ...) as a method or field name and in member
//! access (`x[Symbol.dispose]()`), and a symbol constant (`[VNODE]: true`) as a member name in a
//! declaration or an object literal. Both become an [`Ident`] whose name keeps the brackets
//! (`[Symbol.iterator]`, [`SYMBOL_ITERATOR`]; `[VNODE]`, spanning `VNODE`), so later stages
//! treat them like any other member name; sema resolves `[VNODE]` to the symbol the constant
//! holds. No plain identifier can collide with them.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::Tok;

/// The well-known symbols (`Symbol.<name>`) a computed key may name.
const WELL_KNOWN: [&str; 15] = [
    "asyncDispose",
    "asyncIterator",
    "dispose",
    "hasInstance",
    "isConcatSpreadable",
    "iterator",
    "match",
    "matchAll",
    "replace",
    "search",
    "species",
    "split",
    "toPrimitive",
    "toStringTag",
    "unscopables",
];

impl<'a> Parser<'a> {
    /// Is the cursor at `[Symbol.<name>]`?
    pub(super) fn at_well_known_key(&mut self) -> bool {
        self.peek() == Tok::LBracket
            && self.nth_word(1, "Symbol")
            && self.nth(2) == Tok::Dot
            && Self::is_name(self.nth(3))
            && self.nth(4) == Tok::RBracket
    }

    /// Is the cursor at a symbol key of a declaration: `[Symbol.<name>]` or `[NAME]`?
    pub(super) fn at_symbol_key(&mut self) -> bool {
        self.at_well_known_key()
            || (self.peek() == Tok::LBracket
                && self.nth(1) == Tok::Ident
                && self.nth(2) == Tok::RBracket)
    }

    /// `[Symbol.<name>]` for a well-known symbol, or `[NAME]` (the caller checked
    /// [`Self::at_symbol_key`]).
    pub(super) fn parse_symbol_key(&mut self) -> PResult<Ident> {
        let lo = self.cur_lo();
        self.bump(); // [
        if self.nth(1) == Tok::RBracket {
            // The span is the constant's name alone: sema tells `[KEY]` from a quoted key
            // `"[KEY]"` (whose span includes the quotes) by it.
            let key = self.take_ident();
            self.bump(); // ]
            return Ok(Ident {
                name: format!("[{}]", key.name),
                span: key.span,
            });
        }
        self.bump(); // Symbol
        self.bump(); // .
        let key = self.take_ident();
        self.bump(); // ]
        let span = self.span_from(lo);
        if !WELL_KNOWN.contains(&key.name.as_str()) {
            self.error(
                format!(
                    "`Symbol.{}` is not a well-known symbol: a computed key names a well-known symbol (`[Symbol.iterator]`) or a module constant holding a symbol (`[KEY]`)",
                    key.name
                ),
                span,
            );
            return Err(Fail);
        }
        Ok(Ident {
            name: format!("[Symbol.{}]", key.name),
            span,
        })
    }

    /// A member name in a declaration: an identifier, a keyword, or a symbol key.
    pub(super) fn parse_member_name(&mut self) -> PResult<Ident> {
        if self.at_symbol_key() {
            return self.parse_symbol_key();
        }
        if self.at(Tok::PrivateName) {
            return Ok(self.take_ident());
        }
        self.parse_prop_name()
    }

    /// Is token `pos + n` the plain identifier `word`?
    pub(super) fn nth_word(&mut self, n: usize, word: &str) -> bool {
        let t = self.tok(self.pos + n);
        t.kind == Tok::Ident && self.text(t.lo, t.hi) == word
    }
}
