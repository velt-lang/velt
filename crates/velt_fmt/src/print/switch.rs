//! `switch` statements (prettier style): `case x:` / `default:` one indent level in, their
//! statements one more; a clause whose only statement is a block keeps it on the `case` line
//! (`case 1: {`).

use velt_syntax::ast::{Expr, StmtKind, SwitchCase};

use super::decls::braced;
use super::Printer;
use crate::doc::{cat, hardline, indent, Doc};

impl<'a> Printer<'a> {
    /// `switch (x) { cases }`; `end` is the end of the statement.
    pub(super) fn switch_stmt(&mut self, disc: &Expr, cases: &[SwitchCase], end: u32) -> Doc {
        let head = self.paren_head("switch", disc);
        let body = self.lines(
            cases,
            end,
            |c| (c.span.lo, c.span.hi),
            |_, _| false,
            |p, c| p.switch_case(c),
        );
        cat![head, " ", braced(body)]
    }

    fn switch_case(&mut self, c: &SwitchCase) -> Doc {
        let head = match &c.test {
            Some(t) => cat!["case ", self.expr(t), ":"],
            None => "default:".into(),
        };
        match c.body.as_slice() {
            [] => head,
            [only] if matches!(only.kind, StmtKind::Block(_)) => cat![head, " ", self.stmt(only)],
            stmts => {
                let body = self.lines(
                    stmts,
                    c.span.hi,
                    |s| (s.span.lo, s.span.hi),
                    |_, _| false,
                    |p, s| p.stmt(s),
                );
                cat![head, indent(cat![hardline(), body])]
            }
        }
    }
}
