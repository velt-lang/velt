//! Members of interface and object types that need more than a field parser: call and
//! construct signatures (reported) and the end of a member (`;`, `,` or a line break).

use super::Parser;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// At a call signature (`(x: T): R`, `<T>(x: T): T`) or a construct signature
    /// (`new (x: T): R`) in an interface or object type: reported, as they are not supported
    /// yet, and skipped up to the member's end. True if there was one. (In TypeScript `new(…)`
    /// there is always a construct signature; a method named `new` is written `"new"(…)`. In a
    /// class, `new()` is a method.)
    pub(super) fn skip_call_signature(&mut self) -> bool {
        let construct = self.at(Tok::Kw(Kw::New)) && matches!(self.nth(1), Tok::LParen | Tok::Lt);
        if !(construct || self.at(Tok::LParen) || self.at(Tok::Lt)) {
            return false;
        }
        let span = self.cur_span();
        if self.speculating == 0 {
            let d = match construct {
                true => velt_common::Diagnostic::error(
                    "construct signatures in an interface or object type are not supported yet",
                    span,
                )
                .with_note("pass a factory function instead (`create: (x: T) => C`), or the class's type itself"),
                false => velt_common::Diagnostic::error(
                    "call signatures in an interface or object type are not supported yet",
                    span,
                )
                .with_note("for a callable value, write a function type: `type F = (x: T) => R`"),
            };
            self.diags.push(d);
        }
        if construct {
            self.bump(); // new
        }
        // The signature, up to `;`, `,`, `}` or a line break after its parameters.
        let mut depth = 0u32;
        let mut params = false;
        loop {
            match self.peek() {
                Tok::Eof => break,
                Tok::Semi | Tok::Comma | Tok::RBrace if depth == 0 => break,
                _ if depth == 0 && params && self.line_break_before() => break,
                Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace => {
                    depth = depth.saturating_sub(1);
                    params |= depth == 0;
                }
                _ => {}
            }
            self.bump();
        }
        true
    }

    /// The end of an object type's member: `;` or `,` (consumed), or a line break before the
    /// next member (TypeScript's automatic semicolon insertion).
    pub(super) fn eat_type_member_end(&mut self) -> bool {
        self.eat(Tok::Semi) || self.eat(Tok::Comma) || self.line_break_before()
    }
}
