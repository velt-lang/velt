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
            let mut names = reexported_names(&src);
            if names.is_empty() {
                let marked = [
                    (THIS_FN_LINE, THIS_FN),
                    (ASYNC_HOST_LINE, ASYNC_HOST),
                    (NUMBER_TEXT_LINE, NUMBER_TEXT),
                ];
                names.extend(
                    marked
                        .iter()
                        .filter(|m| src.contains(m.0))
                        .map(|m| m.1.into()),
                );
            }
            let when = load_when(&src);
            Some(Global { file, names, when })
        })
        .collect()
}

/// The names a global module's `export { … } from "…"` items re-export (under their aliases).
fn reexported_names(src: &str) -> Vec<String> {
    let (module, _) = super::std_ast::parse(super::std_ast::NO_FILE, src, false, true);
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
        || (name == ASYNC_HOST && mentions(src, "host") && has_async_host(module))
        || (name == NUMBER_TEXT && may_print(src) && has_text_conversion(module))
}

/// The trigger of the global holding the hooks for a number in a template or `String()`
/// (std/prelude/global/number_text.vlt: std's number printing). No source can mention it as a
/// word.
const NUMBER_TEXT: &str = "<number converted to text>";

/// The line that marks that global ([`NUMBER_TEXT`]).
const NUMBER_TEXT_LINE: &str = "// load for: a number converted to text";

/// Whether source `src` has what [`has_text_conversion`] looks for, as text: a cheap test first.
fn may_print(src: &str) -> bool {
    src.contains("${") || src.contains("String") || src.contains('+')
}

/// Whether `module` may convert a number to text the way lowering prints one through std: a
/// template literal with an expression, `String(…)`, a `.toString(…)` call, or `+` (or `+=`)
/// with a string literal or a template on either side. The types are not known yet: this only
/// keeps std's number printing out of programs that cannot use it (hello world). A conversion
/// it misses is printed by the runtime, and sema checks the hooks only once the program's code
/// really converts a number (`velt_sema` `lazy_std`).
fn has_text_conversion(module: &ast::Module) -> bool {
    struct Find(bool);
    fn texty(e: &ast::Expr) -> bool {
        matches!(
            e.kind,
            ast::ExprKind::Lit(ast::Lit::Str(_)) | ast::ExprKind::Template { .. }
        )
    }
    impl<'a> Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            self.0 |= match &e.kind {
                ast::ExprKind::Template { exprs, .. } => !exprs.is_empty(),
                ast::ExprKind::Call { callee, .. } => match &callee.kind {
                    ast::ExprKind::Ident(id) => id.name == "String",
                    ast::ExprKind::Member { prop, .. } => prop.name == "toString",
                    _ => false,
                },
                ast::ExprKind::Binary {
                    op: ast::BinaryOp::Add,
                    lhs,
                    rhs,
                } => texty(lhs) || texty(rhs),
                ast::ExprKind::Assign {
                    op: Some(ast::BinaryOp::Add),
                    value,
                    ..
                } => texty(value),
                _ => false,
            };
        }
    }
    let mut find = Find(false);
    visit::walk_module(module, &mut find);
    find.0
}

/// The trigger of the global an async host function loads (std/prelude/global/host_async.vlt:
/// the hooks that await its call's promise). No source can mention it as a word.
const ASYNC_HOST: &str = "<async host function>";

/// The line that marks that global ([`ASYNC_HOST`]).
const ASYNC_HOST_LINE: &str = "// load for: an async host function";

/// Whether `module` declares an async host function (`declare host("m") async function`).
fn has_async_host(module: &ast::Module) -> bool {
    module.items.iter().any(
        |item| matches!(&item.kind, ast::ItemKind::ExternFn(f) if f.host.is_some() && f.is_async),
    )
}

/// The trigger of the global a function expression with a `this` parameter loads
/// (std/prelude/global/this_fn.vlt, which re-exports nothing: its value's class and calls are
/// compiler hooks, velt_common `std_hooks.rs`). No source can mention it as a word.
const THIS_FN: &str = "<function with this>";

/// The line that marks that global ([`THIS_FN`]).
const THIS_FN_LINE: &str = "// load for: a function expression with a `this` parameter";

/// Whether `module` holds a function expression (not a generator) whose `this` parameter its
/// body uses (sema's `this_fn_expr`, whose value is a `ThisFn`), or a function declared with a
/// `this` parameter that may be used as a value (not generic, async or a generator: sema's
/// `this_fn_value` makes a `ThisFn` of it).
fn has_this_fn(module: &ast::Module) -> bool {
    struct Find(bool);
    impl<'a> Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            if let ast::ExprKind::Function(d) = &e.kind {
                self.0 |= d.sig.this_param.is_some() && !d.sig.is_generator;
            }
        }
        fn function(&mut self, sig: &'a ast::FnSig, _body: &'a ast::Block) {
            self.0 |= sig.this_param.is_some()
                && sig.generics.is_empty()
                && !sig.is_async
                && !sig.is_generator;
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
    use velt_common::SourceMap;

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

    #[test]
    fn the_global_of_this_functions_loads_on_its_marker_not_a_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("prelude/global");
        std::fs::create_dir_all(&dir).unwrap();
        let src = format!("{THIS_FN_LINE}\nimport \"velt/lang/this_fn\";\n");
        std::fs::write(dir.join("this_fn.vlt"), src).unwrap();
        let g = global_modules(tmp.path());
        assert_eq!(g[0].names, [THIS_FN]);
        let parse = |src: &str| {
            let mut sm = SourceMap::new();
            let file = sm.add(Path::new("main.vlt"), src.to_string());
            velt_syntax::parse_file(file, &sm.get(file).src).0
        };
        let with = "const f = function (this: any) { return this; };\n";
        assert!(names_global(with, &parse(with), THIS_FN));
        // The class's name is no trigger: a program cannot name it.
        let named = "const ThisFn = 1;\n";
        assert!(!names_global(named, &parse(named), THIS_FN));
    }

    #[test]
    fn the_global_of_async_host_functions_loads_on_its_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("prelude/global");
        std::fs::create_dir_all(&dir).unwrap();
        let src = format!("{ASYNC_HOST_LINE}\nimport \"velt/host/promise\";\n");
        std::fs::write(dir.join("host_async.vlt"), src).unwrap();
        assert_eq!(global_modules(tmp.path())[0].names, [ASYNC_HOST]);
        let parse = |src: &str| {
            let mut sm = SourceMap::new();
            let file = sm.add(Path::new("main.vlt"), src.to_string());
            velt_syntax::parse_file(file, &sm.get(file).src).0
        };
        let with = "declare host(\"m\") async function f(): Promise<u32>;\n";
        assert!(names_global(with, &parse(with), ASYNC_HOST));
        let sync = "declare host(\"m\") function f(): u32;\n";
        assert!(!names_global(sync, &parse(sync), ASYNC_HOST));
    }
}
