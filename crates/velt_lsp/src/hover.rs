//! Hover: the description of the definition under the cursor (from sema; locals include their
//! inferred type), else the type of the innermost expression, else the AST-based declaration
//! signature (when sema has no answer). A documented definition's doc comment follows its
//! signature, below a rule ([`docs`]).

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};
use velt_common::Span;

use crate::analysis::Analysis;
use crate::index::scope;
use crate::line_index::LineIndex;
use crate::{definition, docs, sema_query, signature};

/// Hover contents for byte `offset` of the document.
pub fn hover(analysis: &Analysis, offset: u32) -> Option<Hover> {
    let word = sema_query::word_at(analysis.text(), offset);
    let word_span = word.map(|(lo, hi)| Span::new(analysis.file(), lo, hi));
    let (span, text, doc) = if let Some(def) = sema_query::def_at(analysis, offset) {
        let doc = docs::markdown_for(analysis, &def);
        (word_span.unwrap_or(def.span), def.detail, doc)
    } else if let Some(ty) = sema_query::type_at(analysis, offset) {
        let span = word_span.unwrap_or(Span::new(analysis.file(), offset, offset));
        (span, ty, None)
    } else {
        let info = scope::at_offset(analysis, offset);
        let decl = definition::resolve(analysis, &info)?;
        let span = info.reference.as_ref().map_or(decl.name_span, |r| r.span());
        let doc = docs::doc_at(analysis, decl.name_span).map(|d| d.render_markdown());
        (span, signature::decl(analysis, &decl), doc)
    };
    let range = LineIndex::new(analysis.text()).range(span.lo, span.hi);
    let mut value = fenced(&text);
    if let Some(doc) = doc.filter(|d| !d.is_empty()) {
        value.push_str("\n\n---\n\n");
        value.push_str(&doc);
    }
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(range),
    })
}

/// `text` in a `velt` code fence longer than any run of backticks in it (a string literal type
/// may contain three), so the text cannot end the fence.
fn fenced(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}velt\n{text}\n{fence}")
}

#[cfg(test)]
mod fence_tests {
    use super::fenced;

    #[test]
    fn the_fence_outlasts_backticks_in_the_text() {
        assert_eq!(fenced("const x: i64"), "```velt\nconst x: i64\n```");
        assert_eq!(
            fenced("type T = \"```\""),
            "````velt\ntype T = \"```\"\n````"
        );
    }
}
