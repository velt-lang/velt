//! Code actions: quick fixes for compiler diagnostics and for a few patterns TypeScript habits
//! produce.
//!
//! Compiler diagnostics carry no machine-readable fix-its yet (`velt_common::Diagnostic` has only
//! a message, labels and notes), so each fix recognizes its diagnostic by message/notes and builds
//! the replacement from the AST and sema's types:
//! - [`fixits`]: `mut` removal, `undefined` → `null`, a non-`bool` condition → an explicit comparison;
//! - [`concat`]: `"a" + n` → a template literal;
//! - [`exports`]: `export default` → a named export;
//! - [`promise`]: a floating promise (a promise-typed expression statement, a compiler error) →
//!   `await` it or `spawn` it.

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

/// The first non-whitespace byte of `text` before `offset`.
fn char_before(text: &str, offset: u32) -> Option<u8> {
    text.as_bytes()[..(offset as usize).min(text.len())]
        .iter()
        .rev()
        .copied()
        .find(|b| !b.is_ascii_whitespace())
}
