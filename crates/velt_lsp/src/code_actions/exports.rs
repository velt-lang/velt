//! Fix for `export default` (Velt has named exports only), from the parser's note: drop `default`
//! before a declaration (`export default function f` → `export function f`), or turn
//! `export default f;` into the export list `export { f };`.

use velt_common::{Diagnostic, Span};

use crate::analysis::Analysis;

/// Message of the parser's `export default` diagnostic.
const EXPORT_DEFAULT: &str = "`export default` is not supported: Velt has named exports only";

/// `(title, edits)` of the fix for `d` (primary label `span`, the `default` keyword).
pub fn for_diagnostic(
    analysis: &Analysis,
    d: &Diagnostic,
    span: Span,
) -> Option<(String, Vec<(Span, String)>)> {
    if d.message != EXPORT_DEFAULT || analysis.snippet(span) != "default" {
        return None;
    }
    let note = d
        .notes
        .first()?
        .strip_prefix("use a named export: `export ")?;
    let rest = &analysis.text()[span.hi as usize..];
    let ws = (rest.len() - rest.trim_start().len()) as u32;
    if let Some(list) = note.strip_prefix("{ ") {
        let name = list.strip_suffix(" };`")?;
        let end = span.hi + ws + name.len() as u32;
        let edit = (Span::new(span.file, span.lo, end), format!("{{ {name} }}"));
        return Some((format!("Export `{name}` by name"), vec![edit]));
    }
    if note.starts_with("const ") {
        return None;
    }
    let edit = (Span::new(span.file, span.lo, span.hi + ws), String::new());
    Some(("Remove `default` (use a named export)".into(), vec![edit]))
}
