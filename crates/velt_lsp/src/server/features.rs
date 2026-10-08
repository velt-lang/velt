//! Handlers that need more than one analysis lookup: code actions (fixes, imports of unknown
//! names included → workspace edits tied to the client's diagnostics, "fix all in file" variants
//! and `source.fixAll`) and workspace symbols (every analyzed program plus the index of the
//! workspace folders).

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
        if let Some(text) = self.manifest_text(uri) {
            let index = LineIndex::new(text);
            let range = (index.offset(p.range.start), index.offset(p.range.end));
            let dir = self.docs.get(uri).and_then(|d| d.path.parent());
            return Some(crate::manifest::code_actions(
                uri,
                text,
                range,
                &self.registry,
                dir,
            ));
        }
        let (lo, hi) = {
            let index = LineIndex::new(self.analysis(uri)?.text());
            (index.offset(p.range.start), index.offset(p.range.end))
        };
        let import_fixes = self.import_fixes(uri, (lo, hi));
        let analysis = self.analyses.get(uri)?;
        let index = LineIndex::new(analysis.text());
        let mut fixes = code_actions::fixes(analysis, lo, hi);
        fixes.extend(import_fixes);
        let all_like = code_actions::fix_all_like(analysis, &fixes);
        // Quick fixes unless other kinds are asked for; `source.fixAll` only when asked for
        // (editors request it on save or from a menu, not for the light bulb).
        let asked = |kind: &CodeActionKind| {
            p.context
                .only
                .as_ref()
                .map(|only| only.iter().any(|k| covers(k, kind)))
        };
        let wanted =
            |kind: &CodeActionKind| asked(kind).unwrap_or(*kind == CodeActionKind::QUICKFIX);
        let mut actions = vec![];
        if wanted(&CodeActionKind::QUICKFIX) {
            for fix in fixes.into_iter().chain(all_like) {
                actions.push(action(p, &index, fix, CodeActionKind::QUICKFIX));
            }
        }
        if wanted(&CodeActionKind::SOURCE_FIX_ALL) {
            if let Some(fix) = code_actions::fix_all_preferred(analysis) {
                actions.push(action(p, &index, fix, CodeActionKind::SOURCE_FIX_ALL));
            }
        }
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
        self.disk_symbols.search(&self.roots, &mut search);
        search.symbols
    }
}

/// `fix` as a code action of `kind`, tied to the client's diagnostic it resolves.
fn action(
    p: &CodeActionParams,
    index: &LineIndex,
    fix: code_actions::Fix,
    kind: CodeActionKind,
) -> CodeActionOrCommand {
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
    let uri = p.text_document.uri.clone();
    CodeActionOrCommand::CodeAction(CodeAction {
        title: fix.title,
        kind: Some(kind),
        diagnostics,
        edit: Some(WorkspaceEdit::new(HashMap::from([(uri, edits)]))),
        is_preferred: fix.preferred.then_some(true),
        ..Default::default()
    })
}

/// Does a requested kind `asked` cover `kind`: the same kind or a parent of it, by whole
/// dot-separated segments (`source` covers `source.fixAll`, `source.fix` does not).
fn covers(asked: &CodeActionKind, kind: &CodeActionKind) -> bool {
    let (asked, kind) = (asked.as_str(), kind.as_str());
    kind == asked
        || kind
            .strip_prefix(asked)
            .is_some_and(|rest| rest.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use lsp_types::CodeActionKind;

    use super::covers;

    #[test]
    fn kinds_match_by_whole_segments() {
        let fix_all = CodeActionKind::SOURCE_FIX_ALL;
        assert!(covers(&CodeActionKind::SOURCE, &fix_all));
        assert!(covers(&fix_all, &fix_all));
        assert!(!covers(&CodeActionKind::new("source.fix"), &fix_all));
        assert!(!covers(
            &CodeActionKind::new("quick"),
            &CodeActionKind::QUICKFIX
        ));
    }
}
