//! `undefined` and `void expr` are not part of Velt: `null` is the only "nothing" (one way of
//! writing absence, so the null-vs-undefined bug class cannot exist). Each use is reported with
//! the fix-it note "use `null`" (editors offer it as a quick fix) and parses on as `null`, so
//! the rest of the file is still checked.

use super::{PResult, Parser};
use crate::ast::*;
use velt_common::{Diagnostic, Span};

/// The note every `undefined` diagnostic carries; `velt_lsp` turns it into a replacement.
const USE_NULL: &str = "use `null`";

impl<'a> Parser<'a> {
    fn undefined_error(&mut self, msg: &str, span: Span) {
        if self.speculating == 0 {
            self.diags
                .push(Diagnostic::error(msg, span).with_note(USE_NULL));
        }
    }

    /// `undefined` as a value (the cursor is at it): reported, parsed as `null`.
    pub(super) fn undefined_expr(&mut self, span: Span) -> ExprKind {
        self.bump();
        self.undefined_error("`undefined` is not part of Velt", span);
        ExprKind::Lit(Lit::Null)
    }

    /// `void expr` (the cursor is at `void`): reported, parsed as `null`.
    pub(super) fn void_expr(&mut self) -> PResult<ExprKind> {
        let lo = self.cur_lo();
        self.bump();
        self.parse_unary()?;
        let span = self.span_from(lo);
        self.undefined_error(
            "`void` expressions are `undefined`, which is not part of Velt",
            span,
        );
        Ok(ExprKind::Lit(Lit::Null))
    }

    /// `undefined` as a type (the cursor is at it): reported, parsed as `null`.
    pub(super) fn undefined_type(&mut self, span: Span) -> TypeExpr {
        self.bump();
        self.undefined_error("`undefined` is not part of Velt", span);
        TypeExpr {
            kind: TypeExprKind::Null,
            span,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::parse_file;
    use velt_common::FileId;

    fn messages(src: &str) -> Vec<(String, Vec<String>)> {
        let (_, diags) = parse_file(FileId(0), src);
        diags.into_iter().map(|d| (d.message, d.notes)).collect()
    }

    #[test]
    fn undefined_value_type_and_void_are_reported_once_each() {
        let src =
            "function f(a: string | undefined) {\n  const x = undefined;\n  const y = void 0;\n}";
        let found = messages(src);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found.iter().all(|(_, notes)| notes == &[super::USE_NULL]));
        assert!(found[2].0.starts_with("`void` expressions"));
    }

    #[test]
    fn undefined_as_a_protocol_types_return_type_is_left_to_sema() {
        let src = "function* g(): Generator<number, undefined, undefined> {}\nconst x: Box<undefined> = 1;";
        let found = messages(src);
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn a_finished_result_with_value_undefined_says_to_drop_it() {
        let found = messages("function f() { return { done: true, value: undefined }; }");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].1[0].ends_with("write `{ done: true }`"),
            "{found:?}"
        );
        // Elsewhere, `value: undefined` is `null`.
        let found = messages("function f() { return { done: false, value: undefined }; }");
        assert_eq!(found[0].1, [super::USE_NULL]);
    }

    #[test]
    fn yield_may_be_a_branch_of_a_conditional() {
        assert!(messages("function* g(c: bool) { c ? yield 1 : yield 2; }").is_empty());
    }

    #[test]
    fn undefined_as_a_property_name_is_fine() {
        assert!(messages("function f() { g(o.undefined); }").is_empty());
    }
}
