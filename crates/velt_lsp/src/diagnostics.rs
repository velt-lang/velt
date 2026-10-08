//! Compiler [`Diagnostic`]s → LSP diagnostics for one document.
//!
//! Only diagnostics whose primary label lies in the document are shown on it (errors in an imported
//! file appear when that file is open). Location-less diagnostics (`Span::DUMMY`, e.g. an unreadable
//! prelude) are shown at the top of the document so they are not lost. The TypeScript-compatibility
//! findings follow the compiler's ([`crate::ts_compat`]), then a hint tagged
//! [`DiagnosticTag::DEPRECATED`] on each use of a definition documented `@deprecated` (editors
//! strike it through).

use std::collections::HashMap;

use lsp_types::{
    DiagnosticRelatedInformation, DiagnosticSeverity, DiagnosticTag, Location, NumberOrString, Url,
};
use velt_common::{Diagnostic, Severity, Span};

use crate::analysis::Analysis;
use crate::line_index::LineIndex;
use crate::text_scan::{self, TokenKind};

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
        .chain(
            analysis
                .ts_compat
                .iter()
                .map(|f| crate::ts_compat::diagnostic(&index, f)),
        )
        .chain(deprecated_uses(analysis, &index))
        .collect()
}

/// A hint on every name in the document that refers to (not declares) a definition documented
/// `@deprecated`.
fn deprecated_uses(analysis: &Analysis, index: &LineIndex) -> Vec<lsp_types::Diagnostic> {
    let Some(ide) = analysis.ide.as_ref() else {
        return vec![];
    };
    let text = analysis.text();
    let file = analysis.file();
    // Definition (by its name) → its deprecation text, looked up once.
    let mut seen: HashMap<Span, Option<String>> = HashMap::new();
    let mut out = vec![];
    for t in text_scan::scan(text, text.len()) {
        if t.kind != TokenKind::Ident {
            continue;
        }
        let span = Span::new(file, t.lo, t.hi);
        let Some(def) = ide.def_of(span) else {
            continue;
        };
        if def.span == span || def.span == Span::DUMMY {
            continue;
        }
        let deprecated = seen
            .entry(def.span)
            .or_insert_with(|| crate::docs::doc_for(analysis, &def).and_then(|d| d.deprecated));
        let Some(reason) = deprecated else {
            continue;
        };
        let message = match reason.is_empty() {
            true => format!("`{}` is deprecated", def.name),
            false => format!("`{}` is deprecated: {reason}", def.name),
        };
        out.push(lsp_types::Diagnostic {
            range: index.range(t.lo, t.hi),
            severity: Some(DiagnosticSeverity::HINT),
            source: Some("velt".into()),
            message,
            tags: Some(vec![DiagnosticTag::DEPRECATED]),
            ..Default::default()
        });
    }
    out
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
