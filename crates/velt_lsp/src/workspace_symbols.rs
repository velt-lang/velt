//! Workspace symbols: top-level declarations (and the members of types) whose name fuzzy-matches the
//! query, from every file of the analyzed programs (open documents and what they import, without
//! the standard library) and from the `.vlt` files under the workspace folders (parsed on demand).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use lsp_types::{Location, OneOf, SymbolKind, Url, WorkspaceSymbol};
use velt_common::SourceMap;
use velt_syntax::ast;

use crate::documents;
use crate::index::pattern_idents;
use crate::line_index::LineIndex;
// A package manifest is data whose only symbol is the `pkg` every package has: not searched.
use crate::manifest::is_manifest;

/// At most this many symbols are returned (clients re-query as the user types).
const MAX_RESULTS: usize = 256;
/// At most this many files are read from the workspace folders per query.
const MAX_DISK_FILES: usize = 2000;
/// Directories never searched for sources.
const SKIPPED_DIRS: &[&str] = &["target", "node_modules"];

/// One searchable file.
pub struct SourceFile<'a> {
    pub path: &'a Path,
    pub text: &'a str,
    pub ast: &'a ast::Module,
}

/// Collects matching symbols across files.
pub struct Search {
    query: String,
    seen: HashSet<PathBuf>,
    /// The matches so far.
    pub symbols: Vec<WorkspaceSymbol>,
}

impl Search {
    /// A search for `query` (case-insensitive, characters in order; empty matches everything).
    pub fn new(query: &str) -> Search {
        Search {
            query: query.to_lowercase(),
            seen: HashSet::new(),
            symbols: vec![],
        }
    }

    /// Whether the search has all the results it will return.
    pub fn is_full(&self) -> bool {
        self.symbols.len() >= MAX_RESULTS
    }

    /// Add the symbols of `file` (once per path).
    pub fn add(&mut self, file: &SourceFile) {
        if self.is_full() || is_manifest(file.path) || !self.seen.insert(file.path.to_path_buf()) {
            return;
        }
        let Some(uri) = documents::path_to_uri(file.path) else {
            return;
        };
        let index = LineIndex::new(file.text);
        let mut found = vec![];
        for item in &file.ast.items {
            item_symbols(item, &mut found);
        }
        for (name, kind, span, container) in found {
            if self.is_full() || !fuzzy_match(&self.query, &name) {
                continue;
            }
            let range = index.range(span.lo, span.hi);
            self.symbols.push(WorkspaceSymbol {
                name,
                kind,
                tags: None,
                container_name: container,
                location: OneOf::Left(Location::new(uri.clone(), range)),
                data: None,
            });
        }
    }

    /// Add the `.vlt` files under `roots` that were not added yet.
    pub fn add_disk_files(&mut self, roots: &[PathBuf]) {
        let mut files = vec![];
        for root in roots {
            collect_files(root, &mut files);
        }
        for path in files.into_iter().take(MAX_DISK_FILES) {
            if self.is_full() {
                return;
            }
            if self.seen.contains(&path) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut sm = SourceMap::new();
            let file = sm.add(&path, src);
            let text = &sm.get(file).src;
            let parsed = std::panic::catch_unwind(|| velt_syntax::parse_file(file, text).0);
            if let Ok(ast) = parsed {
                self.add(&SourceFile {
                    path: &path,
                    text,
                    ast: &ast,
                });
            }
        }
    }
}

/// `(name, kind, span of the name, container)` of an item and its members.
type Found = Vec<(String, SymbolKind, velt_common::Span, Option<String>)>;

fn item_symbols(item: &ast::Item, out: &mut Found) {
    let mut top = |name: &ast::Ident, kind| out.push((name.name.clone(), kind, name.span, None));
    match &item.kind {
        ast::ItemKind::Function(f) => top(&f.sig.name, SymbolKind::FUNCTION),
        ast::ItemKind::ExternFn(sig) => top(&sig.name, SymbolKind::FUNCTION),
        ast::ItemKind::Interface(i) => top(&i.name, SymbolKind::INTERFACE),
        ast::ItemKind::TypeAlias(a) => top(&a.name, SymbolKind::TYPE_PARAMETER),
        ast::ItemKind::Var(v) => {
            for name in pattern_idents(&v.pattern) {
                top(name, SymbolKind::VARIABLE);
            }
        }
        ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => {
            let kind = if matches!(item.kind, ast::ItemKind::Class(_)) {
                SymbolKind::CLASS
            } else {
                SymbolKind::STRUCT
            };
            top(&t.name, kind);
            let owner = Some(t.name.name.clone());
            for f in &t.fields {
                out.push((
                    f.name.name.clone(),
                    SymbolKind::FIELD,
                    f.name.span,
                    owner.clone(),
                ));
            }
            for m in &t.methods {
                let name = &m.decl.sig.name;
                out.push((
                    name.name.clone(),
                    SymbolKind::METHOD,
                    name.span,
                    owner.clone(),
                ));
            }
        }
        ast::ItemKind::Enum(e) => {
            top(&e.name, SymbolKind::ENUM);
            for v in &e.variants {
                let owner = Some(e.name.name.clone());
                out.push((
                    v.name.name.clone(),
                    SymbolKind::ENUM_MEMBER,
                    v.name.span,
                    owner,
                ));
            }
        }
        ast::ItemKind::Import(_) | ast::ItemKind::Extend(_) => {}
    }
}

/// Whether the characters of `query` (lowercase) appear in `name` in order, ignoring case.
fn fuzzy_match(query: &str, name: &str) -> bool {
    let mut chars = name.chars().flat_map(char::to_lowercase);
    query.chars().all(|q| chars.any(|c| c == q))
}

/// `.vlt` files under `dir` (skipping hidden, `target` and `node_modules` directories).
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if out.len() >= MAX_DISK_FILES {
            return;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name) {
                collect_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "vlt") && !is_manifest(&path) {
            out.push(path);
        }
    }
}

/// Workspace folders from the `initialize` params (`workspaceFolders`, else `rootUri`).
pub fn roots_from_init(params: &serde_json::Value) -> Vec<PathBuf> {
    let folders = params["workspaceFolders"].as_array();
    let uris: Vec<&str> = match folders {
        Some(folders) => folders.iter().filter_map(|f| f["uri"].as_str()).collect(),
        None => params["rootUri"].as_str().into_iter().collect(),
    };
    uris.into_iter()
        .filter_map(|u| Url::parse(u).ok())
        .filter(|u| u.scheme() == "file")
        .map(|u| documents::uri_to_path(&u))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_manifests_are_not_searched() {
        let text = "export const pkg: Package = { name: \"a\", version: \"1.0.0\" };\nexport function pkgHelper() {}\n";
        let (ast, _) = velt_syntax::parse_file(velt_common::FileId(0), text);
        let mut search = Search::new("pkg");
        let dir = std::env::temp_dir();
        for path in [dir.join("a/package.vlt"), dir.join("a/src/pkg.vlt")] {
            search.add(&SourceFile {
                path: &path,
                text,
                ast: &ast,
            });
        }
        let names: Vec<_> = search.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["pkg", "pkgHelper"]);
        assert!(search.symbols.iter().all(|s| match &s.location {
            OneOf::Left(l) => l.uri.path().ends_with("/src/pkg.vlt"),
            OneOf::Right(_) => false,
        }));
    }

    #[test]
    fn fuzzy_matching_is_ordered_and_case_insensitive() {
        assert!(fuzzy_match("usr", "UserService"));
        assert!(fuzzy_match("", "anything"));
        assert!(!fuzzy_match("rsu", "UserService"));
    }
}
