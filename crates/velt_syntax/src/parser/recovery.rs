//! Error recovery: after a syntax error, skip tokens to a point where parsing can resume
//! (statement, item, or class-member boundary) so one mistake yields one diagnostic.

use super::Parser;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// Skips a `{ ... }` group (current token is `{`), including nested braces.
    fn skip_balanced_braces(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.peek() {
                Tok::Eof => return,
                Tok::LBrace => depth += 1,
                Tok::RBrace => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        self.bump();
                        return;
                    }
                }
                _ => {}
            }
            self.bump();
        }
    }

    fn at_stmt_keyword(&self) -> bool {
        use Kw::*;
        matches!(
            self.cur_kw(),
            Some(
                Let | Const
                    | If
                    | While
                    | Do
                    | For
                    | Return
                    | Break
                    | Continue
                    | Throw
                    | Try
                    | Switch
                    | Case
                    | Default
                    | Function
                    | Class
                    | Struct
                    | Interface
                    | Enum
            )
        )
    }

    pub(super) fn at_item_keyword(&self) -> bool {
        use Kw::*;
        match self.cur_kw() {
            Some(
                Function | Async | Class | Struct | Interface | Enum | Import | Export | Declare
                | Const | Let,
            ) => true,
            Some(Type) => Self::is_ident_like(self.nth(1)),
            _ => false,
        }
    }

    /// Statement-level recovery: skip past the next `;`, or up to a `}` or a statement keyword
    /// (once at least one token past `stmt_start` has been consumed).
    pub(super) fn sync_stmt(&mut self, stmt_start: usize) {
        loop {
            match self.peek() {
                Tok::Eof | Tok::RBrace => return,
                Tok::Semi => {
                    self.bump();
                    return;
                }
                Tok::LBrace => self.skip_balanced_braces(),
                _ if self.pos > stmt_start && self.at_stmt_keyword() => return,
                _ => self.bump(),
            }
        }
    }

    /// Item-level recovery: skip to the next item keyword outside any braces.
    pub(super) fn sync_item(&mut self, item_start: usize) {
        loop {
            match self.peek() {
                Tok::Eof => return,
                Tok::Semi => {
                    self.bump();
                    return;
                }
                Tok::LBrace => self.skip_balanced_braces(),
                _ if self.pos > item_start && (self.at_item_keyword() || self.at_extend()) => {
                    return
                }
                _ => self.bump(),
            }
        }
    }

    /// Member-level recovery (class/struct/interface/enum bodies): skip past `;`/`,`, past a
    /// balanced `{...}` (a method body), or up to the closing `}`.
    pub(super) fn sync_member(&mut self) {
        loop {
            match self.peek() {
                Tok::Eof | Tok::RBrace => return,
                Tok::Semi | Tok::Comma => {
                    self.bump();
                    return;
                }
                Tok::LBrace => {
                    self.skip_balanced_braces();
                    return;
                }
                _ => self.bump(),
            }
        }
    }

    /// Runs `f` for one member; on failure, recovers and guarantees progress.
    pub(super) fn member_with_recovery<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> super::PResult<T>,
    ) -> Option<T> {
        let start = self.pos;
        match f(self) {
            Ok(v) => Some(v),
            Err(_) => {
                self.sync_member();
                if self.pos == start {
                    self.bump();
                }
                None
            }
        }
    }
}
