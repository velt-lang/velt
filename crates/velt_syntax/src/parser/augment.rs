//! `declare global { … }` and `declare module "spec" { … }` (TypeScript's augmentation): the
//! declarations inside, an [`Augment`] item. Sema merges their interfaces into the declarations
//! they augment. The variables of `declare global` (`var NAME: T;`) are ambient declarations
//! when the caller asked for them (`crate::parse_with`, the `ambient` module), and an error
//! otherwise.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use crate::HOST_SCHEME;
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
                     // `declare module "wasm:<name>"`: functions the wasm host provides, not an augmentation.
        let host = module
            .as_ref()
            .is_some_and(|(spec, _)| spec.starts_with(HOST_SCHEME));
        let mut items = vec![];
        while !self.at(Tok::RBrace) && !self.at(Tok::Eof) {
            if self.eat(Tok::Semi) {
                continue;
            }
            let item_lo = self.cur_lo();
            // `export` is allowed (and means nothing more) inside the block.
            let exported = self.eat_kw(Kw::Export);
            if global && self.ambient.is_some() {
                // `var NAME: T;` (or `declare var`, `const`, `let`): an ambient variable, for
                // the caller that asked for them.
                let declare = usize::from(self.at_kw(Kw::Declare));
                if self.is_var_word(declare) {
                    if declare == 1 {
                        self.bump();
                    }
                    if self.parse_ambient_vars(true).is_err() {
                        self.skip_to_block_end();
                        return Err(Fail);
                    }
                    continue;
                }
            }
            let member = match host {
                true => self.parse_host_member(item_lo),
                false => self.parse_augment_member(item_lo, global),
            };
            let Ok(kind) = member else {
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

    /// One declaration in a `declare module "wasm:…"` block: a host import.
    fn parse_host_member(&mut self, lo: u32) -> PResult<ItemKind> {
        if self.at_kw(Kw::Function)
            || (self.at_kw(Kw::Async) && self.nth(1) == Tok::Kw(Kw::Function))
        {
            return Ok(ItemKind::ExternFn(self.parse_host_import(lo)?));
        }
        let span = Span::new(self.file, lo, self.cur_span().hi);
        self.error(
            "only functions can be declared in a `declare module \"wasm:…\"` block: it lists the functions the WebAssembly host provides",
            span,
        );
        Err(Fail)
    }

    /// One declaration in an augmentation block: an interface (which merges), or in
    /// `declare global` a namespace of types (declared in the module, as `declare namespace`).
    /// Anything else is reported.
    fn parse_augment_member(&mut self, lo: u32, global: bool) -> PResult<ItemKind> {
        match self.cur_kw() {
            Some(Kw::Interface) => self.parse_interface(),
            None if global && self.at_namespace(0) => {
                Ok(ItemKind::Namespace(self.parse_namespace()?))
            }
            _ => {
                let span = Span::new(self.file, lo, self.cur_span().hi);
                let what = match self.cur_kw() {
                    Some(Kw::Const | Kw::Let) => "a variable",
                    Some(Kw::Function) => "a function",
                    Some(Kw::Class) => "a class",
                    _ if self.at_word("var") => "a variable",
                    _ if self.at_word("namespace") => {
                        "a namespace (only `declare global` may hold one)"
                    }
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
