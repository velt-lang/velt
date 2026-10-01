use std::collections::BTreeMap;

use super::*;

/// A temp dir with helpers to write `.vlt` files.
struct Tree(tempfile::TempDir);

impl Tree {
    fn new() -> Tree {
        Tree(tempfile::tempdir().unwrap())
    }
    fn path(&self, rel: &str) -> PathBuf {
        self.0.path().join(rel)
    }
    fn write(&self, rel: &str, src: &str) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, src).unwrap();
        p
    }
}

fn load(root: &Path, opts: LoadOptions) -> (Loaded, Diagnostics, SourceMap) {
    let mut sm = SourceMap::new();
    let mut diags = vec![];
    let loaded = load_program(&mut sm, root, opts, &mut diags).unwrap();
    (loaded, diags, sm)
}

fn paths(l: &Loaded) -> Vec<&str> {
    l.modules.iter().map(|m| m.path.as_str()).collect()
}

fn imports(l: &Loaded, module: &str) -> Vec<(String, String)> {
    l.modules
        .iter()
        .find(|m| m.path == module)
        .unwrap()
        .imports
        .clone()
}

fn messages(diags: &Diagnostics) -> Vec<String> {
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

struct MapResolver(BTreeMap<String, PathBuf>);

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
    assert_eq!(
        messages(&diags),
        ["module specifier `./q.vlt` should not include the `.vlt` extension"]
    );
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
    assert!(diags[0].notes[1].contains("index.vlt"), "{:?}", diags[0]);
}

/// Aliases `@app/*` → `<root>/src/*` for every importer; no dependencies.
struct AliasResolver(PathBuf);

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

fn jsx_runtime<'l>(l: &'l Loaded, module: &str) -> Option<&'l str> {
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

/// Every importer's package has `[jsx] importSource = "<0>"`.
struct JsxResolver(String);

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
