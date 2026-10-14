//! `declare global { … }` and `declare module "spec" { … }` (TypeScript's augmentation): the
//! declarations inside, an [`Augment`] item. Sema merges their interfaces into the declarations
//! they augment.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use velt_common::Span;

impl<'a> Parser<'a> {
    /// At `declare global {` or `declare module "…"`?
    pub(super) fn at_augment(&mut self) -> bool {
        (self.nth_word(1, "global") && self.nth(2) == Tok::LBrace)
            || (self.nth_word(1, "module") && matches!(self.nth(2), Tok::Str(_)))
    }

    /// `declare global { items }` / `declare module "spec" { items }` (the caller checked
    /// [`Self::at_augment`]).
    pub(super) fn parse_augment(&mut self) -> PResult<Augment> {
        let lo = self.cur_lo();
        self.bump(); // declare
        let global = self.at_word("global");
        self.bump(); // global / module
        let module = match global {
            true => None,
            false => {
                let Tok::Str(idx) = self.peek() else {
                    unreachable!("ICE: `at_augment` saw a string");
                };
                let span = self.cur_span();
                let spec = self.payload_text(idx);
                self.bump();
                Some((spec, span))
            }
        };
        if !self.at(Tok::LBrace) {
            let span = self.span_from(lo);
            self.error(
                "a shorthand ambient module declaration (`declare module \"…\";`) is not supported: Velt types every module from its source",
                span,
            );
            self.eat(Tok::Semi);
            return Err(Fail);
        }
        self.bump(); // {
        let mut items = vec![];
        while !self.at(Tok::RBrace) && !self.at(Tok::Eof) {
            if self.eat(Tok::Semi) {
                continue;
            }
            let item_lo = self.cur_lo();
            // `export` is allowed (and means nothing more) inside the block.
            let exported = self.eat_kw(Kw::Export);
            let Ok(kind) = self.parse_augment_member(item_lo) else {
                self.skip_to_block_end();
                return Err(Fail);
            };
            items.push(Item {
                kind,
                exported,
                span: self.span_from(item_lo),
            });
        }
        self.expect(Tok::RBrace)?;
        Ok(Augment { module, items })
    }

    /// After an error inside an augmentation block: skips past the block's `}`.
    fn skip_to_block_end(&mut self) {
        let mut depth = 1u32;
        loop {
            match self.peek() {
                Tok::LBrace => depth += 1,
                Tok::RBrace => depth -= 1,
                Tok::Eof => return,
                _ => {}
            }
            self.bump();
            if depth == 0 {
                return;
            }
        }
    }

    /// One declaration in an augmentation block: an interface (which merges), or a type alias.
    /// Anything else is reported.
    fn parse_augment_member(&mut self, lo: u32) -> PResult<ItemKind> {
        match self.cur_kw() {
            Some(Kw::Interface) => self.parse_interface(),
            _ => {
                let span = Span::new(self.file, lo, self.cur_span().hi);
                let what = match self.cur_kw() {
                    Some(Kw::Const | Kw::Let) => "a variable",
                    Some(Kw::Function) => "a function",
                    Some(Kw::Class) => "a class",
                    _ if self.at_word("var") => "a variable",
                    _ if self.at_word("namespace") => "a namespace",
                    _ => "this declaration",
                };
                self.error(
                    format!("{what} cannot be declared in `declare global` or `declare module` yet: only interfaces, which merge into the declaration they augment"),
                    span,
                );
                Err(Fail)
            }
        }
    }
}
