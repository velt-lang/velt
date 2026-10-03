//! Code actions: quick fixes for compiler diagnostics and for a few patterns TypeScript habits
//! produce.
//!
//! Compiler diagnostics carry no machine-readable fix-its yet (`velt_common::Diagnostic` has only
//! a message, labels and notes), so each fix recognizes its diagnostic by message/notes and builds
//! the replacement from the AST and sema's types:
//! - [`fixits`]: `mut` removal, `undefined` → `null`, a non-`bool` condition → an explicit comparison,
//!   `async` for a method whose promise must carry its errors;
//! - [`concat`]: `"a" + n` → a template literal;
//! - [`exports`]: `export default` → a named export;
//! - [`promise`]: a floating promise (a promise-typed expression statement, a compiler error) →
//!   `await` it or `spawn` it.
//!
//! A fix that applies to several places of the document also comes as "fix all in file"
//! ([`fix_all_like`]), and every preferred fix of the document as one `source.fixAll` action
//! ([`fix_all_preferred`]).

mod concat;
mod exports;
mod fixits;
mod promise;

use velt_common::{Diagnostic, Span};

use crate::analysis::Analysis;

/// One quick fix: text edits in the document, and the diagnostic it resolves (if any).
#[derive(Debug)]
pub struct Fix {
    /// Title shown in the editor's light-bulb menu.
    pub title: String,
    /// Replacements of document byte ranges.
    pub edits: Vec<(Span, String)>,
    /// The compiler diagnostic this fixes.
    pub diagnostic: Option<Diagnostic>,
    /// Whether this is the obvious fix (editors apply it on "auto fix").
    pub preferred: bool,
}

/// Quick fixes for the document's byte range `lo..hi`.
pub fn fixes(analysis: &Analysis, lo: u32, hi: u32) -> Vec<Fix> {
    let file = analysis.file();
    let mut out = vec![];
    for d in &analysis.diagnostics {
        let Some(span) = d.labels.first().map(|l| l.span) else {
            continue;
        };
        if span.file != file || span.hi < lo || span.lo > hi {
            continue;
        }
        let fix = fixits::for_diagnostic(analysis, d, span)
            .or_else(|| concat::for_diagnostic(analysis, d, span))
            .or_else(|| exports::for_diagnostic(analysis, d, span));
        out.extend(fix.map(|(title, edits)| Fix {
            title,
            edits,
            diagnostic: Some(d.clone()),
            preferred: true,
        }));
    }
    out.extend(promise::fixes(analysis, lo, hi));
    out
}

/// For each kind of fix among `shown` (by title) that applies more than once in the document:
/// one fix applying all of them.
pub fn fix_all_like(analysis: &Analysis, shown: &[Fix]) -> Vec<Fix> {
    let everywhere = fixes(analysis, 0, analysis.text().len() as u32);
    let mut seen = std::collections::HashSet::new();
    let titles = shown
        .iter()
        .map(|f| f.title.as_str())
        .filter(|t| seen.insert(*t));
    titles
        .filter_map(|title| {
            let same: Vec<&Fix> = everywhere.iter().filter(|f| f.title == title).collect();
            (same.len() > 1).then(|| Fix {
                title: format!("Fix all in file: {title}"),
                edits: merged(&same),
                diagnostic: None,
                preferred: false,
            })
        })
        .collect()
}

/// Every preferred fix of the document as one (`None` if there is none).
pub fn fix_all_preferred(analysis: &Analysis) -> Option<Fix> {
    let everywhere = fixes(analysis, 0, analysis.text().len() as u32);
    let preferred: Vec<&Fix> = everywhere.iter().filter(|f| f.preferred).collect();
    (!preferred.is_empty()).then(|| Fix {
        title: "Fix all auto-fixable problems".into(),
        edits: merged(&preferred),
        diagnostic: None,
        preferred: false,
    })
}

/// The edits of `fixes` together, leaving out a fix whose edits overlap an earlier one's.
fn merged(fixes: &[&Fix]) -> Vec<(Span, String)> {
    let mut out: Vec<(Span, String)> = vec![];
    for fix in fixes {
        let overlaps = fix.edits.iter().any(|(s, _)| {
            out.iter()
                .any(|(o, _)| (s.lo < o.hi && o.lo < s.hi) || (s.lo == o.lo && s.hi == o.hi))
        });
        if !overlaps {
            out.extend(fix.edits.iter().cloned());
        }
    }
    out.sort_by_key(|(s, _)| (s.lo, s.hi));
    out
}

/// The first non-whitespace byte of `text` before `offset`.
fn char_before(text: &str, offset: u32) -> Option<u8> {
    text.as_bytes()[..(offset as usize).min(text.len())]
        .iter()
        .rev()
        .copied()
        .find(|b| !b.is_ascii_whitespace())
}

#[cfg(test)]
mod tests {
    use velt_common::{FileId, Span};

    use super::{merged, Fix};

    fn fix(edits: &[(u32, u32, &str)]) -> Fix {
        Fix {
            title: "t".into(),
            edits: edits
                .iter()
                .map(|&(lo, hi, text)| (Span::new(FileId(0), lo, hi), text.to_string()))
                .collect(),
            diagnostic: None,
            preferred: true,
        }
    }

    fn spans(edits: &[(Span, String)]) -> Vec<(u32, u32)> {
        edits.iter().map(|(s, _)| (s.lo, s.hi)).collect()
    }

    #[test]
    fn merged_fixes_skip_overlaps_and_sort() {
        let (a, b, c) = (
            fix(&[(10, 15, "x")]),
            fix(&[(12, 20, "y")]),
            fix(&[(0, 3, "z")]),
        );
        // `b` overlaps `a`, which came first: left out whole.
        assert_eq!(spans(&merged(&[&a, &b, &c])), [(0, 3), (10, 15)]);
        // Touching ranges do not overlap; two insertions at one point do.
        let (d, e, f) = (
            fix(&[(15, 18, "w")]),
            fix(&[(5, 5, "i")]),
            fix(&[(5, 5, "j")]),
        );
        assert_eq!(
            spans(&merged(&[&a, &d, &e, &f])),
            [(5, 5), (10, 15), (15, 18)]
        );
        // The same span twice: the second left out.
        let same = fix(&[(10, 15, "again")]);
        assert_eq!(spans(&merged(&[&a, &same])), [(10, 15)]);
        // An insertion inside another fix's range is left out; one at its start is kept.
        let (inside, start) = (fix(&[(12, 12, "i")]), fix(&[(10, 10, "s")]));
        assert_eq!(spans(&merged(&[&a, &inside])), [(10, 15)]);
        assert_eq!(spans(&merged(&[&a, &start])), [(10, 10), (10, 15)]);
        // A fix whose second edit overlaps is left out with all its edits.
        let g = fix(&[(30, 31, "p"), (11, 12, "q")]);
        assert_eq!(spans(&merged(&[&a, &g])), [(10, 15)]);
    }
}
