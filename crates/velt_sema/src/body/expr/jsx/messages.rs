//! JSX diagnostics worded like TypeScript's, so messages match what TSX users know.

use velt_common::{Diagnostic, Span};

use crate::body::FnCx;
use crate::hir::{self, TyId};

/// The longest edit distance a "Did you mean" suggestion may have.
const MAX_SUGGESTION_DISTANCE: usize = 2;

impl FnCx<'_, '_> {
    /// `Property 'x' does not exist on type 'T'.`, suggesting the closest of `known`.
    pub(super) fn no_property(
        &mut self,
        name: &str,
        on: &str,
        known: &[String],
        span: Span,
        note: Option<String>,
    ) {
        let mut msg = format!("Property '{name}' does not exist on type '{on}'.");
        if let Some(s) = closest(name, known) {
            msg.push_str(&format!(" Did you mean '{s}'?"));
        }
        let d = Diagnostic::error(msg, span);
        self.cx.error(match note {
            Some(n) => d.with_note(n),
            None => d,
        });
    }

    /// `Type 'F' is not assignable to type 'E'.` for `found` checked against `expected`.
    pub(super) fn not_assignable(&mut self, expected: TyId, found: &hir::Expr, what: &str) {
        let (e, f) = (self.cx.display(expected), self.cx.display(found.ty));
        self.cx.error(
            Diagnostic::error(
                format!("Type '{f}' is not assignable to type '{e}'."),
                found.span,
            )
            .with_note(what.to_string()),
        );
    }

    /// Convert `h` to `expected`, reporting a mismatch the TypeScript way (`what` says where);
    /// a mismatched value becomes an error placeholder.
    pub(super) fn jsx_coerce(&mut self, h: hir::Expr, expected: TyId, what: &str) -> hir::Expr {
        match self.try_coerce(h, expected) {
            Ok(h) => h,
            Err(h) => {
                self.not_assignable(expected, &h, what);
                // Reported: the value must not cause a second mismatch where it is used.
                self.error_expr(h.span)
            }
        }
    }
}

/// The name in `known` closest to `name` (case-insensitive equal, or within a small edit
/// distance).
pub(super) fn closest<'k>(name: &str, known: &'k [String]) -> Option<&'k str> {
    known
        .iter()
        .map(|k| (distance(&name.to_lowercase(), &k.to_lowercase()), k))
        .filter(|(d, _)| *d <= MAX_SUGGESTION_DISTANCE)
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k.as_str())
}

/// Levenshtein distance over chars.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_close_names_only() {
        let known = vec!["class".to_string(), "href".to_string(), "id".to_string()];
        assert_eq!(closest("clas", &known), Some("class"));
        assert_eq!(closest("Class", &known), Some("class"));
        assert_eq!(closest("hreff", &known), Some("href"));
        assert_eq!(closest("onClick", &known), None);
    }

    #[test]
    fn edit_distance() {
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("same", "same"), 0);
    }
}
