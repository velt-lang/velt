//! Fixes recognized by a diagnostic's message and notes alone: `mut` removal, `undefined` (and
//! `void 0`) → `null`, and `async` for a method whose promise must carry its errors.

use velt_common::{Diagnostic, Span};

use crate::analysis::Analysis;

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
    None
}

/// `mut` plus the whitespace after it.
fn remove_mut(analysis: &Analysis, span: Span) -> Span {
    let rest = &analysis.text()[span.hi as usize..];
    let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    Span::new(span.file, span.lo, span.hi + ws as u32)
}
