//! `switch (x) { case a: ...; default: ... }` — JS syntax: each clause holds a statement list
//! (no braces needed; fallthrough into the next clause unless it `break`s).

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    pub(super) fn parse_switch(&mut self) -> PResult<StmtKind> {
        self.bump(); // switch
        let discriminant = self.parse_paren_cond()?;
        self.expect(Tok::LBrace)?;
        let mut cases = Vec::new();
        loop {
            match self.peek() {
                Tok::RBrace => {
                    self.bump();
                    break;
                }
                Tok::Eof => {
                    self.error_expected("`}`");
                    break;
                }
                _ => {}
            }
            cases.push(self.parse_switch_case()?);
        }
        Ok(StmtKind::Switch {
            discriminant,
            cases,
        })
    }

    /// `case e:` / `default:` followed by statements up to the next clause or `}`.
    fn parse_switch_case(&mut self) -> PResult<SwitchCase> {
        let lo = self.cur_lo();
        let test = if self.eat_kw(Kw::Case) {
            Some(self.parse_expr()?)
        } else if self.eat_kw(Kw::Default) {
            None
        } else {
            self.error_expected("`case` or `default`");
            return Err(Fail);
        };
        self.expect(Tok::Colon)?;
        let mut body = Vec::new();
        while !(self.at_kw(Kw::Case)
            || self.at_kw(Kw::Default)
            || self.at(Tok::RBrace)
            || self.at(Tok::Eof))
        {
            self.parse_stmt_recovering(&mut body);
        }
        Ok(SwitchCase {
            test,
            body,
            span: self.span_from(lo),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::{Item, ItemKind, StmtKind};
    use crate::parse_file;
    use velt_common::FileId;

    fn first_stmt(src: &str) -> StmtKind {
        let (m, diags) = parse_file(FileId(0), src);
        assert!(diags.is_empty(), "{diags:?}");
        let Item {
            kind: ItemKind::Function(f),
            ..
        } = &m.items[0]
        else {
            panic!("expected a function");
        };
        f.body.stmts[0].kind.clone()
    }

    #[test]
    fn clauses_hold_statement_lists() {
        let s = first_stmt(
            "function f() { switch (x) { case 1: case 2: a(); b(); break; default: { c(); } } }",
        );
        let StmtKind::Switch { cases, .. } = s else {
            panic!("expected a switch");
        };
        assert_eq!(cases.len(), 3);
        assert!(cases[0].body.is_empty());
        assert_eq!(cases[1].body.len(), 3);
        assert!(cases[2].test.is_none());
    }

    #[test]
    fn missing_colon_is_reported() {
        let (_, diags) = parse_file(FileId(0), "function f() { switch (x) { case 1 a(); } }");
        assert!(diags[0].message.contains("expected `:`"), "{diags:?}");
    }
}
