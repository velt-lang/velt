//! Hover: the description of the definition under the cursor (from sema; locals include their
//! inferred type), else the type of the innermost expression, else the AST-based declaration
//! signature (when sema has no answer).

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};
use velt_common::Span;

use crate::analysis::Analysis;
use crate::index::scope;
use crate::line_index::LineIndex;
use crate::{definition, sema_query, signature};

/// Hover contents for byte `offset` of the document.
pub fn hover(analysis: &Analysis, offset: u32) -> Option<Hover> {
    let word = sema_query::word_at(analysis.text(), offset);
    let word_span = word.map(|(lo, hi)| Span::new(analysis.file(), lo, hi));
    let (span, text) = if let Some(def) = sema_query::def_at(analysis, offset) {
        (word_span.unwrap_or(def.span), def.detail)
    } else if let Some(ty) = sema_query::type_at(analysis, offset) {
        (
            word_span.unwrap_or(Span::new(analysis.file(), offset, offset)),
            ty,
        )
    } else {
        let info = scope::at_offset(analysis, offset);
        let decl = definition::resolve(analysis, &info)?;
        let span = info.reference.as_ref().map_or(decl.name_span, |r| r.span());
        (span, signature::decl(analysis, &decl))
    };
    let range = LineIndex::new(analysis.text()).range(span.lo, span.hi);
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!("```velt\n{text}\n```"),
        }),
        range: Some(range),
    })
}
