//! Binding patterns (`let`/`const`/`for...of`/`catch`): identifiers, `_`, object and array
//! destructuring, and defaults inside them (`{ a = 1 }`, `[x = 0]`).

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
        self.check_binding_name(&id);
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
                rest = Some(self.parse_binding_ident()?);
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
            let pat = self.with_default(pat)?;
            fields.push((key, pat));
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBrace)?;
        let span = self.span_from(lo);
        Ok(self.mk_pat(PatternKind::Object { fields, rest }, span))
    }

    /// `pat = value` after a field or element pattern.
    fn with_default(&mut self, pat: Pattern) -> PResult<Pattern> {
        if !self.eat(Tok::Eq) {
            return Ok(pat);
        }
        let lo = pat.span.lo;
        let value = self.parse_assign()?;
        let span = self.span_from(lo);
        let kind = PatternKind::Default {
            pattern: Box::new(pat),
            value: Box::new(value),
        };
        Ok(self.mk_pat(kind, span))
    }

    /// `[a, , b, ...rest]` — holes become wildcards.
    fn parse_array_pattern(&mut self) -> PResult<Pattern> {
        let lo = self.cur_lo();
        self.expect(Tok::LBracket)?;
        let mut elems = Vec::new();
        let mut rest = None;
        while !self.at(Tok::RBracket) {
            if self.eat(Tok::DotDotDot) {
                rest = Some(self.parse_binding_ident()?);
                self.eat(Tok::Comma);
                break;
            }
            if self.at(Tok::Comma) {
                let at = self.cur_lo();
                elems.push(self.mk_pat(PatternKind::Wildcard, Span::new(self.file, at, at)));
                self.bump();
                continue;
            }
            let pat = self.parse_binding_pattern()?;
            elems.push(self.with_default(pat)?);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBracket)?;
        let span = self.span_from(lo);
        Ok(self.mk_pat(PatternKind::Array { elems, rest }, span))
    }

    /// The name a declaration introduces (function, class, parameter, import).
    pub(super) fn parse_binding_ident(&mut self) -> PResult<Ident> {
        let id = self.parse_ident()?;
        self.check_binding_name(&id);
        Ok(id)
    }

    /// Velt modules are strict-mode code, where `arguments` and `eval` cannot be declared (as in
    /// TypeScript). Reported even while speculating: a failed speculation drops it with the
    /// other diagnostics, a successful one keeps the binding it reports on.
    pub(super) fn check_binding_name(&mut self, id: &Ident) {
        if matches!(id.name.as_str(), "arguments" | "eval") {
            let msg = format!(
                "invalid use of `{}` in strict mode: a declaration cannot be named `arguments` or `eval`",
                id.name
            );
            self.diags
                .push(velt_common::Diagnostic::error(msg, id.span));
        }
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
