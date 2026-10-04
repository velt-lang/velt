//! The completion request: import help first (inside `import { … }` and module specifiers), then
//! the names and members at the cursor, extended by auto-import where a name is being typed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lsp_types::{CompletionList, CompletionResponse, TextDocumentPositionParams, Url};

use super::Server;
use crate::analysis::Analysis;
use crate::completion;
use crate::documents::Documents;
use crate::imports::exports::{self, Export};
use crate::line_index::LineIndex;

/// Words after which the word being typed declares a name (no auto-import there).
const DECLARING: &[&str] = &[
    "function",
    "const",
    "let",
    "var",
    "class",
    "struct",
    "interface",
    "type",
    "enum",
    "as",
];

impl Server<'_> {
    /// Completion at `pos`; `trigger` is the character that triggered it, if any.
    pub(super) fn completion(
        &mut self,
        pos: &TextDocumentPositionParams,
        trigger: Option<&str>,
    ) -> Option<CompletionResponse> {
        let uri = &pos.text_document.uri;
        self.analysis(uri)?;
        let analysis = self.analyses.get(uri)?;
        let doc = self.docs.get(uri)?.path.clone();
        let text = analysis.text();
        let offset = (LineIndex::new(text).offset(pos.position) as usize).min(text.len());
        let word_start = completion::word_start(text, offset);
        let help = self
            .imports
            .complete(analysis, self.loader, &doc, offset, word_start);
        if let Some(items) = help {
            return Some(CompletionResponse::Array(items));
        }
        if matches!(trigger, Some("\"" | "/")) {
            return Some(CompletionResponse::Array(vec![]));
        }
        let mut found = completion::complete(analysis, offset as u32, trigger == Some("<"));
        let prefix = found
            .name_start
            .map(|start| &text[start..offset])
            .filter(|p| !p.is_empty() && !declares(&text[..word_start]));
        let Some(prefix) = prefix else {
            return Some(CompletionResponse::Array(found.items));
        };
        let in_scope: HashSet<String> = found.items.iter().map(|i| i.label.clone()).collect();
        let open = open_exports(&self.analyses, &self.docs, uri, &doc);
        let mut package = self.disk_symbols.exports(&self.roots, &doc);
        package.retain(|(path, _)| !open.iter().any(|(p, _)| p == path));
        package.extend(open.iter().map(|(p, e)| (p.as_path(), e.as_slice())));
        let (auto, capped) =
            self.imports
                .auto_imports(analysis, self.loader, &doc, &package, prefix, &in_scope);
        found.items.extend(auto);
        Some(if capped {
            // Typing more may bring other names: ask again.
            CompletionResponse::List(CompletionList {
                is_incomplete: true,
                items: found.items,
            })
        } else {
            CompletionResponse::Array(found.items)
        })
    }

    /// "Import `x` from …" fixes in the document's byte range `lo..hi`.
    pub(super) fn import_fixes(
        &mut self,
        uri: &lsp_types::Url,
        (lo, hi): (u32, u32),
    ) -> Vec<crate::code_actions::Fix> {
        let (Some(analysis), Some(doc)) = (self.analyses.get(uri), self.docs.get(uri)) else {
            return vec![];
        };
        if !crate::imports::has_unknown_names(analysis, lo, hi) {
            return vec![];
        }
        let doc = doc.path.clone();
        let open = open_exports(&self.analyses, &self.docs, uri, &doc);
        let mut package: Vec<(&Path, &[Export])> = self.disk_symbols.exports(&self.roots, &doc);
        package.retain(|(path, _)| !open.iter().any(|(p, _)| p == path));
        package.extend(open.iter().map(|(p, e)| (p.as_path(), e.as_slice())));
        self.imports
            .fixes(analysis, self.loader, &doc, &package, (lo, hi))
    }
}

/// Whether the text before the word being typed ends with a word that declares it
/// (`function na|`).
fn declares(before: &str) -> bool {
    let trimmed = before.trim_end();
    let word = trimmed
        .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
        .next()
        .unwrap_or("");
    trimmed.len() < before.len() && DECLARING.contains(&word)
}

/// The exports of the other open documents of `doc`'s package (outside a package: of the other
/// open documents outside packages), from their latest analysis: unsaved edits count.
fn open_exports(
    analyses: &HashMap<Url, Analysis>,
    docs: &Documents,
    uri: &Url,
    doc: &Path,
) -> Vec<(PathBuf, Vec<Export>)> {
    let package = doc.parent().and_then(vpm::manifest::find_package_root);
    analyses
        .iter()
        .filter(|(other, _)| *other != uri)
        .filter_map(|(other, analysis)| {
            let path = &docs.get(other)?.path;
            let same = path.parent().and_then(vpm::manifest::find_package_root) == package;
            same.then(|| {
                let exports = crate::index::module_items(analysis, analysis.root, true)
                    .iter()
                    .map(|d| exports::of_decl(analysis, d))
                    .collect();
                (path.clone(), exports)
            })
        })
        .collect()
}
