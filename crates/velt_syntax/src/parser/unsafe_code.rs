//! `unsafe` code (docs/internals/design/c-ffi.md): `unsafe { … }` blocks, `unsafe { expr }` in
//! expression position, and the `unsafe` modifier of functions and methods. `unsafe` is a word
//! only there, so it stays an ordinary identifier everywhere else (`const unsafe = 1`).

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// At `unsafe {` on one line.
    pub(super) fn at_unsafe_block(&mut self) -> bool {
        self.at_word("unsafe") && self.nth(1) == Tok::LBrace && !self.line_break_after(0)
    }

    /// At `unsafe function` or `unsafe async function`, on one line.
    pub(super) fn at_unsafe_fn(&mut self) -> bool {
        if !self.at_word("unsafe") || self.line_break_after(0) {
            return false;
        }
        match self.nth(1) {
            Tok::Kw(Kw::Function) => true,
            Tok::Kw(Kw::Async) => self.nth(2) == Tok::Kw(Kw::Function),
            _ => false,
        }
    }

    /// `unsafe { … }` as a statement (the caller checked [`Self::at_unsafe_block`]).
    pub(super) fn parse_unsafe_block(&mut self) -> PResult<StmtKind> {
        self.bump(); // unsafe
        Ok(StmtKind::Unsafe(self.parse_block()?))
    }

    /// `unsafe { expr }` in expression position: one expression, whose value it is.
    pub(super) fn parse_unsafe_expr(&mut self) -> PResult<ExprKind> {
        self.bump(); // unsafe
        self.expect(Tok::LBrace)?;
        let e = self.allow_in(|p| p.parse_expr())?;
        if self.at(Tok::Semi) {
            let span = self.cur_span();
            self.diags.push(
                velt_common::Diagnostic::error(
                    "an `unsafe { … }` expression holds one expression",
                    span,
                )
                .with_note(
                    "for several statements, write an `unsafe { … }` block statement and assign the value inside it",
                ),
            );
            self.bump();
        }
        self.expect(Tok::RBrace)?;
        Ok(ExprKind::Unsafe(Box::new(e)))
    }

    /// `unsafe [async] function name(…) { … }` (the caller checked [`Self::at_unsafe_fn`]).
    pub(super) fn parse_unsafe_fn(&mut self, lo: u32) -> PResult<FnDecl> {
        self.bump(); // unsafe
        let is_async = self.eat_kw(Kw::Async);
        self.parse_fn_decl(lo, is_async, true)
    }

    /// `unsafe [Symbol.dispose]()`, `unsafe toString()`: a symbol-keyed method, and each of the
    /// methods conversions and `await` look for by name, is called by the language itself
    /// (disposal at the end of a scope, `for…of`, operators, `String(x)`, `JSON.stringify`),
    /// where no `unsafe` block can be.
    pub(super) fn unsafe_implicit_method(&mut self, sig: &FnSig) {
        let name = sig.name.name.as_str();
        let implicit = name.starts_with('[')
            || matches!(
                name,
                "toString" | "toLocaleString" | "valueOf" | "toJSON" | "then"
            );
        if sig.is_unsafe && implicit {
            self.error(
                format!(
                    "`{}` can't be unsafe: the language calls it implicitly, where no `unsafe` block can be",
                    sig.name.name
                ),
                sig.name.span,
            );
        }
    }

    /// Overload signatures and their implementation are all unsafe or all safe: a call is
    /// checked against a signature, so one safe signature of an unsafe function would let a
    /// call skip the unsafe context.
    pub(super) fn overloads_agree_on_unsafe<'s>(
        &mut self,
        overloads: impl IntoIterator<Item = &'s FnSig>,
        imp: &FnSig,
    ) {
        for o in overloads {
            if o.is_unsafe != imp.is_unsafe {
                self.error(
                    "overload signatures and the implementation must all be `unsafe` or all be safe",
                    o.name.span,
                );
            }
        }
    }
}
