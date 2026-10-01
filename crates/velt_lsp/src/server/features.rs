//! Handlers that need more than one analysis lookup: code actions (fixes → workspace edits tied to
//! the client's diagnostics) and workspace symbols (every analyzed program plus the workspace
//! folders on disk).

use std::collections::HashMap;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, TextEdit, WorkspaceEdit,
    WorkspaceSymbol,
};

use super::Server;
use crate::code_actions;
use crate::line_index::LineIndex;
use crate::workspace_symbols::{Search, SourceFile};

impl Server<'_> {
    pub(super) fn code_actions(
        &mut self,
        p: &CodeActionParams,
    ) -> Option<Vec<CodeActionOrCommand>> {
        let uri = &p.text_document.uri;
        let analysis = self.analysis(uri)?;
        let index = LineIndex::new(analysis.text());
        let lo = index.offset(p.range.start);
        let hi = index.offset(p.range.end);
        let actions = code_actions::fixes(analysis, lo, hi)
            .into_iter()
            .map(|fix| {
                let edits = fix
                    .edits
                    .iter()
                    .map(|(span, text)| TextEdit::new(index.range(span.lo, span.hi), text.clone()))
                    .collect();
                let diagnostics = fix.diagnostic.as_ref().map(|d| {
                    let range = d.labels.first().map(|l| index.range(l.span.lo, l.span.hi));
                    p.context
                        .diagnostics
                        .iter()
                        .filter(|c| Some(c.range) == range && c.message.starts_with(&d.message))
                        .cloned()
                        .collect()
                });
                CodeActionOrCommand::CodeAction(CodeAction {
                    title: fix.title,
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics,
                    edit: Some(WorkspaceEdit::new(HashMap::from([(uri.clone(), edits)]))),
                    is_preferred: fix.preferred.then_some(true),
                    ..Default::default()
                })
            })
            .collect();
        Some(actions)
    }

    pub(super) fn workspace_symbols(&mut self, query: &str) -> Vec<WorkspaceSymbol> {
        let mut search = Search::new(query);
        for uri in self.docs.uris() {
            let Some(analysis) = self.analysis(&uri) else {
                continue;
            };
            for (i, module) in analysis.modules.iter().enumerate() {
                if analysis.is_std(i) {
                    continue;
                }
                let file = analysis.sm.get(module.file);
                search.add(&SourceFile {
                    path: &file.path,
                    text: &file.src,
                    ast: &module.ast,
                });
            }
        }
        search.add_disk_files(&self.roots);
        search.symbols
    }
}
