//! Members of interface and object types that need more than a field parser: call and
//! construct signatures and the end of a member (`;`, `,` or a line break).

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// At a call signature (`(x: T): R`, `<T>(x: T): T`) or a construct signature
    /// (`new (x: T): R`) in an interface or object type: the signature, else `None`. Sema
    /// gives it its type (and reports what it does not support). (In TypeScript `new(…)` there
    /// is always a construct signature; a method named `new` is written `"new"(…)`. In a
    /// class, `new()` is a method.)
    pub(super) fn parse_call_signature(&mut self) -> PResult<Option<CallSig>> {
        let construct = self.at(Tok::Kw(Kw::New)) && matches!(self.nth(1), Tok::LParen | Tok::Lt);
        if !(construct || self.at(Tok::LParen) || self.at(Tok::Lt)) {
            return Ok(None);
        }
        let lo = self.cur_lo();
        if construct {
            self.bump(); // new
        }
        let at = self.cur_span();
        let name = Ident {
            name: String::new(),
            span: velt_common::Span::new(at.file, at.lo, at.lo),
        };
        let sig_lo = self.cur_lo();
        let sig = self.parse_sig_rest(sig_lo, name, false, false)?;
        Ok(Some(CallSig {
            sig,
            construct,
            span: self.span_from(lo),
        }))
    }

    /// The end of an object type's member: `;` or `,` (consumed), or a line break before the
    /// next member (TypeScript's automatic semicolon insertion).
    pub(super) fn eat_type_member_end(&mut self) -> bool {
        self.eat(Tok::Semi) || self.eat(Tok::Comma) || self.line_break_before()
    }
}
