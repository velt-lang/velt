//! Document highlight: every occurrence in the document of the definition under the cursor (from
//! sema's reference table), marked as a write where it is declared or assigned (`x = ...`, `x += ...`,
//! `x++`, `this.x = ...`) and as a read elsewhere.

use std::collections::HashSet;

use lsp_types::{DocumentHighlight, DocumentHighlightKind};
use velt_common::Span;
use velt_syntax::ast::{self, ExprKind as E};

use crate::analysis::Analysis;
use crate::line_index::LineIndex;
use crate::sema_query;
use velt_syntax::visit::{self, Visit};

/// Highlights for the name at byte `offset` of the document.
pub fn highlights(analysis: &Analysis, offset: u32) -> Option<Vec<DocumentHighlight>> {
    let ide = analysis.ide.as_ref()?;
    let def = sema_query::def_at(analysis, offset)?;
    let mut writes = Writes::default();
    visit::walk_module(&analysis.module().ast, &mut writes);
    let index = LineIndex::new(analysis.text());
    let file = analysis.file();
    let highlights = ide
        .references(&def)
        .into_iter()
        .filter(|s| s.file == file)
        .map(|s| {
            let write = s == def.span || writes.0.contains(&s);
            DocumentHighlight {
                range: index.range(s.lo, s.hi),
                kind: Some(if write {
                    DocumentHighlightKind::WRITE
                } else {
                    DocumentHighlightKind::READ
                }),
            }
        })
        .collect();
    Some(highlights)
}

/// Spans of the names assigned to in the document.
#[derive(Default)]
struct Writes(HashSet<Span>);

impl<'a> Visit<'a> for Writes {
    fn expr(&mut self, e: &'a ast::Expr) {
        let target = match &e.kind {
            E::Assign { target, .. } | E::Update { target, .. } => target,
            _ => return,
        };
        match &target.kind {
            E::Ident(i) => self.0.insert(i.span),
            E::Member { prop, .. } => self.0.insert(prop.span),
            _ => false,
        };
    }
}
