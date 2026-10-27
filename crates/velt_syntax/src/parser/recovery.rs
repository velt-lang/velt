//! Error recovery: after a syntax error, skip tokens to a point where parsing can resume
//! (statement, item, or class-member boundary) so one mistake yields one diagnostic.

use super::Parser;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// Skips a `{ ... }` group (current token is `{`), including nested braces.
    pub(super) fn skip_balanced_braces(&mut self) {
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

    fn at_stmt_keyword(&mut self) -> bool {
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

    pub(super) fn at_item_keyword(&mut self) -> bool {
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

    /// Statement-level recovery: skip past the next `;`, or up to a `}`, a statement keyword or,
    /// outside parentheses and brackets, a line break before a token that can start a statement
    /// (where automatic semicolon insertion would have ended it), once at least one token past
    /// `stmt_start` has been consumed.
    pub(super) fn sync_stmt(&mut self, stmt_start: usize) {
        let mut depth = 0i32;
        loop {
            match self.peek() {
                Tok::Eof | Tok::RBrace => return,
                Tok::Semi => {
                    self.bump();
                    return;
                }
                Tok::LBrace => self.skip_balanced_braces(),
                _ if self.pos > stmt_start && self.at_stmt_keyword() => return,
                t if depth == 0
                    && self.pos > stmt_start
                    && Self::can_start_expr(t)
                    && !matches!(t, Tok::LParen | Tok::LBracket | Tok::Plus | Tok::Minus)
                    && self.line_break_before() =>
                {
                    return
                }
                t => {
                    depth += Self::paren_delta(t);
                    self.bump();
                }
            }
        }
    }

    /// +1 for `(` and `[`, -1 for `)` and `]`: the nesting recovery keeps track of.
    fn paren_delta(t: Tok) -> i32 {
        match t {
            Tok::LParen | Tok::LBracket => 1,
            Tok::RParen | Tok::RBracket => -1,
            _ => 0,
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
    /// balanced `{...}` (a method body), up to the closing `}`, or, outside parentheses and
    /// brackets, up to a line break (a member ends there, as with automatic semicolon
    /// insertion) once the member consumed a token (it started at `member_start`).
    pub(super) fn sync_member(&mut self, member_start: usize) {
        let start = member_start;
        let mut depth = 0i32;
        loop {
            if depth == 0 && self.pos > start && self.line_break_before() {
                return;
            }
            depth += Self::paren_delta(self.peek());
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
        let outer = self.begin_statement();
        let result = f(self);
        self.end_statement(outer);
        match result {
            Ok(v) => {
                // A member ends at a line break, so one that consumed nothing could return
                // `Ok` forever: always move on.
                if self.pos == start {
                    self.bump();
                }
                Some(v)
            }
            Err(_) => {
                self.sync_member(start);
                if self.pos == start {
                    self.bump();
                }
                None
            }
        }
    }
}
