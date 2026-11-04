//! Applying fixes to source text (`velt check --fix`).

use std::collections::BTreeMap;

use velt_common::{Applicability, Diagnostic, FileId, Fix, Span};

/// Which fixes `--fix` applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    /// Only those that keep the program's behaviour.
    Safe,
    /// Those that may change it too (`--fix=review`).
    Review,
}

impl Which {
    fn takes(self, fix: &Fix) -> bool {
        self == Which::Review || fix.applicability == Applicability::Safe
    }
}

/// Edits per file, sorted by position.
pub type Edits = BTreeMap<FileId, Vec<(Span, String)>>;

/// The fixes `which` picks, one per diagnostic (its first such fix), as edits per file, sorted.
/// A fix whose edits overlap an edit already picked is left out whole, so the picked fixes can
/// all be applied; the second value is the diagnostics whose fix was picked, by index.
pub fn pick(diagnostics: &[Diagnostic], which: Which) -> (Edits, Vec<usize>) {
    let mut edits = Edits::new();
    let mut picked = vec![];
    for (i, d) in diagnostics.iter().enumerate() {
        let Some(fix) = d.fixes.iter().find(|f| which.takes(f)) else {
            continue;
        };
        let overlaps = fix.edits.iter().any(|(s, _)| {
            edits.get(&s.file).is_some_and(|taken| {
                taken
                    .iter()
                    .any(|(t, _)| t.lo < s.hi && s.lo < t.hi || t.lo == s.lo)
            })
        });
        if overlaps {
            continue;
        }
        for (span, text) in &fix.edits {
            edits
                .entry(span.file)
                .or_default()
                .push((*span, text.clone()));
        }
        picked.push(i);
    }
    for list in edits.values_mut() {
        list.sort_by_key(|(s, _)| (s.lo, s.hi));
    }
    (edits, picked)
}

/// `src` with `edits` (sorted, not overlapping, all in this file) applied.
pub fn apply(src: &str, edits: &[(Span, String)]) -> String {
    let mut out = String::with_capacity(src.len());
    let mut at = 0usize;
    for (span, text) in edits {
        let (lo, hi) = (span.lo as usize, span.hi as usize);
        if lo < at || hi > src.len() {
            continue;
        }
        out.push_str(&src[at..lo]);
        out.push_str(text);
        at = hi;
    }
    out.push_str(&src[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(lo: u32, hi: u32, text: &str, applicability: Applicability) -> Fix {
        Fix::replace("t", Span::new(FileId(0), lo, hi), text, applicability)
    }

    #[test]
    fn picks_safe_fixes_without_overlap() {
        let d = |f: Fix| Diagnostic::warning("w", Span::new(FileId(0), 0, 0)).with_fix(f);
        let diags = [
            d(fix(4, 5, "B", Applicability::Safe)),
            d(fix(4, 6, "X", Applicability::Safe)),
            d(fix(0, 1, "A", Applicability::Review)),
            d(fix(9, 10, "C", Applicability::Safe)),
        ];
        let (edits, picked) = pick(&diags, Which::Safe);
        assert_eq!(picked, [0, 3]);
        assert_eq!(apply("a = b; c d", &edits[&FileId(0)]), "a = B; c C");
        let (edits, picked) = pick(&diags, Which::Review);
        assert_eq!(picked, [0, 2, 3]);
        assert_eq!(apply("a = b; c d", &edits[&FileId(0)]), "A = B; c C");
    }
}
