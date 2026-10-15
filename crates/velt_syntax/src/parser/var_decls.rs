//! `var` declarations. Directly in a function body or at the top level, `var x = e` declares
//! what `let x = e` does there: the variable belongs to the function (or module) either way, and
//! only reads before the declaration differ (`undefined` for `var`, which Velt reports as a read
//! before assignment). In a nested block, a loop or a `switch`, a `var` still belongs to the
//! whole function, which `let` does not express: that is reported (not supported yet).

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::Tok;

impl<'a> Parser<'a> {
    /// At `var name`, `var [a, b]` or `var { a }` (`var` is a reserved word, so never a name).
    pub(super) fn at_var_decl(&mut self) -> bool {
        self.at_word("var")
            && (Self::is_ident_like(self.nth(1))
                || matches!(self.nth(1), Tok::LBracket | Tok::LBrace))
    }

    /// A `var` statement; `var_ok` when it is directly in a function body.
    pub(super) fn var_stmt(&mut self, var_ok: bool) -> PResult<StmtKind> {
        // Reported, then read as `let` so that the rest of the function is still checked.
        if !var_ok {
            self.reject_nested_var("in a nested block, a loop or a `switch`");
        }
        let var = self.parse_var_decl()?;
        self.expect_semi()?;
        Ok(StmtKind::Var(var))
    }

    /// Reports the `var` at the cursor, which is `place`.
    pub(super) fn reject_nested_var(&mut self, place: &str) {
        let span = self.cur_span();
        self.diags.push(
            velt_common::Diagnostic::error(
                format!("`var` {place} is not supported yet"),
                span,
            )
            .with_note("a `var` belongs to the whole function, not to the block it is written in; declare it with `let` (or `const`), or move it to the top of the function body"),
        );
    }
}
