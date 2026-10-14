//! Members of interface and object types that need more than a field parser: call signatures
//! (reported), the end of a member (`;`, `,` or a line break), and the number literal members
//! an `as` type rejects.

use super::Parser;
use crate::ast::*;
use crate::lexer::Tok;

impl<'a> Parser<'a> {
    /// A numeric literal type after `|` in an `as` type (`x as i32 | 1`): TypeScript reads a
    /// union there, and Velt read a bitwise or before; rather than change such a program's
    /// value silently, it is an error that shows both spellings.
    pub(super) fn reject_cast_number_member(&mut self, member: &TypeExpr) {
        let TypeExprKind::Literal(SignedLit {
            lit: Lit::Int { .. } | Lit::Float { .. },
            ..
        }) = &member.kind
        else {
            return;
        };
        if self.speculating != 0 {
            return;
        }
        self.diags.push(
            velt_common::Diagnostic::error(
                "a cast to a union with a number literal member is ambiguous",
                member.span,
            )
            .with_note(
                "write `(x as T) | n` for a bitwise or of the cast value, or `x as (T | n)` for a cast to the union type",
            ),
        );
    }

    /// At a call signature (`(x: T): R`, `<T>(x: T): T`) in an interface or object type:
    /// reported, as they are not supported yet, and skipped up to the member's end. True if
    /// there was one. (`new(x: T): R` is a method named `new` in Velt.)
    pub(super) fn skip_call_signature(&mut self) -> bool {
        if !(self.at(Tok::LParen) || self.at(Tok::Lt)) {
            return false;
        }
        let span = self.cur_span();
        if self.speculating == 0 {
            self.diags.push(
                velt_common::Diagnostic::error(
                    "call signatures in an interface or object type are not supported yet",
                    span,
                )
                .with_note("for a callable value, write a function type: `type F = (x: T) => R`"),
            );
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
