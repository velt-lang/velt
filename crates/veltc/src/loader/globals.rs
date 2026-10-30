//! Globals loaded on demand (`std/prelude/global/*.vlt`): names that are in scope everywhere
//! without an import, like the prelude's, but whose modules a program loads only when it
//! mentions one of them. Node's web globals (`fetch`, `Response`, `URL`, `AbortController`, …)
//! sit on top of large parts of std (the HTTP client, the URL parser, cancellation), which a
//! program that never names them should not pay for in compile time.
//!
//! A global module consists only of re-exports (`export { fetch, Response } from "velt/net/http";`);
//! the names it re-exports are its triggers. A non-std module whose source contains one of them
//! as a whole word (in code, a comment or a string: a false positive only costs loading time)
//! loads the global module, which then becomes part of the prelude (its canonical path starts
//! with `std/prelude/`), unless the module binds that name itself at the top level (an import,
//! as `import { Response } from "./api"` does, or a declaration), which hides the global.
//! A regular expression literal (`/a+/`, which the parser reads as `new RegExp(…)`) counts as
//! naming `RegExp`. std modules import what they use, so they never trigger one.
//!
//! A **conditional** global names nothing: it holds a `// load when <module> uses: <words>`
//! line and imports what it adds (`import "velt/net/http/fetch/typed";`, `extend` blocks that give
//! `Response` its `bytes()`). It is loaded once the program has loaded std module `<module>`
//! (`std/fetch/response`) and a non-std module mentions one of the words (`bytes`): a method
//! whose result type lives in modules that most users of the class never need. A word written
//! `.name` matches only a read of the property (`res.body`, `res["body"]`, `const { body } =
//! res`; not `{ body: … }`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use velt_common::SourceMap;
use velt_syntax::ast;
use velt_syntax::visit::{self, Visit};

/// A global module: its file and the names that load it.
pub(super) struct Global {
    pub file: PathBuf,
    pub names: Vec<String>,
    /// A conditional global's `// load when <module> uses: <words>` line: the std module
    /// (canonical path) and the words.
    pub when: Option<(String, Vec<String>)>,
}

/// The global modules under `<std>/prelude/global/`, sorted by file name (empty if none).
pub(super) fn global_modules(std_root: &Path) -> Vec<Global> {
    let Ok(entries) = vpm::probe::read_dir(&std_root.join("prelude").join("global")) else {
        return vec![];
    };
    let mut files: Vec<PathBuf> = entries
        .into_iter()
        .map(|e| e.path())
        .filter(|p| vpm::probe::is_file(p) && p.extension().is_some_and(|e| e == "vlt"))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|file| {
            let src = vpm::probe::read_to_string(&file).ok()?;
            let names = reexported_names(&src);
            let when = load_when(&src);
            Some(Global { file, names, when })
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

/// A conditional global's condition: the std module and the words of its first
/// `// load when <module> uses: <word> <word> …` line (none: not conditional).
fn load_when(src: &str) -> Option<(String, Vec<String>)> {
    let line = src
        .lines()
        .find_map(|l| l.trim().strip_prefix("// load when "))?;
    let (module, words) = line.split_once(" uses:")?;
    let words: Vec<String> = words.split_whitespace().map(str::to_string).collect();
    (!words.is_empty()).then(|| (module.trim().to_string(), words))
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
            ast::ItemKind::Namespace(n) => {
                names.insert(n.name.name.as_str());
            }
            ast::ItemKind::Extend(_) | ast::ItemKind::Augment(_) => {}
        }
    }
    names
}

/// Whether module `module` (source `src`) names global `name`: as a whole word of its source,
/// or for `RegExp` also by a regular expression literal.
pub(super) fn names_global(src: &str, module: &ast::Module, name: &str) -> bool {
    mentions(src, name)
        || (name == "RegExp" && src.contains('/') && has_regex(module))
        || (name == THIS_FN && mentions(src, "this") && has_this_fn(module))
}

/// The global a function expression with a `this` parameter loads (std/lang/this_fn/index.vlt).
const THIS_FN: &str = "__ThisFn";

/// Whether `module` holds a function expression (not a generator) whose `this` parameter its
/// body uses (sema's `this_fn_expr`, whose value is a `__ThisFn`).
fn has_this_fn(module: &ast::Module) -> bool {
    struct Find(bool);
    impl<'a> Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            if let ast::ExprKind::Function(d) = &e.kind {
                self.0 |= d.sig.this_param.is_some() && !d.sig.is_generator;
            }
        }
    }
    let mut find = Find(false);
    visit::walk_module(module, &mut find);
    find.0
}

/// Whether `module` holds a `new RegExp(…)` (what the parser makes of a regex literal).
fn has_regex(module: &ast::Module) -> bool {
    struct Find(bool);
    impl<'a> Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            if let ast::ExprKind::New { class, .. } = &e.kind {
                if let ast::TypeExprKind::Named { path, .. } = &class.kind {
                    self.0 |= matches!(path.as_slice(), [id] if id.name == "RegExp");
                }
            }
        }
    }
    let mut find = Find(false);
    visit::walk_module(module, &mut find);
    find.0
}

/// Whether module `module` (source `src`) uses a conditional global's word: mentions `name` as
/// a whole identifier, or for `.name` reads property `name` ([`reads_property`]).
pub(super) fn mentions_word(src: &str, module: &ast::Module, word: &str) -> bool {
    match word.strip_prefix('.') {
        Some(prop) => mentions(src, prop) && reads_property(module, prop),
        None => mentions(src, word),
    }
}

/// Whether `module` reads property `prop`: `x.prop`, `x["prop"]`, or a destructuring pattern
/// with the key `prop` (`const { prop } = x`, and in parameters, `for…of` and `catch`). An
/// object literal's `{ prop: … }` is not a read.
fn reads_property(module: &ast::Module, prop: &str) -> bool {
    struct Find<'p> {
        prop: &'p str,
        found: bool,
    }
    impl Find<'_> {
        fn pattern(&mut self, p: &ast::Pattern) {
            match &p.kind {
                ast::PatternKind::Ident(_) | ast::PatternKind::Wildcard => {}
                ast::PatternKind::Object { fields, .. } => {
                    for (key, p) in fields {
                        self.found |= key.name == self.prop;
                        self.pattern(p);
                    }
                }
                ast::PatternKind::Array { elems, .. } => elems.iter().for_each(|p| self.pattern(p)),
                ast::PatternKind::Default { pattern, .. } => self.pattern(pattern),
            }
        }
    }
    impl<'a> Visit<'a> for Find<'_> {
        fn function(&mut self, sig: &'a ast::FnSig, _body: &'a ast::Block) {
            sig.params
                .iter()
                .flat_map(|p| &p.pattern)
                .for_each(|p| self.pattern(p));
        }
        fn stmt(&mut self, s: &'a ast::Stmt) {
            match &s.kind {
                ast::StmtKind::ForOf { pattern, .. } => self.pattern(pattern),
                ast::StmtKind::Try {
                    catch: Some((Some(pattern), _)),
                    ..
                } => self.pattern(pattern),
                _ => {}
            }
        }
        fn var_decl(&mut self, v: &'a ast::VarDecl) {
            self.pattern(&v.pattern);
        }
        fn expr(&mut self, e: &'a ast::Expr) {
            match &e.kind {
                ast::ExprKind::Member { prop, .. } => self.found |= prop.name == self.prop,
                ast::ExprKind::Index { index, .. } => {
                    self.found |= matches!(&index.kind,
                        ast::ExprKind::Lit(ast::Lit::Str(key)) if key == self.prop);
                }
                ast::ExprKind::Arrow { params, .. } => {
                    params
                        .iter()
                        .flat_map(|p| &p.pattern)
                        .for_each(|p| self.pattern(p));
                }
                _ => {}
            }
        }
    }
    let mut find = Find { prop, found: false };
    visit::walk_module(module, &mut find);
    find.found
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
    fn a_dot_word_is_a_property_read() {
        let uses = |src: &str, word: &str| {
            let mut sm = SourceMap::new();
            let file = sm.add(Path::new("m.vlt"), src.to_string());
            let (module, _) = velt_syntax::parse_file(file, &sm.get(file).src);
            mentions_word(src, &module, word)
        };
        assert!(uses("const b = res.body;", ".body"));
        assert!(uses(
            "async function f() { for await (const c of req.body) {} }",
            ".body"
        ));
        assert!(uses("const b = res?.body;", ".body"));
        assert!(uses("const b = res[\"body\"];", ".body"));
        assert!(uses("const { body, status } = res;", ".body"));
        assert!(uses("const { body: b } = res;", ".body"));
        assert!(uses("const [{ body }] = [res];", ".body"));
        assert!(uses("function f({ body }: Response) {}", ".body"));
        assert!(uses("const f = ({ body }: Response) => 1;", ".body"));
        assert!(uses("for (const { body } of rs) {}", ".body"));
        assert!(!uses("fetch(u, { body: s });", ".body"));
        assert!(!uses("const body = s; fetch(u, { body });", ".body"));
        assert!(!uses("res.bodyUsed", ".body"));
        assert!(!uses("const b = res[\"bodyUsed\"];", ".body"));
        assert!(!uses("// res.body\nconst s = \"res.body\";", ".body"));
        assert!(uses("const body = 1;", "body"));
    }

    #[test]
    fn a_regex_literal_names_regexp() {
        let names = |src: &str| {
            let mut sm = SourceMap::new();
            let file = sm.add(Path::new("m.vlt"), src.to_string());
            let (module, _) = velt_syntax::parse_file(file, &sm.get(file).src);
            names_global(src, &module, "RegExp")
        };
        assert!(names(
            "function f(): bool {\n  return /a+/.test(\"caa\");\n}\n"
        ));
        assert!(!names("function f(): number {\n  return 4 / 2 / 1;\n}\n"));
        assert!(!names("// a /comment/\nfunction f() {}\n"));
    }

    #[test]
    fn names_are_the_reexports() {
        let src = "// globals\nexport { fetch, Response as R } from \"velt/net/http\";\n\
                   export { URL } from \"velt/net/url\";\n";
        assert_eq!(reexported_names(src), ["fetch", "R", "URL"]);
    }

    #[test]
    fn a_conditional_global_names_its_module_and_words() {
        let src = "// Bytes.\n// load when std/net/http/fetch/response uses: bytes arrayBuffer\nimport \"velt/net/http/fetch/typed\";\n";
        assert_eq!(
            load_when(src),
            Some((
                "std/net/http/fetch/response".into(),
                vec!["bytes".into(), "arrayBuffer".into()]
            ))
        );
        assert!(reexported_names(src).is_empty());
        assert_eq!(load_when("export { A } from \"velt/a\";\n"), None);
    }

    #[test]
    fn lists_global_modules_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(global_modules(tmp.path()).is_empty());
        let dir = tmp.path().join("prelude/global");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.vlt"), "export { B } from \"velt/b\";\n").unwrap();
        std::fs::write(dir.join("a.vlt"), "export { A } from \"velt/a\";\n").unwrap();
        let g = global_modules(tmp.path());
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].names, ["A"]);
        assert_eq!(g[1].file, dir.join("b.vlt"));
    }
}
