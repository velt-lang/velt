//! `namespace N { … }` and `declare namespace N { … }` (TypeScript's namespaces, types only): a
//! [`Namespace`] item holding type aliases, interfaces and namespaces. `namespace A.B { … }` is
//! `A` holding the exported namespace `B`. Values in a namespace (`const`, `function`, `class`,
//! `enum`) are reported: they would need a run-time object.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use velt_common::Span;

impl<'a> Parser<'a> {
    /// At `namespace N {` or `namespace N.`? `n`: the offset of `namespace` from the cursor.
    pub(super) fn at_namespace(&mut self, n: usize) -> bool {
        self.nth_word(n, "namespace")
            && !self.line_break_after(n)
            && Self::is_ident_like(self.nth(n + 1))
            && matches!(self.nth(n + 2), Tok::LBrace | Tok::Dot)
    }

    /// `[declare] namespace A[.B…] { members }` (the caller checked [`Self::at_namespace`]).
    pub(super) fn parse_namespace(&mut self) -> PResult<Namespace> {
        let declare = self.eat_kw(Kw::Declare);
        self.bump(); // namespace
        let mut names = vec![self.parse_ident()?];
        while self.eat(Tok::Dot) {
            names.push(self.parse_ident()?);
        }
        self.expect(Tok::LBrace)?;
        let mut items = vec![];
        while !self.at(Tok::RBrace) && !self.at(Tok::Eof) {
            if self.eat(Tok::Semi) {
                continue;
            }
            let item_lo = self.cur_lo();
            let start = self.pos;
            let exported = self.eat_kw(Kw::Export);
            match self.parse_namespace_member(item_lo) {
                Ok(Some(kind)) => items.push(Item {
                    kind,
                    exported,
                    span: self.span_from(item_lo),
                }),
                Ok(None) => {}
                Err(Fail) => self.sync_namespace_member(start),
            }
        }
        self.expect(Tok::RBrace)?;
        // `namespace A.B { … }`: the innermost name holds the members.
        let mut ns = Namespace {
            name: names.pop().expect("ICE: a namespace has a name"),
            items,
            declare,
        };
        while let Some(outer) = names.pop() {
            let span = Span::new(self.file, ns.name.span.lo, self.prev_hi);
            ns = Namespace {
                name: outer,
                items: vec![Item {
                    kind: ItemKind::Namespace(ns),
                    exported: true,
                    span,
                }],
                declare,
            };
        }
        Ok(ns)
    }

    /// One member of a namespace body after its `export`: a type alias, an interface or a
    /// nested namespace. A value is parsed (for recovery), reported and dropped (`None`).
    fn parse_namespace_member(&mut self, lo: u32) -> PResult<Option<ItemKind>> {
        match self.cur_kw() {
            Some(Kw::Interface) => return self.parse_interface().map(Some),
            Some(Kw::Type) if Self::is_ident_like(self.nth(1)) => {
                return self
                    .parse_type_alias()
                    .map(|a| Some(ItemKind::TypeAlias(a)));
            }
            Some(Kw::Declare) if self.at_namespace(1) => {
                return self.parse_namespace().map(|n| Some(ItemKind::Namespace(n)));
            }
            None if self.at_namespace(0) => {
                return self.parse_namespace().map(|n| Some(ItemKind::Namespace(n)));
            }
            _ => {}
        }
        let what = match self.cur_kw() {
            Some(Kw::Const | Kw::Let) => "variables",
            Some(Kw::Function | Kw::Async) => "functions",
            Some(Kw::Class) => "classes",
            Some(Kw::Enum) => "enums",
            Some(Kw::Import) => "imports",
            _ if self.at_word("var") => "variables",
            _ => {
                self.error_expected("a type, interface or namespace");
                return Err(Fail);
            }
        };
        let item = self.parse_item_kind(lo, false);
        let span = Span::new(self.file, lo, self.prev_hi.max(lo));
        self.diags.push(
            velt_common::Diagnostic::error(
                format!("{what} in a namespace are not supported yet: a namespace may hold types, interfaces and namespaces"),
                span,
            )
            .with_note("declare the value at the top level of the module (`export const x = …`) and import it by name"),
        );
        item.map(|_| None)
    }

    /// After an error in a namespace member: skips to the next member (past a `;`, up to an
    /// item keyword) or up to the namespace's `}`, always moving on from `start`.
    fn sync_namespace_member(&mut self, start: usize) {
        loop {
            match self.peek() {
                Tok::Eof | Tok::RBrace => break,
                Tok::Semi => {
                    self.bump();
                    break;
                }
                Tok::LBrace => self.skip_balanced_braces(),
                _ if self.pos > start && (self.at_item_keyword() || self.at_namespace(0)) => break,
                _ => self.bump(),
            }
        }
        if self.pos == start {
            self.bump();
        }
    }
}
