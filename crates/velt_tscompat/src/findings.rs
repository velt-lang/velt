//! Collecting the findings of one module: the source text the rules quote, and the findings so
//! far. Shared by the syntax rules ([`crate::rules`]) and the typed rules ([`crate::typed`]).

use velt_common::Span;

use crate::{Finding, Fix, Severity};

/// What the rules share: the source text and the findings so far.
pub(crate) struct Cx<'a> {
    pub(crate) src: &'a str,
    pub(crate) findings: Vec<Finding>,
}

impl<'a> Cx<'a> {
    pub(crate) fn new(src: &'a str) -> Cx<'a> {
        Cx {
            src,
            findings: vec![],
        }
    }

    /// The source text of `span` (empty if it is out of range, which a parsed module's spans
    /// never are).
    pub(crate) fn text(&self, span: Span) -> &'a str {
        self.src
            .get(span.lo as usize..span.hi as usize)
            .unwrap_or("")
    }

    /// Report a finding.
    pub(crate) fn report(
        &mut self,
        code: &'static str,
        severity: Severity,
        span: Span,
        message: String,
        notes: &[&str],
        fix: Option<Fix>,
    ) {
        debug_assert!(
            crate::RULES.contains(&code),
            "ICE: `{code}` is not in RULES"
        );
        self.findings.push(Finding {
            code,
            severity,
            span,
            message,
            notes: notes.iter().map(|n| n.to_string()).collect(),
            fix,
        });
    }

    /// Report an error: `tsc` rejects the code or JavaScript runs it differently.
    pub(crate) fn error(
        &mut self,
        code: &'static str,
        span: Span,
        message: String,
        notes: &[&str],
    ) {
        self.report(code, Severity::Error, span, message, notes, None);
    }

    /// [`Cx::error`] with a fix.
    pub(crate) fn error_with_fix(
        &mut self,
        code: &'static str,
        span: Span,
        message: String,
        notes: &[&str],
        fix: Fix,
    ) {
        self.report(code, Severity::Error, span, message, notes, Some(fix));
    }

    /// The span of keyword `word` inside `within` (the first whole-word occurrence; `within`
    /// itself when the source doesn't have it there).
    pub(crate) fn keyword(&self, within: Span, word: &str) -> Span {
        let text = self.text(within);
        let mut from = 0;
        while let Some(at) = text[from..].find(word).map(|i| i + from) {
            let before = text[..at].chars().next_back();
            let after = text[at + word.len()..].chars().next();
            let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            if !is_word(before) && !is_word(after) {
                let lo = within.lo + at as u32;
                return Span::new(within.file, lo, lo + word.len() as u32);
            }
            from = at + word.len();
        }
        within
    }
}
