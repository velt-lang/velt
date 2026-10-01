//! Binding patterns (`let`/`const`/`for...of`/`catch`): identifiers, `_`, object and array
//! destructuring.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::Tok;
use velt_common::Span;

impl<'a> Parser<'a> {
    /// Identifier, `_`, object or array destructuring.
    pub(super) fn parse_binding_pattern(&mut self) -> PResult<Pattern> {
        self.guarded(|p| match p.peek() {
            Tok::LBrace => p.parse_object_pattern(),
            Tok::LBracket => p.parse_array_pattern(),
            t if Self::is_ident_like(t) => {
                let id = p.take_ident();
                Ok(p.ident_pattern(id))
            }
            _ => {
                p.error_expected("pattern");
                Err(Fail)
            }
        })
    }

    /// `x` binds; `_` is a wildcard.
    fn ident_pattern(&mut self, id: Ident) -> Pattern {
        let span = id.span;
        let kind = if id.name == "_" {
            PatternKind::Wildcard
        } else {
            PatternKind::Ident(id)
        };
        self.mk_pat(kind, span)
    }

    /// `{ a, b: pat, ...rest }`
    fn parse_object_pattern(&mut self) -> PResult<Pattern> {
        let lo = self.cur_lo();
        self.expect(Tok::LBrace)?;
        let mut fields = Vec::new();
        let mut rest = None;
        while !self.at(Tok::RBrace) {
            if self.eat(Tok::DotDotDot) {
                rest = Some(self.parse_ident()?);
                self.eat(Tok::Comma);
                break;
            }
            let shorthand_ok = self.at_ident_like();
            let key = self.parse_prop_name()?;
            let pat = if self.eat(Tok::Colon) {
                self.parse_binding_pattern()?
            } else if shorthand_ok {
                self.ident_pattern(key.clone())
            } else {
                self.error_expected("`:`");
                return Err(Fail);
            };
            fields.push((key, pat));
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBrace)?;
        let span = self.span_from(lo);
        Ok(self.mk_pat(PatternKind::Object { fields, rest }, span))
    }

    /// `[a, , b, ...rest]` — holes become wildcards.
    fn parse_array_pattern(&mut self) -> PResult<Pattern> {
        let lo = self.cur_lo();
        self.expect(Tok::LBracket)?;
        let mut elems = Vec::new();
        let mut rest = None;
        while !self.at(Tok::RBracket) {
            if self.eat(Tok::DotDotDot) {
                rest = Some(self.parse_ident()?);
                self.eat(Tok::Comma);
                break;
            }
            if self.at(Tok::Comma) {
                let at = self.cur_lo();
                elems.push(self.mk_pat(PatternKind::Wildcard, Span::new(self.file, at, at)));
                self.bump();
                continue;
            }
            elems.push(self.parse_binding_pattern()?);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBracket)?;
        let span = self.span_from(lo);
        Ok(self.mk_pat(PatternKind::Array { elems, rest }, span))
    }
}

#[cfg(test)]
mod tests {
    use crate::parse_file;
    use velt_common::FileId;

    #[test]
    fn kw_is_not_a_pattern() {
        let (_, diags) = parse_file(FileId(0), "function f() { let if = 1; }");
        assert!(diags[0].message.contains("expected pattern"));
    }
}
