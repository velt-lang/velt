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
    /// Does another open document, or a file of the workspace folders, read the default export
    /// of the document `uri`?
    fn default_imported(&mut self, uri: &lsp_types::Url) -> bool {
        use code_actions::exports::{default_specifiers, names_module};
        let target = crate::documents::uri_to_path(uri);
        let open = (self.analyses.iter())
            .filter(|(other, _)| *other != uri)
            .any(|(other, analysis)| {
                let path = crate::documents::uri_to_path(other);
                default_specifiers(&analysis.module().ast)
                    .iter()
                    .any(|spec| names_module(&path, spec, &target))
            });
        open || self.disk_symbols.imports_default_of(&self.roots, &target)
    }

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
        let converts = self
            .analyses
            .get(uri)
            .is_some_and(|a| !code_actions::exports::fixes(a, lo, hi).is_empty());
        let imported = converts && self.default_imported(uri);
        let analysis = self.analyses.get(uri)?;
        let index = LineIndex::new(analysis.text());
        let mut fixes = code_actions::fixes(analysis, lo, hi);
        if imported {
            fixes.retain(|f| f.title != code_actions::exports::TITLE);
        }
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
                actions.extend(action(p, analysis, &index, fix, CodeActionKind::QUICKFIX));
            }
        }
        if wanted(&CodeActionKind::SOURCE_FIX_ALL) {
            if let Some(fix) = code_actions::fix_all_preferred(analysis) {
                actions.extend(action(
                    p,
                    analysis,
                    &index,
                    fix,
                    CodeActionKind::SOURCE_FIX_ALL,
                ));
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

/// `fix` as a code action of `kind`, tied to the client's diagnostic it resolves. Edits in other
/// loaded files (a lint fix that rewrites the uses of a type) go to their files; `None` when one
/// of those has no URI.
fn action(
    p: &CodeActionParams,
    analysis: &crate::analysis::Analysis,
    index: &LineIndex,
    fix: code_actions::Fix,
    kind: CodeActionKind,
) -> Option<CodeActionOrCommand> {
    let document = analysis.file();
    let mut changes: HashMap<lsp_types::Url, Vec<TextEdit>> = HashMap::new();
    for (span, text) in &fix.edits {
        if span.file == document {
            let edit = TextEdit::new(index.range(span.lo, span.hi), text.clone());
            changes
                .entry(p.text_document.uri.clone())
                .or_default()
                .push(edit);
            continue;
        }
        let file = analysis.sm.get(span.file);
        let uri = lsp_types::Url::from_file_path(&file.path).ok()?;
        let range = LineIndex::new(&file.src).range(span.lo, span.hi);
        changes
            .entry(uri)
            .or_default()
            .push(TextEdit::new(range, text.clone()));
    }
    let diagnostics = fix.diagnostic.as_ref().map(|d| {
        let range = d.labels.first().map(|l| index.range(l.span.lo, l.span.hi));
        p.context
            .diagnostics
            .iter()
            .filter(|c| Some(c.range) == range && c.message.starts_with(&d.message))
            .cloned()
            .collect()
    });
    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: fix.title,
        kind: Some(kind),
        diagnostics,
        edit: Some(WorkspaceEdit::new(changes)),
        is_preferred: fix.preferred.then_some(true),
        ..Default::default()
    }))
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
