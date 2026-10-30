//! Finding test files (`*.test.vlt`, `*.test.ts`, `*.test.tsx`) and the test functions inside
//! them.
//!
//! A test is a top-level `export function test_*()` (or `export async function test_*()`) with no
//! parameters and no generics. Tests must be exported because the generated harness imports them
//! from a separate module; a `test_*` function that is not exported (or takes parameters) is
//! reported and skipped. Tests registered with `describe`, `it` and `test` (`velt/test`, or
//! `vitest`) are known only when the program runs; here they are counted ([`TestFunctions`]).

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
    // Through `probe`, so a recording (`member_stamp`) notes each directory listed: a new test
    // file is a change.
    let entries =
        vpm::probe::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    for entry in entries {
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
    /// Its top-level statements that are `describe`, `it`, `test` and hook calls, each of
    /// which queues one registration (`velt/test/suite`); they don't count as running code.
    pub queued: usize,
    /// It imports `describe`, `it`, `test` or a hook, so it may have tests that only running it
    /// lists (`--filter` can't rule it out).
    pub registers: bool,
}

/// The functions of `velt/test` that register tests and hooks.
const REGISTERING: [&str; 7] = [
    "describe",
    "it",
    "test",
    "beforeAll",
    "afterAll",
    "beforeEach",
    "afterEach",
];

/// The modules that export them.
const TEST_MODULES: [&str; 2] = ["velt/test", "vitest"];

/// The local names `module` imports the [`REGISTERING`] functions under.
fn registering_names(module: &ast::Module) -> Vec<String> {
    let mut names = vec![];
    for item in &module.items {
        let ast::ItemKind::Import(i) = &item.kind else {
            continue;
        };
        if !TEST_MODULES.contains(&i.from.as_str()) {
            continue;
        }
        for n in &i.names {
            if !n.type_only && REGISTERING.contains(&n.name.name.as_str()) {
                names.push(n.alias.as_ref().unwrap_or(&n.name).name.clone());
            }
        }
    }
    names
}

/// How many of `stmts` (a file's top-level statements) are calls of one of `names` with inert
/// arguments (a name, a callback); `None` if a statement among them runs other code. A variable
/// initialized with an inert value is among them when only the statements use it (the parser
/// moves it into them, `velt_syntax` `parser/script.rs`), and doesn't count.
fn registrations(stmts: &[ast::Stmt], names: &[String]) -> Option<usize> {
    let mut count = 0;
    for stmt in stmts {
        match &stmt.kind {
            ast::StmtKind::Var(v)
                if matches!(v.kind, ast::VarKind::Const | ast::VarKind::Let)
                    && v.init.as_ref().is_none_or(inert) => {}
            ast::StmtKind::Expr(e) => {
                let ast::ExprKind::Call { callee, args, .. } = &e.kind else {
                    return None;
                };
                let registering =
                    matches!(&callee.kind, ast::ExprKind::Ident(id) if names.contains(&id.name));
                if !registering || !args.iter().all(inert) {
                    return None;
                }
                count += 1;
            }
            _ => return None,
        }
    }
    Some(count)
}

/// Collect `test_*` functions from a parsed test module, and count its top-level registrations.
pub fn test_functions(module: &ast::Module) -> TestFunctions {
    let names = registering_names(module);
    let mut found = TestFunctions {
        registers: !names.is_empty(),
        ..Default::default()
    };
    // Top-level statements that only register leave the module as one that only declares.
    let mut other_code = false;
    for item in &module.items {
        let ast::ItemKind::Function(f) = &item.kind else {
            other_code = other_code || item_runs_code(item);
            continue;
        };
        if velt_syntax::is_script_main(&f.sig.name) {
            match registrations(&f.body.stmts, &names) {
                Some(n) => found.queued = n,
                None => other_code = true,
            }
            continue;
        }
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
    found.runs_code = other_code;
    found
}

/// Does a module that `module` (the file `file`) imports by a relative path, directly or through
/// others, run code when it is evaluated? Its code would run in the process of every test file
/// sharing a harness, and a failure there would end them all. Package and `velt:` imports are
/// not followed; a module that cannot be read or parsed counts as running code (its file then
/// gets a harness of its own, which reports the error).
pub fn imports_run_code(file: &Path, module: &ast::Module, scanned: &mut Scanned) -> bool {
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
            // Another test file: its top-level registrations would run before this file's and
            // be taken for them.
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            if name.is_some_and(|n| is_test_file(&n)) {
                return true;
            }
            let Some(imports) = scanned
                .modules
                .entry(path.clone())
                .or_insert_with(|| scan(&path))
                .clone()
            else {
                return true;
            };
            stack.push((path, imports));
        }
    }
    false
}

/// The modules [`imports_run_code`] has read, for every test file of a run: a module many of
/// them import is read and parsed once.
#[derive(Default)]
pub struct Scanned {
    /// Each module's relative imports; `None` if it runs code or cannot be read or parsed.
    modules: std::collections::HashMap<PathBuf, Option<Vec<String>>>,
}

/// The relative imports of the module `path`, if it only declares.
fn scan(path: &Path) -> Option<Vec<String>> {
    let src = std::fs::read_to_string(path).ok()?;
    let (m, diags) = velt_syntax::parse_file(velt_common::FileId(0), &src);
    if diags.iter().any(|d| d.is_error()) || runs_code(&m) {
        return None;
    }
    Some(relative_imports(&m))
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
    module.items.iter().any(item_runs_code)
}

/// Does evaluating `item` run code ([`runs_code`])?
fn item_runs_code(item: &ast::Item) -> bool {
    match &item.kind {
        ast::ItemKind::Function(f) => velt_syntax::is_script_main(&f.sig.name),
        ast::ItemKind::Var(v) => v.init.as_ref().is_some_and(|e| !inert(e)),
        ast::ItemKind::Class(t) | ast::ItemKind::Struct(t) => t
            .fields
            .iter()
            .any(|f| f.is_static && f.default.as_ref().is_some_and(|e| !inert(e))),
        _ => false,
    }
}

/// Does evaluating `e` only make a value, without calling anything or failing? `n!`, a cast of a
/// value that isn't a literal (`raw as string`) and integer `/` and `%` (by zero) can panic, and in
/// a shared program that would stop every file's tests, so they count as running code; `as const`
/// and a cast of a literal can't fail.
fn inert(e: &ast::Expr) -> bool {
    use ast::ExprKind as E;
    let prop = |p: &ast::ObjectProp| match p {
        ast::ObjectProp::KeyValue(_, v) | ast::ObjectProp::Spread(v) => inert(v),
        ast::ObjectProp::Shorthand(_) | ast::ObjectProp::Method(_) => true,
    };
    match &e.kind {
        E::Lit(_) | E::Ident(_) | E::Arrow { .. } | E::Function(_) => true,
        E::Template { exprs, .. } => exprs.iter().all(inert),
        E::Unary { expr, .. } | E::Spread(expr) | E::Paren(expr) => inert(expr),
        E::Cast { expr, ty } => {
            let as_const = matches!(&ty.kind, ast::TypeExprKind::Named { path, args }
                if args.is_empty() && path.len() == 1 && path[0].name == "const");
            (as_const || matches!(expr.kind, E::Lit(_))) && inert(expr)
        }
        E::Binary { op, lhs, rhs } => {
            !matches!(op, ast::BinaryOp::Div | ast::BinaryOp::Rem) && inert(lhs) && inert(rhs)
        }
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
        assert!(!runs("const P = { x: 1 } as const;\nconst H = 0.5 * 2;\n"));
        // What can panic while the module is evaluated.
        assert!(runs(
            "const n: string | null = null;\nconst m = n!;\nexport function test_a() {}\n"
        ));
        assert!(runs("const raw: any = 1;\nconst s = raw as string;\n"));
        assert!(runs("const z: i64 = 0;\nconst q: i64 = 10 / z;\n"));
        assert!(runs("const z: i64 = 0;\nconst q: i64 = 10 % z;\n"));
        assert!(runs("class C { static { console.log(1); } }\n"));
        assert!(runs(
            "class C { static n: i64 = make(); }\nfunction make(): i64 { return 1; }\n"
        ));
    }

    #[test]
    fn top_level_registrations_are_counted_not_run() {
        let found = |src: &str| {
            let (module, diags) = velt_syntax::parse_file(FileId(0), src);
            assert!(diags.is_empty(), "{diags:?}");
            test_functions(&module)
        };
        let f = found(
            "import { describe, it as spec } from \"velt/test\";\n\
             const log: string[] = [];\n\
             describe(`a ${1}`, () => { spec(\"x\", () => { log.push(\"x\"); }); });\n\
             spec(\"y\", async () => {});\n\
             export function test_z() {}\n",
        );
        assert_eq!((f.queued, f.registers, f.runs_code), (2, true, false));
        assert_eq!(f.tests, ["test_z"]);
        // Vitest's module is `velt/test`.
        let f = found("import { test } from \"vitest\";\ntest(\"t\", () => {});\n");
        assert_eq!((f.queued, f.registers, f.runs_code), (1, true, false));
        // Anything else at the top level runs code, and so does a registration whose arguments
        // do (or a `using`).
        for src in [
            "import { it } from \"velt/test\";\nit(\"a\", () => {});\nconsole.log(1);\n",
            "import { it } from \"velt/test\";\nit(name(), () => {});\nfunction name(): string { return \"a\"; }\n",
            "import { it } from \"./mine\";\nit(\"a\", () => {});\n",
        ] {
            let f = found(src);
            assert!(f.runs_code, "{src}");
        }
        let f = found("export function test_a() {}\n");
        assert_eq!((f.queued, f.registers, f.runs_code), (0, false, false));
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
            imports_run_code(&file, &module, &mut Scanned::default())
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
        // Another test file: its top-level registrations would be taken for this file's.
        write("t/other.test.vlt", "export function test_o() {}\n");
        assert!(imports("import { x } from \"./other.test\";\n"));

        // One scan for a run's test files: a module they share is read once (here, before it
        // changes on disk), and what it imports is still followed.
        let mut scanned = Scanned::default();
        let src = "import { a } from \"../lib/c1\";\n";
        let (module, _) = velt_syntax::parse_file(FileId(0), src);
        assert!(!imports_run_code(
            &d.join("t/a.test.vlt"),
            &module,
            &mut scanned
        ));
        write(
            "lib/c1.vlt",
            "export const a = load();\nfunction load(): i64 { return 1; }\n",
        );
        assert!(!imports_run_code(
            &d.join("t/b.test.vlt"),
            &module,
            &mut scanned
        ));
        assert!(imports_run_code(
            &d.join("t/b.test.vlt"),
            &module,
            &mut Scanned::default()
        ));
        write(
            "lib/c2.vlt",
            "export const b = load();\nfunction load(): i64 { return 1; }\n",
        );
        write(
            "lib/c1.vlt",
            "import { b } from \"./c2\";\nexport const a = 1;\n",
        );
        let mut scanned = Scanned::default();
        assert!(imports_run_code(
            &d.join("t/a.test.vlt"),
            &module,
            &mut scanned
        ));
        assert!(imports_run_code(
            &d.join("t/b.test.vlt"),
            &module,
            &mut scanned
        ));
    }
}
