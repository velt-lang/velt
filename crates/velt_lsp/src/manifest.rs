//! `package.vlt`, the package manifest, is not analyzed as a program: `velt` reads it as data and
//! never compiles it (docs/internals/design/package-manifest.md "Editors"). Its diagnostics are
//! the reader's own (`vpm::manifest::read`, what every `velt` command reports), and completion and
//! hover come from the manifest's field schema (`vpm::manifest::ide`).

use std::path::Path;

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionTextEdit, Documentation, Hover, HoverContents,
    InsertTextFormat, MarkupContent, MarkupKind, TextEdit,
};
use velt_common::{FileId, Severity, Span};
use vpm::manifest::ide;

use crate::line_index::LineIndex;

/// Whether `path` is a package manifest.
pub fn is_manifest(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n == vpm::manifest::MANIFEST_FILE)
}

/// The reader's diagnostics for manifest `text`.
pub fn diagnostics(text: &str) -> Vec<lsp_types::Diagnostic> {
    let Err(diags) = vpm::Manifest::read(FileId(0), text) else {
        return vec![];
    };
    let index = LineIndex::new(text);
    diags
        .iter()
        .map(|d| {
            let span = d.labels.first().map_or(Span::DUMMY, |l| l.span);
            let mut message = d.message.clone();
            for note in &d.notes {
                message.push_str("\nnote: ");
                message.push_str(note);
            }
            lsp_types::Diagnostic {
                range: index.range(span.lo, span.hi),
                severity: Some(match d.severity {
                    Severity::Error => lsp_types::DiagnosticSeverity::ERROR,
                    Severity::Warning => lsp_types::DiagnosticSeverity::WARNING,
                    Severity::Note => lsp_types::DiagnosticSeverity::INFORMATION,
                }),
                source: Some("velt".into()),
                message,
                ..Default::default()
            }
        })
        .collect()
}

/// Completions at byte `offset` of manifest `text`.
pub fn completion(text: &str, offset: u32) -> Vec<CompletionItem> {
    let index = LineIndex::new(text);
    ide::completions(text, offset)
        .into_iter()
        .map(|c| CompletionItem {
            label: c.label,
            kind: Some(match c.kind {
                ide::CompletionKind::Field => CompletionItemKind::FIELD,
                ide::CompletionKind::Value => CompletionItemKind::VALUE,
            }),
            detail: Some(c.detail),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: c.doc,
            })),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
                index.range(c.replace.start, c.replace.end),
                c.text,
            ))),
            insert_text_format: Some(if c.snippet {
                InsertTextFormat::SNIPPET
            } else {
                InsertTextFormat::PLAIN_TEXT
            }),
            ..Default::default()
        })
        .collect()
}

/// Hover at byte `offset` of manifest `text`.
pub fn hover(text: &str, offset: u32) -> Option<Hover> {
    let h = ide::hover(text, offset)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: h.markdown,
        }),
        range: Some(LineIndex::new(text).range(h.range.start, h.range.end)),
    })
}
