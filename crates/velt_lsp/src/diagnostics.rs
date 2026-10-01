//! Compiler [`Diagnostic`]s → LSP diagnostics for one document.
//!
//! Only diagnostics whose primary label lies in the document are shown on it (errors in an imported
//! file appear when that file is open). Location-less diagnostics (`Span::DUMMY`, e.g. an unreadable
//! prelude) are shown at the top of the document so they are not lost.

use lsp_types::{DiagnosticRelatedInformation, DiagnosticSeverity, Location, NumberOrString, Url};
use velt_common::{Diagnostic, Severity, Span};

use crate::analysis::Analysis;
use crate::line_index::LineIndex;

/// The LSP diagnostics of `analysis` that belong to its document.
pub fn for_document(
    analysis: &Analysis,
    uri_of: &dyn Fn(&std::path::Path) -> Option<Url>,
) -> Vec<lsp_types::Diagnostic> {
    let file = analysis.file();
    let index = LineIndex::new(analysis.text());
    analysis
        .diagnostics
        .iter()
        .filter_map(|d| {
            let span = d.labels.first().map_or(Span::DUMMY, |l| l.span);
            let range = if span == Span::DUMMY {
                index.range(0, 0)
            } else if span.file == file {
                index.range(span.lo, span.hi)
            } else {
                return None;
            };
            Some(convert(analysis, d, range, uri_of))
        })
        .collect()
}

fn convert(
    analysis: &Analysis,
    d: &Diagnostic,
    range: lsp_types::Range,
    uri_of: &dyn Fn(&std::path::Path) -> Option<Url>,
) -> lsp_types::Diagnostic {
    let mut message = d.message.clone();
    for note in &d.notes {
        message.push_str("\nnote: ");
        message.push_str(note);
    }
    let related: Vec<DiagnosticRelatedInformation> = d
        .labels
        .iter()
        .skip(1)
        .filter(|l| !l.message.is_empty() && l.span != Span::DUMMY)
        .filter_map(|l| {
            let file = analysis.sm.get(l.span.file);
            let uri = uri_of(&file.path)?;
            let range = LineIndex::new(&file.src).range(l.span.lo, l.span.hi);
            Some(DiagnosticRelatedInformation {
                location: Location::new(uri, range),
                message: l.message.clone(),
            })
        })
        .collect();
    lsp_types::Diagnostic {
        range,
        severity: Some(severity(d.severity)),
        code: None::<NumberOrString>,
        code_description: None,
        source: Some("velt".into()),
        message,
        related_information: (!related.is_empty()).then_some(related),
        tags: None,
        data: None,
    }
}

fn severity(s: Severity) -> DiagnosticSeverity {
    match s {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Note => DiagnosticSeverity::INFORMATION,
    }
}
