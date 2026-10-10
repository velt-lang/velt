//! What a parameter binds: a name, or a destructuring pattern (`function f([a, b]: [A, B])`,
//! `xs.map(([k, v]) => …)`, `({ a, b }: P) => …`), which gets a name no source can spell
//! (`#param0`) for the body to destructure. And the type of a parameter written with only a
//! default (`status = 302`), taken from the default as for class fields (`field_types`).

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::Tok;

impl Parser<'_> {
    /// The `index`-th parameter's name and, for `[…]` / `{…}`, its pattern.
    pub(super) fn parse_param_binding(
        &mut self,
        index: usize,
        rest: bool,
    ) -> PResult<(Ident, Option<Pattern>)> {
        if !matches!(self.peek(), Tok::LBracket | Tok::LBrace) {
            return Ok((self.parse_binding_ident()?, None));
        }
        // Only a pattern a parameter's `:`, `=`, `?`, `,` or `)` follows: otherwise the `[` / `{`
        // is reported where it stands, so recovery resumes as for any bad parameter.
        let parsed = self.speculate(|p| {
            let pattern = p.parse_binding_pattern()?;
            match p.peek() {
                Tok::Colon | Tok::Eq | Tok::Question | Tok::Comma | Tok::RParen => Ok(pattern),
                _ => Err(Fail),
            }
        });
        let Some(pattern) = parsed else {
            return Ok((self.parse_binding_ident()?, None));
        };
        if rest {
            self.param_error(
                "a rest parameter cannot be destructured yet; write `...xs: T[]` and destructure `xs` in the body",
                pattern.span,
            );
        }
        let name = Ident {
            name: format!("#param{index}"),
            span: pattern.span,
        };
        Ok((name, Some(pattern)))
    }

    /// An error that holds whatever the parse turns out to be, so it is kept while speculating
    /// on an arrow's parameters (a failed speculation drops it, as `check_binding_name` does).
    pub(super) fn param_error(&mut self, msg: &str, span: velt_common::Span) {
        self.diags.push(velt_common::Diagnostic::error(msg, span));
    }

    /// The type a parameter's default states (`= 302` is a `number`), else an error.
    pub(super) fn param_type_of(&mut self, name: &Ident, default: &Expr) -> PResult<TypeExpr> {
        if let Some(t) = super::field_types::stated_type(default) {
            return Ok(TypeExpr {
                span: default.span,
                ..t
            });
        }
        let msg = if name.name.starts_with('#') {
            "cannot infer the type of this parameter from its default; write `pattern: T = ...`"
                .to_string()
        } else {
            format!(
                "cannot infer the type of parameter `{}` from its default; write `{}: T = ...`",
                name.name, name.name
            )
        };
        self.error(msg, default.span);
        Err(Fail)
    }
}
