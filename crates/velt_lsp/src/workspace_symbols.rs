//! Workspace symbols: top-level declarations (and the members of types) whose name fuzzy-matches the
//! query, from every file of the analyzed programs (open documents and what they import, without
//! the standard library) and from the source files under the workspace folders (indexed once and
//! kept up to date by [`crate::disk_index`]): `.vlt` files anywhere, `.ts` and `.tsx` files in a
//! package's `src/` and `tests/` (open documents are searched whatever their folder).

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
/// At most this many files of the workspace folders are indexed.
const MAX_DISK_FILES: usize = 2000;
/// Directories never searched for sources.
pub const SKIPPED_DIRS: &[&str] = &["target", "node_modules"];

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
        if !is_manifest(file.path) && !self.seen.contains(file.path) {
            self.add_symbols(file.path, &file_symbols(file));
        }
    }

    /// Add the matching ones of `symbols`, the symbols of the file at `path` (once per path).
    pub fn add_symbols(&mut self, path: &Path, symbols: &[WorkspaceSymbol]) {
        if self.is_full() || !self.seen.insert(path.to_path_buf()) {
            return;
        }
        for symbol in symbols {
            if self.is_full() {
                return;
            }
            if fuzzy_match(&self.query, &symbol.name) {
                self.symbols.push(symbol.clone());
            }
        }
    }
}

/// Every symbol of `file`.
pub fn file_symbols(file: &SourceFile) -> Vec<WorkspaceSymbol> {
    let Some(uri) = documents::path_to_uri(file.path) else {
        return vec![];
    };
    let index = LineIndex::new(file.text);
    let mut found = vec![];
    for item in &file.ast.items {
        item_symbols(item, &mut found);
    }
    found
        .into_iter()
        .map(|(name, kind, span, container)| WorkspaceSymbol {
            name,
            kind,
            tags: None,
            container_name: container,
            location: OneOf::Left(Location::new(uri.clone(), index.range(span.lo, span.hi))),
            data: None,
        })
        .collect()
}

/// Every symbol of the source file at `path` on disk (empty if it cannot be read).
pub fn disk_file_symbols(path: &Path) -> Vec<WorkspaceSymbol> {
    let Ok(src) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let mut sm = SourceMap::new();
    let file = sm.add(path, src);
    let text = &sm.get(file).src;
    let parsed = std::panic::catch_unwind(|| {
        if vpm::sources::is_plain_ts(path) {
            velt_syntax::parse_ts_file(file, text).0
        } else {
            velt_syntax::parse_file(file, text).0
        }
    });
    match parsed {
        Ok(ast) => file_symbols(&SourceFile {
            path,
            text,
            ast: &ast,
        }),
        Err(_) => vec![],
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

/// Source files under the workspace folder `dir` (skipping hidden, `target` and `node_modules`
/// directories): `.vlt` files anywhere, `.ts` and `.tsx` files only in a package's `src/` and
/// `tests/`, so the TypeScript frontend of a monorepo is not indexed.
pub fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    collect(dir, in_package_sources(dir), out);
}

/// [`collect_files`] below `dir`; `typescript`: whether `dir` is in a package's `src/` or
/// `tests/`.
fn collect(dir: &Path, typescript: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    let package_root = is_package_root(dir);
    for path in entries {
        if out.len() >= MAX_DISK_FILES {
            return;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name) {
                let sources = if package_root {
                    PACKAGE_SOURCE_DIRS.contains(&name)
                } else {
                    typescript && !is_package_root(&path)
                };
                collect(&path, sources, out);
            }
        } else if vpm::sources::is_source_file(&path)
            && !is_manifest(&path)
            && (typescript || !is_typescript(&path))
        {
            out.push(path);
        }
    }
}

/// The directories of a package whose `.ts` and `.tsx` files the index takes.
const PACKAGE_SOURCE_DIRS: [&str; 2] = ["src", "tests"];

/// Whether the workspace index takes the source file `path` (a `.vlt` file, or a `.ts` or
/// `.tsx` file in a package's `src/` or `tests/`), as [`collect_files`] decides.
pub fn indexes(path: &Path) -> bool {
    !is_typescript(path) || path.parent().is_some_and(in_package_sources)
}

/// Whether `dir` is in (or is) the `src/` or `tests/` directory of the package it belongs to.
fn in_package_sources(dir: &Path) -> bool {
    let Some(root) = vpm::manifest::find_package_root(dir) else {
        return false;
    };
    let rel = dir.strip_prefix(&root).ok();
    let first = rel.and_then(|r| r.components().next());
    first.is_some_and(|c| PACKAGE_SOURCE_DIRS.iter().any(|d| c.as_os_str() == *d))
}

fn is_package_root(dir: &Path) -> bool {
    dir.join(vpm::manifest::MANIFEST_FILE).is_file()
}

fn is_typescript(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "ts" || e == "tsx")
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
    fn typescript_files_are_indexed_in_package_sources_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for f in [
            "package.vlt",
            "src/a.ts",
            "src/ui/b.tsx",
            "tests/c.test.ts",
            "script.vlt",
            "web/src/d.ts",
            "web/e.vlt",
            "src/nested/package.vlt",
            "src/nested/f.ts",
            "src/nested/src/g.ts",
        ] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        let mut found = vec![];
        collect_files(root, &mut found);
        let found: Vec<String> = found
            .iter()
            .map(|p| vpm::relpath::relative(p, root))
            .collect();
        let expected = [
            "script.vlt",
            "src/a.ts",
            "src/nested/src/g.ts",
            "src/ui/b.tsx",
            "tests/c.test.ts",
            "web/e.vlt",
        ];
        assert_eq!(found, expected);
        for f in expected {
            assert!(indexes(&root.join(f)), "{f}");
        }
        for f in ["web/src/d.ts", "src/nested/f.ts", "x.ts"] {
            assert!(!indexes(&root.join(f)), "{f}");
        }
        // A workspace folder opened inside a package's sources.
        let mut found = vec![];
        collect_files(&root.join("src/ui"), &mut found);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn fuzzy_matching_is_ordered_and_case_insensitive() {
        assert!(fuzzy_match("usr", "UserService"));
        assert!(fuzzy_match("", "anything"));
        assert!(!fuzzy_match("rsu", "UserService"));
    }
}
