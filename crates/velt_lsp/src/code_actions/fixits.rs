//! Fixes recognized by a diagnostic's message and notes alone: `mut` removal, `undefined` (and
//! `void 0`) → `null`, a non-`bool` condition (`if (count)`) or negation (`!count`) → an explicit
//! comparison (`count !== 0`, `name !== ""`, `user !== null`, `count === 0`), and `async` for a
//! method whose promise must carry its errors.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast::{self, ExprKind as E};

use super::char_before;
use crate::analysis::Analysis;
use crate::syntax_walk::{self, Visit};

/// Note of the parser's `undefined` / `void expr` diagnostics.
const USE_NULL_NOTE: &str = "use `null`";

/// Message of the parser's `mut` diagnostic.
const MUT_MESSAGE: &str = "`mut` is not needed: mutation is inferred";

/// Part of sema's message for a synchronous method returning a promise that must be `async`
/// (its primary label is the method's name).
const MUST_BE_ASYNC: &str = " must be `async`: ";

/// `(title, edits)` of the fix for `d` (primary label `span`), if it is one of these.
pub fn for_diagnostic(
    analysis: &Analysis,
    d: &Diagnostic,
    span: Span,
) -> Option<(String, Vec<(Span, String)>)> {
    let text = analysis.snippet(span);
    if d.message == MUT_MESSAGE {
        return Some((
            "Remove `mut`".into(),
            vec![(remove_mut(analysis, span), String::new())],
        ));
    }
    if d.message.contains(MUST_BE_ASYNC) {
        let at = Span::new(span.file, span.lo, span.lo);
        return Some(("Add `async`".into(), vec![(at, "async ".into())]));
    }
    if d.notes.iter().any(|n| n == USE_NULL_NOTE) {
        let title = format!("Replace `{text}` with `null`");
        return Some((title, vec![(span, "null".into())]));
    }
    if d.message == "mismatched types" {
        let found = d
            .notes
            .iter()
            .find_map(|n| n.strip_prefix("expected bool, found "))?;
        return compare_to_zero_value(analysis, span, span, found, "!==");
    }
    let negated = d
        .message
        .strip_prefix("cannot apply unary operator `!` to type `")
        .and_then(|rest| rest.strip_suffix('`'));
    if let (Some(found), Some(operand)) = (negated, text.strip_prefix('!')) {
        let skipped = text.len() - operand.trim_start().len();
        let operand = Span::new(span.file, span.lo + skipped as u32, span.hi);
        return compare_to_zero_value(analysis, span, operand, found, "===");
    }
    None
}

/// `mut` plus the whitespace after it.
fn remove_mut(analysis: &Analysis, span: Span) -> Span {
    let rest = &analysis.text()[span.hi as usize..];
    let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    Span::new(span.file, span.lo, span.hi + ws as u32)
}

/// Replace `whole` by `operand <op> <zero value of type found>`: `count` → `count !== 0` for a
/// condition, `!count` → `count === 0` for a negation.
fn compare_to_zero_value(
    analysis: &Analysis,
    whole: Span,
    operand: Span,
    found: &str,
    op: &str,
) -> Option<(String, Vec<(Span, String)>)> {
    let zero = if found.ends_with("| null") || found.starts_with("null |") {
        "null"
    } else if found == "string" {
        "\"\""
    } else if matches!(found, "f32" | "f64") {
        "0.0"
    } else if is_integer(found) {
        "0"
    } else {
        return None;
    };
    let text = analysis.snippet(operand);
    let operand = if binds_tighter_than_equality(analysis, operand) {
        text.to_string()
    } else {
        format!("({text})")
    };
    let mut replacement = format!("{operand} {op} {zero}");
    if char_before(analysis.text(), whole.lo) == Some(b'!') {
        replacement = format!("({replacement})");
    }
    let title = format!("Compare with `{zero}`");
    Some((title, vec![(whole, replacement)]))
}

fn is_integer(ty: &str) -> bool {
    matches!(
        ty,
        "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize" | "number"
    )
}

/// Whether the expression at exactly `span` can be an operand of `!==` without parentheses.
fn binds_tighter_than_equality(analysis: &Analysis, span: Span) -> bool {
    struct Find {
        span: Span,
        found: Option<bool>,
    }
    impl<'a> Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            if e.span != self.span || self.found.is_some() {
                return;
            }
            let tight = match &e.kind {
                E::Binary { op, .. } => {
                    use ast::BinaryOp as B;
                    matches!(op, B::Add | B::Sub | B::Mul | B::Div | B::Rem | B::Pow)
                }
                E::Assign { .. }
                | E::Cond { .. }
                | E::Arrow { .. }
                | E::InstanceOf { .. }
                | E::Cast { .. } => false,
                _ => true,
            };
            self.found = Some(tight);
        }
    }
    let mut find = Find { span, found: None };
    syntax_walk::walk_module(&analysis.module().ast, &mut find);
    find.found.unwrap_or(false)
}
