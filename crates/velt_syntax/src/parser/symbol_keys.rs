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
        self.symbol_key_len(0) > 0
    }

    /// The number of tokens of the symbol key (`[Symbol.<name>]`: 5, `[NAME]`: 3) starting `n`
    /// tokens after the cursor, or 0 if there is none.
    pub(super) fn symbol_key_len(&mut self, n: usize) -> usize {
        if self.nth(n) != Tok::LBracket {
            return 0;
        }
        if self.nth_word(n + 1, "Symbol")
            && self.nth(n + 2) == Tok::Dot
            && Self::is_name(self.nth(n + 3))
            && self.nth(n + 4) == Tok::RBracket
        {
            return 5;
        }
        match self.nth(n + 1) == Tok::Ident && self.nth(n + 2) == Tok::RBracket {
            true => 3,
            false => 0,
        }
    }

    /// The number of tokens of the computed key of a declared member starting `n` tokens after
    /// the cursor, or 0: a symbol key ([`Self::symbol_key_len`]), a string or number literal
    /// (`["k"]`, `[1]`), or a constant's or enum member's path (`[Flags.SKIP]`, `[ns.E.A]`).
    pub(super) fn member_key_len(&mut self, n: usize) -> usize {
        match self.symbol_key_len(n) {
            0 if self.nth(n) == Tok::LBracket => {}
            len => return len,
        }
        if matches!(self.nth(n + 1), Tok::Str(_) | Tok::Int(_) | Tok::Float(_)) {
            return match self.nth(n + 2) == Tok::RBracket {
                true => 3,
                false => 0,
            };
        }
        if self.nth(n + 1) != Tok::Ident || self.nth_word(n + 1, "Symbol") {
            return 0;
        }
        let mut k = n + 2;
        while self.nth(k) == Tok::Dot && Self::is_name(self.nth(k + 1)) {
            k += 2;
        }
        match self.nth(k) == Tok::RBracket {
            true => k + 1 - n,
            false => 0,
        }
    }

    /// The computed key of a declared member (the caller checked [`Self::member_key_len`]): a
    /// string or number literal names the member as if unquoted (`["k"]` is `k`, `[1.5]` is
    /// `1.5`); a path names it as `[Flags.SKIP]`, spanning `Flags.SKIP`, which sema resolves
    /// like a constant's `[KEY]`; the rest is [`Self::parse_symbol_key`].
    pub(super) fn parse_member_key(&mut self) -> PResult<Ident> {
        match self.nth(1) {
            Tok::Str(idx) => {
                self.bump(); // [
                let key = Ident {
                    name: self.payload_text(idx),
                    span: self.cur_span(),
                };
                self.bump();
                self.bump(); // ]
                self.check_quoted_key(&key);
                return Ok(key);
            }
            Tok::Int(_) | Tok::Float(_) => {
                self.bump(); // [
                let span = self.cur_span();
                let lit = self.literal_at_cursor();
                let Some(name) = lit.as_ref().and_then(super::primary::number_key) else {
                    self.error_expected("property name");
                    return Err(Fail);
                };
                self.bump();
                self.bump(); // ]
                return Ok(Ident { name, span });
            }
            _ => {}
        }
        if self.symbol_key_len(0) > 0 {
            return self.parse_symbol_key();
        }
        self.bump(); // [
        let lo = self.cur_lo();
        let first = self.take_ident();
        let mut path = first.name.clone();
        while self.eat(Tok::Dot) {
            path.push('.');
            path.push_str(&self.take_ident().name);
        }
        self.bump(); // ]
        self.computed_keys.insert(first.name);
        Ok(Ident {
            name: format!("[{path}]"),
            span: velt_common::Span::new(self.file, lo, lo + path.len() as u32),
        })
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
            self.computed_keys.insert(key.name.clone());
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

    /// At a computed key that makes a symbol in place: `[Symbol("x")]`, `[Symbol.for("q")]`?
    pub(super) fn at_inline_symbol_key(&mut self) -> bool {
        self.at(Tok::LBracket)
            && self.nth_word(1, "Symbol")
            && (self.nth(2) == Tok::LParen
                || (self.nth(2) == Tok::Dot
                    && self.nth(3) == Tok::Kw(crate::lexer::Kw::For)
                    && self.nth(4) == Tok::LParen))
    }

    /// Reports the key [`Self::at_inline_symbol_key`] found: a member named by a symbol needs
    /// the symbol in a module constant, so that every use names the same symbol.
    pub(super) fn reject_inline_symbol_key(&mut self) -> Fail {
        let span = self.cur_span();
        if self.speculating == 0 {
            self.diags.push(
                velt_common::Diagnostic::error(
                    "a symbol made in a computed key is not supported: declare it as a module constant (`const KEY = Symbol(\"x\")`) and write `[KEY]`",
                    span,
                )
                .with_note("Velt objects have fixed fields: a computed key names a well-known symbol (`[Symbol.iterator]`) or a module constant the compiler knows"),
            );
        }
        Fail
    }

    /// A member name in a declaration: an identifier, a keyword, or a computed key
    /// ([`Self::member_key_len`]).
    pub(super) fn parse_member_name(&mut self) -> PResult<Ident> {
        if self.at_inline_symbol_key() {
            return Err(self.reject_inline_symbol_key());
        }
        if self.member_key_len(0) > 0 {
            return self.parse_member_key();
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
