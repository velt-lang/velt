//! Globals loaded on demand (`std/prelude/global/*.vlt`): names that are in scope everywhere
//! without an import, like the prelude's, but whose modules a program loads only when it
//! mentions one of them. Node's web globals (`fetch`, `Response`, `URL`, `AbortController`, …)
//! sit on top of large parts of std (the HTTP client, the URL parser, cancellation), which a
//! program that never names them should not pay for in compile time.
//!
//! A global module consists only of re-exports (`export { fetch, Response } from "velt:fetch";`);
//! the names it re-exports are its triggers. A non-std module whose source contains one of them
//! as a whole word (in code, a comment or a string: a false positive only costs loading time)
//! loads the global module, which then becomes part of the prelude (its canonical path starts
//! with `std/prelude/`), unless the module binds that name itself at the top level (an import,
//! as `import { Response } from "./api"` does, or a declaration), which hides the global.
//! std modules import what they use, so they never trigger one.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use velt_common::SourceMap;
use velt_syntax::ast;

/// A global module: its file and the names that load it.
pub(super) struct Global {
    pub file: PathBuf,
    pub names: Vec<String>,
}

/// The global modules under `<std>/prelude/global/`, sorted by file name (empty if none).
pub(super) fn global_modules(std_root: &Path) -> Vec<Global> {
    let Ok(entries) = std::fs::read_dir(std_root.join("prelude").join("global")) else {
        return vec![];
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "vlt"))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|file| {
            let src = std::fs::read_to_string(&file).ok()?;
            let names = reexported_names(&src);
            Some(Global { file, names })
        })
        .collect()
}

/// The names a global module's `export { … } from "…"` items re-export (under their aliases).
fn reexported_names(src: &str) -> Vec<String> {
    let mut sm = SourceMap::new();
    let file = sm.add(Path::new("global.vlt"), src.to_string());
    let (module, _) = velt_syntax::parse_file(file, &sm.get(file).src);
    let mut names = vec![];
    for item in &module.items {
        if let ast::ItemKind::Import(imp) = &item.kind {
            if item.exported {
                for n in &imp.names {
                    names.push(n.alias.as_ref().unwrap_or(&n.name).name.clone());
                }
            }
        }
    }
    names
}

/// The names `module` binds at its top level: what it imports and what it declares.
pub(super) fn bound_names(module: &ast::Module) -> HashSet<&str> {
    let mut names = HashSet::new();
    for item in &module.items {
        match &item.kind {
            ast::ItemKind::Import(imp) => {
                if !item.exported || imp.from.is_empty() {
                    names.extend(
                        imp.names
                            .iter()
                            .map(|n| n.alias.as_ref().unwrap_or(&n.name).name.as_str()),
                    );
                }
                names.extend(imp.namespace.iter().map(|n| n.name.as_str()));
            }
            ast::ItemKind::Function(f) => {
                names.insert(f.sig.name.name.as_str());
            }
            ast::ItemKind::ExternFn(f) => {
                names.insert(f.name.name.as_str());
            }
            ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => {
                names.insert(t.name.name.as_str());
            }
            ast::ItemKind::Interface(i) => {
                names.insert(i.name.name.as_str());
            }
            ast::ItemKind::Enum(e) => {
                names.insert(e.name.name.as_str());
            }
            ast::ItemKind::TypeAlias(a) => {
                names.insert(a.name.name.as_str());
            }
            ast::ItemKind::Var(v) => {
                if let ast::PatternKind::Ident(id) = &v.pattern.kind {
                    names.insert(id.name.as_str());
                }
            }
            ast::ItemKind::Extend(_) => {}
        }
    }
    names
}

/// Whether `src` contains `name` as a whole identifier (not as part of a longer one; identifier
/// characters are ASCII letters, digits, `_` and `$`, as the lexer reads them).
pub(super) fn mentions(src: &str, name: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    src.match_indices(name).any(|(at, _)| {
        let before = src[..at].chars().next_back();
        let after = src[at + name.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_words_only() {
        assert!(mentions("const r = await fetch(url);", "fetch"));
        assert!(mentions("fetch", "fetch"));
        assert!(!mentions(
            "prefetch(url); fetched; $fetch; fetch_all",
            "fetch"
        ));
        assert!(mentions("prefetch(); fetch()", "fetch"));
        // Identifiers are ASCII (as the lexer reads them): any other character ends one.
        assert!(mentions("Responseé", "Response"));
        assert!(!mentions("Response_x", "Response"));
    }

    #[test]
    fn names_are_the_reexports() {
        let src = "// globals\nexport { fetch, Response as R } from \"velt:fetch\";\n\
                   export { URL } from \"velt:url\";\n";
        assert_eq!(reexported_names(src), ["fetch", "R", "URL"]);
    }

    #[test]
    fn lists_global_modules_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(global_modules(tmp.path()).is_empty());
        let dir = tmp.path().join("prelude/global");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.vlt"), "export { B } from \"velt:b\";\n").unwrap();
        std::fs::write(dir.join("a.vlt"), "export { A } from \"velt:a\";\n").unwrap();
        let g = global_modules(tmp.path());
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].names, ["A"]);
        assert_eq!(g[1].file, dir.join("b.vlt"));
    }
}
