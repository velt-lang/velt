//! Finding test files (`*.test.vlt`, `*.test.ts`, `*.test.tsx`) and the test functions inside
//! them.
//!
//! A test is a top-level `export function test_*()` (or `export async function test_*()`) with no
//! parameters and no generics. Tests must be exported because the generated harness imports them
//! from a separate module; a `test_*` function that is not exported (or takes parameters) is
//! reported and skipped.

use std::path::{Path, PathBuf};

use velt_syntax::ast;

/// How test files are named, for messages.
pub const TEST_FILES: &str = "*.test.vlt, *.test.ts, *.test.tsx";

/// Whether a file named `name` is a test file: a source module (`vpm::sources`) whose name
/// without the extension ends in `.test`.
pub fn is_test_file(name: &str) -> bool {
    vpm::sources::strip_source_extension(name).is_some_and(|stem| stem.ends_with(".test"))
}

/// Test files under `path` (recursively, as [`vpm::sources::walks_into`] walks: skipping
/// `target/`, `node_modules/`, hidden and symlinked directories and nested packages), sorted. An explicitly named file is used even without the `.test` suffix.
pub fn find_test_files(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        return Err(format!("`{}` does not exist", path.display()));
    }
    files_where(path, &is_test_file)
}

/// Source modules under `dir` (`.vlt`, `.ts`, `.tsx`, not `package.vlt`; recursively, skipping
/// `target/`, `node_modules/`, hidden and symlinked directories and nested packages), sorted.
pub fn source_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    files_where(dir, &vpm::sources::walk_keeps)
}

/// Files under `dir` whose names satisfy `keep`, sorted: recursively, into the directories
/// [`vpm::sources::walks_into`] enters (the walk the language server's `tsCompat` folder
/// membership follows, [`vpm::sources::in_folder`]). A nested package's files are its own.
fn files_where(dir: &Path, keep: &dyn Fn(&str) -> bool) -> Result<Vec<PathBuf>, String> {
    let mut out = vec![];
    collect(dir, keep, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect(dir: &Path, keep: &dyn Fn(&str) -> bool, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if vpm::sources::walks_into(&path) {
                collect(&path, keep, out)?;
            }
        } else if keep(&name) {
            out.push(path);
        }
    }
    Ok(())
}

/// The tests of one module, in source order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TestFunctions {
    /// Runnable tests.
    pub tests: Vec<String>,
    /// The runnable tests that are `async` (the harness awaits them).
    pub async_tests: Vec<String>,
    /// `test_*` functions that cannot run, with the reason.
    pub skipped: Vec<(String, &'static str)>,
    /// The module runs code when it is evaluated ([`runs_code`]), so it gets a harness of its
    /// own instead of sharing one with the other test files.
    pub runs_code: bool,
}

/// Collect `test_*` functions from a parsed test module.
pub fn test_functions(module: &ast::Module) -> TestFunctions {
    let mut found = TestFunctions {
        runs_code: runs_code(module),
        ..Default::default()
    };
    for item in &module.items {
        let ast::ItemKind::Function(f) = &item.kind else {
            continue;
        };
        let name = &f.sig.name.name;
        if !name.starts_with("test_") {
            continue;
        }
        let problem = if !item.exported {
            Some("not exported (write `export function`)")
        } else if !f.sig.params.is_empty() || !f.sig.generics.is_empty() {
            Some("tests must not take parameters or be generic")
        } else {
            None
        };
        match problem {
            Some(reason) => found.skipped.push((name.clone(), reason)),
            None => {
                if f.sig.is_async {
                    found.async_tests.push(name.clone());
                }
                found.tests.push(name.clone());
            }
        }
    }
    found
}

/// Does a module that `module` (the file `file`) imports by a relative path, directly or through
/// others, run code when it is evaluated? Its code would run in the process of every test file
/// sharing a harness, and a failure there would end them all. Package and `velt:` imports are
/// not followed; a module that cannot be read or parsed counts as running code (its file then
/// gets a harness of its own, which reports the error).
pub fn imports_run_code(file: &Path, module: &ast::Module) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![(file.to_path_buf(), relative_imports(module))];
    while let Some((from, specs)) = stack.pop() {
        let dir = from.parent().unwrap_or(Path::new(""));
        for spec in specs {
            let Some(path) = resolve(&dir.join(&spec)) else {
                // Not a source file (JSON, an asset) or missing: the build says which.
                continue;
            };
            if !seen.insert(path.clone()) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&path) else {
                return true;
            };
            let (m, diags) = velt_syntax::parse_file(velt_common::FileId(0), &src);
            if diags.iter().any(|d| d.is_error()) || runs_code(&m) {
                return true;
            }
            stack.push((path, relative_imports(&m)));
        }
    }
    false
}

fn relative_imports(module: &ast::Module) -> Vec<String> {
    module
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ast::ItemKind::Import(i) if i.from.starts_with("./") || i.from.starts_with("../") => {
                Some(i.from.clone())
            }
            _ => None,
        })
        .collect()
}

/// The source file a relative import `base` names: itself, with an extension, its `index`, or
/// for `./m.js` the `./m.ts` beside it.
fn resolve(base: &Path) -> Option<PathBuf> {
    let source = |p: &Path| {
        matches!(
            p.extension().and_then(|e| e.to_str()),
            Some("vlt" | "ts" | "tsx")
        ) && p.is_file()
    };
    if source(base) {
        return Some(base.to_path_buf());
    }
    // `./m.js` names `./m.ts`, as in TypeScript.
    if base.extension().is_some_and(|e| e == "js") {
        return ["ts", "tsx", "vlt"]
            .iter()
            .map(|ext| base.with_extension(ext))
            .find(|p| p.is_file());
    }
    let name = base.file_name()?.to_string_lossy().into_owned();
    ["vlt", "ts", "tsx"]
        .iter()
        .map(|ext| base.with_file_name(format!("{name}.{ext}")))
        .chain(
            ["vlt", "ts", "tsx"]
                .iter()
                .map(|ext| base.join(format!("index.{ext}"))),
        )
        .find(|p| p.is_file())
}

/// Does evaluating `module` do anything but declare? Every module a program imports is evaluated
/// when it starts (docs/reference/modules.md), so a test file in a shared harness is evaluated in
/// every test file's process: only one that just declares can share one. Top-level statements,
/// and a variable or static field initialized with a call (`const C = component(...)`), run code;
/// literals, names, operators and functions do not.
pub fn runs_code(module: &ast::Module) -> bool {
    module.items.iter().any(|item| match &item.kind {
        ast::ItemKind::Function(f) => velt_syntax::is_script_main(&f.sig.name),
        ast::ItemKind::Var(v) => v.init.as_ref().is_some_and(|e| !inert(e)),
        ast::ItemKind::Class(t) | ast::ItemKind::Struct(t) => t
            .fields
            .iter()
            .any(|f| f.is_static && f.default.as_ref().is_some_and(|e| !inert(e))),
        _ => false,
    })
}

/// Does evaluating `e` only make a value, without calling anything?
fn inert(e: &ast::Expr) -> bool {
    use ast::ExprKind as E;
    let prop = |p: &ast::ObjectProp| match p {
        ast::ObjectProp::KeyValue(_, v) | ast::ObjectProp::Spread(v) => inert(v),
        ast::ObjectProp::Shorthand(_) | ast::ObjectProp::Method(_) => true,
    };
    match &e.kind {
        E::Lit(_) | E::Ident(_) | E::Arrow { .. } | E::Function(_) => true,
        E::Template { exprs, .. } => exprs.iter().all(inert),
        E::Unary { expr, .. } | E::Spread(expr) | E::Paren(expr) | E::NonNull(expr) => inert(expr),
        E::Cast { expr, .. } => inert(expr),
        E::Binary { lhs, rhs, .. } => inert(lhs) && inert(rhs),
        E::Cond { cond, then, els } => inert(cond) && inert(then) && inert(els),
        E::Array(items) => items.iter().all(inert),
        E::Object(props) | E::StructLit { props, .. } => props.iter().all(prop),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_common::FileId;

    #[test]
    fn finds_test_files_recursively() {
        let tmp = tempfile::tempdir().unwrap();
        for f in [
            "a.test.vlt",
            "sub/b.test.vlt",
            "sub/c.test.ts",
            "sub/d.test.tsx",
            "sub/e.test.d.ts",
            "main.vlt",
            "target/c.test.vlt",
            ".git/d.test.vlt",
        ] {
            let p = tmp.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        let found = find_test_files(tmp.path()).unwrap();
        assert_eq!(
            found,
            [
                tmp.path().join("a.test.vlt"),
                tmp.path().join("sub/b.test.vlt"),
                tmp.path().join("sub/c.test.ts"),
                tmp.path().join("sub/d.test.tsx"),
            ]
        );
        assert_eq!(
            find_test_files(&tmp.path().join("main.vlt")).unwrap().len(),
            1
        );
        assert!(find_test_files(&tmp.path().join("nope")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directories_are_not_walked() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("a.test.vlt"), "").unwrap();
        std::fs::write(tmp.path().join("b.vlt"), "").unwrap();
        std::os::unix::fs::symlink("..", sub.join("up")).unwrap();
        std::os::unix::fs::symlink("b.vlt", tmp.path().join("link.vlt")).unwrap();
        let found = source_files(&sub).unwrap();
        assert_eq!(found, [sub.join("a.test.vlt")]);
        // A symlinked file is still a file.
        let found = source_files(tmp.path()).unwrap();
        let link = tmp.path().join("link.vlt");
        assert_eq!(
            found,
            [tmp.path().join("b.vlt"), link, sub.join("a.test.vlt")]
        );
    }

    #[test]
    fn nested_packages_and_manifests_are_not_sources() {
        let tmp = tempfile::tempdir().unwrap();
        for f in [
            "package.vlt",
            "src/a.vlt",
            "src/a.test.vlt",
            "src/inner/package.vlt",
            "src/inner/b.vlt",
            "src/inner/b.test.vlt",
        ] {
            let p = tmp.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        assert_eq!(
            source_files(tmp.path()).unwrap(),
            [
                tmp.path().join("src/a.test.vlt"),
                tmp.path().join("src/a.vlt")
            ]
        );
        assert_eq!(
            find_test_files(tmp.path()).unwrap(),
            [tmp.path().join("src/a.test.vlt")]
        );
        // Named directly, the nested package's folder is walked (without its manifest).
        let inner = tmp.path().join("src/inner");
        assert_eq!(
            source_files(&inner).unwrap(),
            [inner.join("b.test.vlt"), inner.join("b.vlt")]
        );
    }

    #[test]
    fn collects_exported_parameterless_tests() {
        let src = "
            export function test_add() {}
            function test_hidden() {}
            export function test_param(x: i64) {}
            export function helper() {}
            export function test_sub(): void {}
            export async function test_io() {}
        ";
        let (module, diags) = velt_syntax::parse_file(FileId(0), src);
        assert!(diags.is_empty(), "{diags:?}");
        let found = test_functions(&module);
        assert_eq!(found.tests, ["test_add", "test_sub", "test_io"]);
        assert_eq!(found.async_tests, ["test_io"]);
        let skipped: Vec<&str> = found.skipped.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(skipped, ["test_hidden", "test_param"]);
    }

    #[test]
    fn declarations_and_constants_do_not_run_code() {
        let runs = |src: &str| {
            let (module, diags) = velt_syntax::parse_file(FileId(0), src);
            assert!(diags.is_empty(), "{diags:?}");
            test_functions(&module).runs_code
        };
        assert!(!runs(
            "import { f } from \"./f\";\n\
             const LINE = `a b`;\n\
             const N = -1 + 2;\n\
             const XS = [1, 2, ...[3]];\n\
             const O = { a: 1, b: \"x\" as string };\n\
             const G = () => f();\n\
             class C { static readonly K: i64 = 3; }\n\
             interface I { x: i64 }\n\
             type T = i64;\n\
             export function test_a() {}\n"
        ));
        assert!(runs("console.log(\"hi\");\nexport function test_a() {}\n"));
        assert!(runs(
            "const C = make();\nfunction make(): i64 { return 1; }\n"
        ));
        assert!(runs(
            "const T = `${make()}`;\nfunction make(): i64 { return 1; }\n"
        ));
        assert!(runs("class C { static { console.log(1); } }\n"));
        assert!(runs(
            "class C { static n: i64 = make(); }\nfunction make(): i64 { return 1; }\n"
        ));
    }

    #[test]
    fn code_run_by_what_a_test_file_imports() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let write = |rel: &str, src: &str| {
            let p = d.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, src).unwrap();
        };
        write("lib/pure.vlt", "export function f(): i64 { return 1; }\n");
        write("lib/index.ts", "export * from \"./pure.vlt\";\n");
        write(
            "lib/init.ts",
            "export const DATA = load();\nfunction load(): i64 { return 1; }\n",
        );
        write(
            "lib/via.vlt",
            "import { DATA } from \"./init.js\";\nexport const D2 = 2;\n",
        );
        let imports = |src: &str| {
            let file = d.join("t/a.test.vlt");
            write("t/a.test.vlt", src);
            let (module, diags) = velt_syntax::parse_file(FileId(0), src);
            assert!(diags.is_empty(), "{diags:?}");
            imports_run_code(&file, &module)
        };
        // Through `index`, a re-export and a package that is not followed: only declarations.
        assert!(!imports(
            "import { f } from \"../lib\";\nimport { x } from \"pkg\";\nimport \"./missing.json\";\n"
        ));
        // Code two imports away, behind a `.js` name for a `.ts` file.
        assert!(imports("import { D2 } from \"../lib/via\";\n"));
        // An import cycle ends.
        write(
            "lib/c1.vlt",
            "import { b } from \"./c2\";\nexport const a = 1;\n",
        );
        write(
            "lib/c2.vlt",
            "import { a } from \"./c1\";\nexport const b = 2;\n",
        );
        assert!(!imports("import { a } from \"../lib/c1\";\n"));
    }
}
