use std::collections::BTreeMap;

use super::*;

/// A temp dir with helpers to write `.vlt` files.
pub(super) struct Tree(tempfile::TempDir);

impl Tree {
    pub(super) fn new() -> Tree {
        Tree(tempfile::tempdir().unwrap())
    }
    pub(super) fn path(&self, rel: &str) -> PathBuf {
        self.0.path().join(rel)
    }
    pub(super) fn write(&self, rel: &str, src: &str) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, src).unwrap();
        p
    }
}

pub(super) fn load(root: &Path, opts: LoadOptions) -> (Loaded, Diagnostics, SourceMap) {
    let mut sm = SourceMap::new();
    let mut diags = vec![];
    let loaded = load_program(&mut sm, root, opts, &mut diags).unwrap();
    (loaded, diags, sm)
}

pub(super) fn paths(l: &Loaded) -> Vec<&str> {
    l.modules.iter().map(|m| m.path.as_str()).collect()
}

pub(super) fn imports(l: &Loaded, module: &str) -> Vec<(String, String)> {
    l.modules
        .iter()
        .find(|m| m.path == module)
        .unwrap()
        .imports
        .clone()
}

pub(super) fn messages(diags: &Diagnostics) -> Vec<String> {
    diags.iter().map(|d| d.message.clone()).collect()
}

#[test]
fn relative_imports_cycles_and_dedupe() {
    let t = Tree::new();
    let root = t.write(
        "app/main.vlt",
        "import { a } from \"./a\";\nimport { b } from \"./lib/b\";\nfunction main() {}\n",
    );
    t.write(
        "app/a.vlt",
        "import { b } from \"./lib/b\";\nexport function a() {}\n",
    );
    t.write(
        "app/lib/b.vlt",
        "import { a } from \"../a\";\nimport { main } from \"../main\";\nexport function b() {}\n",
    );
    let (l, diags, _) = load(&root, LoadOptions::default());
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(paths(&l), ["main", "a", "lib/b"]);
    assert_eq!(l.root, 0);
    assert_eq!(
        imports(&l, "main"),
        [
            ("./a".into(), "a".into()),
            ("./lib/b".into(), "lib/b".into())
        ]
    );
    assert_eq!(
        imports(&l, "lib/b"),
        [
            ("../a".into(), "a".into()),
            ("../main".into(), "main".into())
        ]
    );
}

#[test]
fn missing_module_reports_tried_paths_at_the_specifier() {
    let t = Tree::new();
    let src = "import { x } from \"./nope\";\nfunction main() {}\n";
    let root = t.write("main.vlt", src);
    let (l, diags, sm) = load(&root, LoadOptions::default());
    assert_eq!(messages(&diags), ["cannot find module `./nope`"]);
    assert!(diags[0].notes[0].starts_with("tried `") && diags[0].notes[0].contains("nope.vlt"));
    let rendered = diags[0].render(&sm);
    assert!(rendered.contains("main.vlt:1:19"), "{rendered}");
    assert!(l.modules[0].imports.is_empty());
}

#[test]
fn globals_load_when_a_user_module_names_them() {
    let t = Tree::new();
    let std = t.path("std");
    t.write(
        "std/prelude/core.vlt",
        "export function id(): i64 { return 0; }\n",
    );
    t.write(
        "std/prelude/global/web.vlt",
        "export { fetch, Response } from \"velt:web\";\n",
    );
    t.write(
        "std/prelude/global/url.vlt",
        "export { URL } from \"velt:url\";\n",
    );
    // A std module that mentions a global's name does not load it.
    t.write(
        "std/web.vlt",
        "// URL\nexport function fetch() {}\nexport class Response {}\n",
    );
    t.write("std/url.vlt", "export class URL {}\n");
    t.write("std/server.vlt", "export class Response {}\n");
    let opts = || LoadOptions {
        std_root: Some(std.clone()),
        ..Default::default()
    };
    let plain = t.write("p/plain.vlt", "// prefetch\nfunction main() {}\n");
    let (l, diags, _) = load(&plain, opts());
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(paths(&l), ["std/prelude/core", "main"]);

    t.write(
        "p/lib.vlt",
        "export async function get() { await fetch(); }\n",
    );
    let root = t.write(
        "p/main.vlt",
        "import { get } from \"./lib\";\nfunction main() {}\n",
    );
    let (l, diags, _) = load(&root, opts());
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    let want = [
        "std/prelude/core",
        "main",
        "lib",
        "std/prelude/global/web",
        "std/web",
    ];
    assert_eq!(paths(&l), want);

    // Names a module binds itself (an import, a declaration) hide the global: nothing loads.
    let own = t.write(
        "p/own.vlt",
        "import { Response } from \"velt:server\";\nclass URL {}\n\
         function main() { const r = new Response(); const u = new URL(); }\n",
    );
    let (l, diags, _) = load(&own, opts());
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(paths(&l), ["std/prelude/core", "main", "std/server"]);
}

#[test]
fn std_modules_and_prelude_come_first() {
    let t = Tree::new();
    let std = t.path("std");
    t.write(
        "std/prelude/b_array.vlt",
        "export function len(): i64 { return 0; }\n",
    );
    t.write(
        "std/prelude/a_core.vlt",
        "import { clamp } from \"velt:math\";\nexport function id(): i64 { return 0; }\n",
    );
    t.write(
        "std/math.vlt",
        "export function clamp(x: i64, lo: i64, hi: i64): i64 { return x; }\n",
    );
    t.write(
        "std/net/index.vlt",
        "import { clamp } from \"../math\";\nexport function connect() {}\n",
    );
    let root = t.write("p/main.vlt", "import { clamp } from \"velt:math\";\nimport { connect } from \"velt:net\";\nfunction main() {}\n");
    let (l, diags, _) = load(
        &root,
        LoadOptions {
            std_root: Some(std),
            ..Default::default()
        },
    );
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(
        paths(&l),
        [
            "std/prelude/a_core",
            "std/prelude/b_array",
            "main",
            "std/math",
            "std/net"
        ]
    );
    assert_eq!(l.root, 2);
    assert_eq!(
        imports(&l, "std/net"),
        [("../math".into(), "std/math".into())]
    );
    assert_eq!(
        imports(&l, "std/prelude/a_core"),
        [("velt:math".into(), "std/math".into())]
    );
}

#[test]
fn std_import_without_std_root() {
    let t = Tree::new();
    let root = t.write(
        "main.vlt",
        "import { f } from \"velt:fs\";\nfunction main() {}\n",
    );
    let (_, diags, _) = load(&root, LoadOptions::default());
    assert_eq!(messages(&diags), ["cannot find module `velt:fs`"]);
    assert!(diags[0].notes[0].contains("VELT_STD"));
}

pub(super) struct MapResolver(pub(super) BTreeMap<String, PathBuf>);

impl PackageResolver for MapResolver {
    fn dependency_root(&self, _importer: &Path, name: &str) -> Result<PathBuf, String> {
        self.0
            .get(name)
            .cloned()
            .ok_or_else(|| vpm::graph::not_a_dependency(name))
    }
}

#[test]
fn package_imports() {
    let t = Tree::new();
    t.write(
        "json/src/lib.vlt",
        "import { tok } from \"./lexer\";\nexport function parse() {}\n",
    );
    t.write("json/src/lexer.vlt", "export function tok() {}\n");
    let root = t.write(
        "app/src/main.vlt",
        "import { parse } from \"json\";\nimport { tok } from \"json/lexer\";\nimport { y } from \"yaml\";\nfunction main() {}\n",
    );
    let resolver = MapResolver(BTreeMap::from([("json".to_string(), t.path("json"))]));
    let (l, diags, _) = load(
        &root,
        LoadOptions {
            packages: Some(&resolver),
            ..Default::default()
        },
    );
    assert_eq!(paths(&l), ["main", "json", "json/lexer"]);
    assert_eq!(
        imports(&l, "json"),
        [("./lexer".into(), "json/lexer".into())]
    );
    assert_eq!(
        messages(&diags),
        ["package `yaml` is not a dependency (add it with `velt add yaml`)"]
    );
}

#[test]
fn root_source_override_and_bad_specs() {
    let t = Tree::new();
    t.write("dir/math.test.vlt", "export function test_add() {}\n");
    let virtual_root = t.path("dir/__harness.vlt");
    let src = "import { test_add } from \"./math.test\";\nimport { q } from \"./q.vlt\";\nfunction main() {}\n".to_string();
    let (l, diags, _) = load(
        &virtual_root,
        LoadOptions {
            root_source: Some(src),
            ..Default::default()
        },
    );
    assert_eq!(paths(&l), ["main", "math.test"]);
    assert_eq!(messages(&diags), ["cannot find module `./q.vlt`"]);
}

#[test]
fn missing_root_file() {
    let mut sm = SourceMap::new();
    let mut diags = vec![];
    let err = load_program(
        &mut sm,
        Path::new("definitely/missing.vlt"),
        LoadOptions::default(),
        &mut diags,
    )
    .err()
    .unwrap();
    assert!(err.contains("cannot read"), "{err}");
}

#[test]
fn overlay_wins_over_disk_and_supplies_unsaved_files() {
    let t = Tree::new();
    let root = t.write("main.vlt", "function main() {}\n");
    t.write("a.vlt", "export function old() {}\n");
    let overlay = HashMap::from([
        (
            root.clone(),
            "import { a } from \"./a\";\nimport { n } from \"./new\";\nfunction main() {}\n".into(),
        ),
        (t.path("a.vlt"), "export function a() {}\n".into()),
        (t.path("new.vlt"), "export function n() {}\n".into()),
    ]);
    let (l, diags, sm) = load(
        &root,
        LoadOptions {
            overlay: Some(&overlay),
            ..Default::default()
        },
    );
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(paths(&l), ["main", "a", "new"]);
    assert!(sm.get(l.modules[1].file).src.contains("function a()"));
}

#[test]
fn folder_modules_load_index_velt() {
    let t = Tree::new();
    t.write(
        "app/shapes/index.vlt",
        "export { square } from \"./square\";\n",
    );
    t.write("app/shapes/square.vlt", "export function square() {}\n");
    let root = t.write(
        "app/main.vlt",
        "import { square } from \"./shapes\";\nexport { square as sq };\nfunction main() {}\n",
    );
    let (l, diags, _) = load(&root, LoadOptions::default());
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(paths(&l), ["main", "shapes", "shapes/square"]);
    assert_eq!(imports(&l, "main"), [("./shapes".into(), "shapes".into())]);

    let missing = t.write("other/main.vlt", "import { x } from \"./gone\";\n");
    let (_, diags, _) = load(&missing, LoadOptions::default());
    assert!(
        diags[0].notes.iter().any(|n| n.contains("index.vlt")),
        "{:?}",
        diags[0]
    );
}

/// Aliases `@app/*` → `<root>/src/*` for every importer; no dependencies.
pub(super) struct AliasResolver(pub(super) PathBuf);

impl PackageResolver for AliasResolver {
    fn dependency_root(&self, _importer: &Path, name: &str) -> Result<PathBuf, String> {
        Err(vpm::graph::not_a_dependency(name))
    }

    fn path_alias(&self, _importer: &Path, spec: &str) -> Option<PathBuf> {
        let rest = spec.strip_prefix("@app/")?;
        Some(self.0.join("src").join(rest))
    }
}

#[test]
fn path_aliases_resolve_like_relative_imports() {
    let t = Tree::new();
    t.write("p/src/util/strings.vlt", "export function up() {}\n");
    t.write("p/src/ui/index.vlt", "export function draw() {}\n");
    let root = t.write(
        "p/src/main.vlt",
        "import { up } from \"@app/util/strings\";\nimport { draw } from \"@app/ui\";\nimport { x } from \"@other/x\";\nfunction main() {}\n",
    );
    let resolver = AliasResolver(t.path("p"));
    let (l, diags, _) = load(
        &root,
        LoadOptions {
            packages: Some(&resolver),
            ..Default::default()
        },
    );
    assert_eq!(paths(&l), ["main", "util/strings", "ui"]);
    assert_eq!(
        messages(&diags),
        ["invalid package name `@other` in module specifier `@other/x`"]
    );
}

pub(super) fn jsx_runtime<'l>(l: &'l Loaded, module: &str) -> Option<&'l str> {
    l.modules
        .iter()
        .find(|m| m.path == module)
        .and_then(|m| m.jsx_runtime.as_deref())
}

#[test]
fn jsx_runtime_from_the_pragma() {
    let t = Tree::new();
    t.write("app/ui/jsx-runtime.vlt", "export class Element {}\n");
    let root = t.write(
        "app/main.vlt",
        "// @jsxImportSource ./ui\nimport { f } from \"./plain\";\nfunction main() { const e = <p />; }\n",
    );
    t.write(
        "app/plain.vlt",
        "// @jsxImportSource ./nowhere\nexport function f(a: i64, b: i64): bool { return a < b; }\n",
    );
    let (l, diags, _) = load(&root, LoadOptions::default());
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(paths(&l), ["main", "plain", "ui/jsx-runtime"]);
    assert_eq!(jsx_runtime(&l, "main"), Some("ui/jsx-runtime"));
    assert_eq!(jsx_runtime(&l, "plain"), None, "no JSX, no runtime");
}

/// Every importer's package has `jsx: { importSource: "<0>" }`.
pub(super) struct JsxResolver(pub(super) String);

impl PackageResolver for JsxResolver {
    fn dependency_root(&self, _importer: &Path, name: &str) -> Result<PathBuf, String> {
        Err(vpm::graph::not_a_dependency(name))
    }

    fn jsx_import_source(&self, _importer: &Path) -> Option<String> {
        Some(self.0.clone())
    }
}

#[test]
fn jsx_runtime_from_the_package_unless_the_file_names_one() {
    let t = Tree::new();
    t.write("app/pkg-ui/jsx-runtime.vlt", "export class Element {}\n");
    t.write("app/own/jsx-runtime.vlt", "export class Element {}\n");
    let root = t.write(
        "app/main.vlt",
        "import { g } from \"./other\";\nfunction main() { const e = <></>; }\n",
    );
    t.write(
        "app/other.vlt",
        "// @jsxImportSource ./own\nexport function g() { const e = <b />; }\n",
    );
    let resolver = JsxResolver("./pkg-ui".into());
    let (l, diags, _) = load(
        &root,
        LoadOptions {
            packages: Some(&resolver),
            ..Default::default()
        },
    );
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(jsx_runtime(&l, "main"), Some("pkg-ui/jsx-runtime"));
    assert_eq!(jsx_runtime(&l, "other"), Some("own/jsx-runtime"));
}

#[test]
fn missing_jsx_runtime_says_where_it_came_from() {
    let t = Tree::new();
    let root = t.write("app/main.vlt", "function main() { const e = <p />; }\n");
    let (l, diags, _) = load(&root, LoadOptions::default());
    assert_eq!(
        messages(&diags),
        ["cannot find module `velt:jsx/jsx-runtime`"]
    );
    let note = diags[0].notes.last().unwrap();
    assert!(note.contains("from the default"), "{note}");
    assert_eq!(jsx_runtime(&l, "main"), None);
}

/// Loads `p/main.vlt` (with `main`) against the std root `std/` of `t`.
fn load_with_std(t: &Tree, main: &str) -> (Loaded, Diagnostics) {
    let root = t.write("p/main.vlt", main);
    let (l, d, _) = load(
        &root,
        LoadOptions {
            std_root: Some(t.path("std")),
            ..Default::default()
        },
    );
    (l, d)
}

#[test]
fn only_files_below_the_std_root_are_std() {
    let t = Tree::new();
    t.write("std/math.vlt", "export function one(): i64 { return 1; }\n");
    t.write(
        "std/escape.vlt",
        "import { x } from \"../outside\";\nexport function y() {}\n",
    );
    t.write("outside.vlt", "export function x() {}\n");
    // std modules are std; the root and its own modules are not.
    let (l, d) = load_with_std(
        &t,
        "import { one } from \"velt:math\";\nfunction main() {}\n",
    );
    assert!(d.is_empty(), "{:?}", messages(&d));
    let std_flags: Vec<(&str, bool)> = l
        .modules
        .iter()
        .map(|m| (m.path.as_str(), m.is_std))
        .collect();
    assert_eq!(std_flags, [("main", false), ("std/math", true)]);
    // A std module whose relative import leaves the std root.
    let (_, d) = load_with_std(
        &t,
        "import { y } from \"velt:escape\";\nfunction main() {}\n",
    );
    assert_eq!(
        messages(&d),
        ["module `../outside` resolves to a file outside the standard library"]
    );
    // A user's relative import of a std file.
    let (_, d) = load_with_std(
        &t,
        "import { one } from \"../std/math\";\nfunction main() {}\n",
    );
    assert_eq!(
        messages(&d),
        ["module `../std/math` is a file of the standard library: import it as `velt:math`"]
    );
}

/// On Windows, `\` and drive letters would let a `velt:` path leave the std root; the specifier is
/// rejected before any file is looked up.
#[cfg(windows)]
#[test]
fn windows_std_specifiers_cannot_leave_the_std_root() {
    let t = Tree::new();
    t.write("std/math.vlt", "export function one(): i64 { return 1; }\n");
    let evil = t.write("evil.vlt", "export function x() {}\n");
    let absolute = evil
        .with_extension("")
        .display()
        .to_string()
        .replace('\\', "/");
    for spec in [
        r"velt:..\evil".to_string(),
        r"velt:..\std\..\evil".to_string(),
        format!("velt:{absolute}"),
    ] {
        // In the source text a `\` is written `\\`.
        let quoted = spec.replace('\\', r"\\");
        let main = format!("import {{ x }} from \"{quoted}\";\nfunction main() {{}}\n");
        let (l, d) = load_with_std(&t, &main);
        assert!(
            messages(&d)
                .iter()
                .any(|m| m.contains("invalid standard library module")),
            "{spec}: {:?}",
            messages(&d)
        );
        assert!(l.modules.iter().all(|m| m.path == "main"), "{spec}");
    }
}

#[test]
fn extra_roots_load_once_after_the_root_program() {
    let t = Tree::new();
    let root = t.write(
        "pkg/src/main.vlt",
        "import { a } from \"./a\";\nfunction main() {}\n",
    );
    let a = t.write("pkg/src/a.vlt", "export function a() {}\n");
    let lib = t.write(
        "pkg/src/lib.vlt",
        "import { a } from \"./a\";\nimport { h } from \"./util/h\";\n",
    );
    t.write("pkg/src/util/h.vlt", "export function h() {}\n");
    let test = t.write(
        "pkg/tests/lib.test.vlt",
        "import { h } from \"../src/util/h\";\n",
    );
    let main = t.write("pkg/tests/main.vlt", "");
    let mut sm = SourceMap::new();
    let mut diags = vec![];
    let extra = [a, lib, test, root.clone(), main];
    let l = load_with_roots(&mut sm, &root, &extra, LoadOptions::default(), &mut diags).unwrap();
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(
        paths(&l),
        [
            "main",
            "a",
            "lib",
            "util/h",
            "../tests/lib.test",
            "../tests/main"
        ]
    );
    assert_eq!(imports(&l, "../tests/lib.test")[0].1, "util/h");
}

#[test]
fn extra_roots_with_taken_or_reserved_paths_get_fallback_names() {
    let t = Tree::new();
    t.write("package.vlt", "");
    let root = t.write("src/app.vlt", "function main() {}\n");
    let main = t.write("src/main.vlt", "");
    let std = t.write("src/std/x.vlt", "");
    let b = t.write(
        "src/b.vlt",
        "import { f } from \"./main\";\nimport { g } from \"./main/index\";\n",
    );
    t.write("src/main/index.vlt", "");
    let mut sm = SourceMap::new();
    let mut diags = vec![];
    let extra = [main.clone(), std, b];
    let l = load_with_roots(&mut sm, &root, &extra, LoadOptions::default(), &mut diags).unwrap();
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(paths(&l), ["main", "#main", "#std/x", "b", "#main#2"]);
    // The relative import finds the extra root by its file, under its fallback name.
    let b_imports: Vec<String> = imports(&l, "b").into_iter().map(|(_, p)| p).collect();
    assert_eq!(b_imports, ["#main", "#main#2"]);
    // Diagnostics show the file, not the name.
    assert_eq!(sm.get(l.modules[1].file).path, main);
}

#[test]
fn unreadable_extra_roots_are_reported_in_their_file() {
    let t = Tree::new();
    t.write("package.vlt", "");
    let root = t.write("src/main.vlt", "function main() {}\n");
    let gone = t.path("src/gone.vlt");
    let mut sm = SourceMap::new();
    let mut diags = vec![];
    let extra = [gone.clone()];
    let l = load_with_roots(&mut sm, &root, &extra, LoadOptions::default(), &mut diags).unwrap();
    assert_eq!(paths(&l), ["main"]);
    let msgs = messages(&diags);
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(
        msgs[0].starts_with("cannot read `src/gone.vlt`: "),
        "{msgs:?}"
    );
    let span = diags[0].labels[0].span;
    assert_eq!(sm.get(span.file).path, gone);
}
