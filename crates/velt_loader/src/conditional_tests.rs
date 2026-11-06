//! `#name` imports ([`super::conditional`]) and the comparison of their variants
//! ([`super::variants`]).

use std::collections::BTreeMap;

use vpm::imports::{ImportMap, Variant};

use super::tests::{load, messages, paths, Tree};
use super::*;

fn map(entries: &[(&str, &[(&str, &str)])]) -> ImportMap {
    entries
        .iter()
        .map(|(name, variants)| {
            let variants = variants
                .iter()
                .map(|(c, t)| Variant {
                    condition: c.to_string(),
                    target: t.to_string(),
                })
                .collect();
            (name.to_string(), variants)
        })
        .collect()
}

/// Package `app` in `app/`, with `imports` and the dependency `lib` (in `lib/`, no imports).
fn graph(t: &Tree, imports: ImportMap) -> vpm::PackageGraph {
    let mut g = vpm::PackageGraph::default();
    let deps = BTreeMap::from([("lib".to_string(), t.path("lib"))]);
    g.add("app", &t.path("app"), deps).imports = imports;
    g.add("lib", &t.path("lib"), BTreeMap::new());
    g
}

const IO: &[(&str, &str)] = &[
    ("js", "./src/io/js.velt"),
    ("wasi", "./src/io/wasi.velt"),
    ("default", "./src/io/native.velt"),
];

fn io_package(t: &Tree) -> PathBuf {
    for v in ["js", "wasi", "native"] {
        t.write(
            &format!("app/src/io/{v}.velt"),
            &format!("export function name(): string {{ return \"{v}\"; }}\n"),
        );
    }
    t.write(
        "app/src/main.velt",
        "import { name } from \"#io\";\nfunction main() { name(); }\n",
    )
}

fn opts<'a>(g: &'a vpm::PackageGraph, target: &str) -> LoadOptions<'a> {
    LoadOptions {
        packages: Some(g),
        target: Some(target.into()),
        ..Default::default()
    }
}

#[test]
fn the_build_conditions_select_the_variant() {
    let t = Tree::new();
    let root = io_package(&t);
    let g = graph(&t, map(&[("#io", IO)]));
    for (target, module) in [
        ("x86_64-unknown-linux-gnu", "io/native"),
        ("aarch64-apple-darwin", "io/native"),
        ("wasm32-wasip1", "io/wasi"),
        ("wasm32-unknown-unknown", "io/js"),
    ] {
        let (l, d, _) = load(&root, opts(&g, target));
        assert!(d.is_empty(), "{target}: {:?}", messages(&d));
        assert_eq!(paths(&l), ["main", module], "{target}");
    }
    // The language server's resolution agrees.
    let found = resolve_module("#io", &root, None, Some(&g), Some("wasm32-wasip1"));
    assert_eq!(found.map(|f| f.ends_with("src/io/wasi.velt")), Some(true));
}

#[test]
fn a_build_no_variant_names_without_default_is_an_error() {
    let t = Tree::new();
    let root = io_package(&t);
    let g = graph(&t, map(&[("#io", &[("js", "./src/io/js.velt")])]));
    let (_, d, _) = load(&root, opts(&g, "x86_64-unknown-linux-gnu"));
    assert_eq!(
        messages(&d),
        ["`#io` of package `app` has no variant for this build (it has js; the build: native, linux)"]
    );
    assert!(
        d[0].notes[0].contains("add a `default` variant"),
        "{:?}",
        d[0].notes
    );
    let (_, d, _) = load(&root, opts(&g, "wasm32-unknown-unknown"));
    assert!(d.is_empty(), "{:?}", messages(&d));
}

#[test]
fn a_name_resolves_only_in_the_package_that_defines_it() {
    let t = Tree::new();
    io_package(&t);
    // `lib` imports app's `#io`: not its own.
    t.write(
        "lib/src/lib.velt",
        "import { name } from \"#io\";\nexport function f() {}\n",
    );
    let root = t.write(
        "app/src/main.velt",
        "import { f } from \"lib\";\nimport { x } from \"#nope\";\nfunction main() {}\n",
    );
    let g = graph(&t, map(&[("#io", IO)]));
    let (_, d, _) = load(&root, opts(&g, "aarch64-apple-darwin"));
    assert_eq!(
        messages(&d),
        [
            "package `app` has no conditional import `#nope`",
            "package `lib` has no conditional import `#io`",
        ]
    );
    // Outside every package.
    let lone = t.write(
        "lone/main.velt",
        "import { x } from \"#io\";\nfunction main() {}\n",
    );
    let (_, d, _) = load(&lone, LoadOptions::default());
    assert_eq!(d.len(), 1);
    assert!(
        d[0].message.contains("is in no package"),
        "{}",
        d[0].message
    );
}

fn check<'a>(g: &'a vpm::PackageGraph) -> LoadOptions<'a> {
    LoadOptions {
        packages: Some(g),
        target: Some("aarch64-apple-darwin".into()),
        check_variants: true,
        ..Default::default()
    }
}

#[test]
fn variants_must_export_the_same_api() {
    let t = Tree::new();
    let root = io_package(&t);
    t.write(
        "app/src/io/js.velt",
        "export function name(): string { return \"js\"; }\nexport function extra() {}\n",
    );
    t.write(
        "app/src/io/wasi.velt",
        "export function name(n: i64): string { return \"wasi\"; }\n",
    );
    let g = graph(&t, map(&[("#io", IO)]));
    // Not asked for (a build that runs): no comparison.
    let (_, d, _) = load(&root, opts(&g, "aarch64-apple-darwin"));
    assert!(d.is_empty(), "{:?}", messages(&d));
    let (_, d, sm) = load(&root, check(&g));
    assert_eq!(
        messages(&d),
        [
            "`#io`: `extra` is exported by `./src/io/js.velt` but not by `./src/io/wasi.velt`",
            "`#io`: `name` has another signature in `./src/io/wasi.velt` than in `./src/io/js.velt`",
            "`#io`: `extra` is exported by `./src/io/js.velt` but not by `./src/io/native.velt`",
        ]
    );
    // Located at the export, with the reference's signature beside it.
    let rendered = d[1].render(&sm);
    assert!(rendered.contains("wasi.velt:1:17"), "{rendered}");
    assert!(rendered.contains("js.velt"), "{rendered}");
    assert!(
        d[1].notes
            .iter()
            .any(|n| n == "`./src/io/wasi.velt`: function (n:i64):string"),
        "{:?}",
        d[1].notes
    );
}

#[test]
fn variants_that_agree_pass_through_layout_classes_and_re_exports() {
    let t = Tree::new();
    let root = io_package(&t);
    let class = |name: &str, extra: &str| {
        format!(
            "export class Conn {{\n  readonly host: string;\n  private fd: i64 = 0;{extra}\n  constructor(host: string) {{ this.host = host; }}\n  send(data: string): i64 {{ return 0; }}\n}}\nexport function name(): string {{ return \"{name}\"; }}\nexport const LIMIT = {};\n",
            name.len()
        )
    };
    t.write("app/src/io/native.velt", &class("native", ""));
    // Another private field, another layout and comments: the same API.
    t.write(
        "app/src/io/js.velt",
        &class("js", "\n  private handle: i64 = 1; // the host's")
            .replace("send(data: string)", "send( data : string /* utf-8 */ )"),
    );
    // Re-exported from a file of its own.
    t.write("app/src/io/wasi/conn.velt", &class("wasi", ""));
    t.write(
        "app/src/io/wasi.velt",
        "export { Conn, name, LIMIT } from \"./wasi/conn\";\n",
    );
    let g = graph(&t, map(&[("#io", IO)]));
    let (_, d, _) = load(&root, check(&g));
    assert!(d.is_empty(), "{:?}", messages(&d));
    // A public member that differs does count.
    t.write(
        "app/src/io/js.velt",
        &class("js", "").replace("send(data: string): i64", "send(data: string): i32"),
    );
    let (_, d, _) = load(&root, check(&g));
    assert_eq!(
        messages(&d),
        [
            "`#io`: `Conn` has another signature in `./src/io/wasi.velt` than in `./src/io/js.velt`",
            "`#io`: `Conn` has another signature in `./src/io/native.velt` than in `./src/io/js.velt`",
        ]
    );
}

/// A std root whose `imports.velt` maps `#t` to internal files of `velt/x`, which `velt/x`
/// imports.
fn std_with_imports(t: &Tree, imports: &str) -> PathBuf {
    t.write("std/imports.velt", imports);
    t.write("std/x/index.velt", "export { t } from \"./api\";\n");
    t.write(
        "std/x/api.velt",
        "import { t } from \"#t\";\nexport { t };\n",
    );
    t.write(
        "std/x/t/native.velt",
        "export function t(): i64 { return 1; }\n",
    );
    t.write("std/x/t/js.velt", "export function t(): i64 { return 2; }\n");
    t.path("std")
}

const STD_IMPORTS: &str = "import type { Imports } from \"velt/package\";\n\nexport const imports: Imports = {\n  \"#t\": { js: \"./x/t/js.velt\", default: \"./x/t/native.velt\" },\n};\n";

#[test]
fn std_has_conditional_imports_of_its_own() {
    let t = Tree::new();
    let std = std_with_imports(&t, STD_IMPORTS);
    let root = t.write(
        "main.velt",
        "import { t } from \"velt/x\";\nfunction main() { t(); }\n",
    );
    let std_opts = |target: &str| LoadOptions {
        std_root: Some(std.clone()),
        target: Some(target.into()),
        check_variants: true,
        ..Default::default()
    };
    for (target, variant) in [
        ("x86_64-pc-windows-msvc", "std/x/t/native"),
        ("wasm32-unknown-unknown", "std/x/t/js"),
    ] {
        let (l, d, _) = load(&root, std_opts(target));
        assert!(d.is_empty(), "{:?}", messages(&d));
        assert!(paths(&l).contains(&variant), "{:?}", paths(&l));
    }
    // A program cannot name std's `#t`, nor import its variants.
    let user = t.write(
        "user.velt",
        "import { t } from \"#t\";\nimport { t as u } from \"velt/x/t/js\";\nfunction main() {}\n",
    );
    let mut with_package = vpm::PackageGraph::default();
    with_package.add("user", &t.path(""), BTreeMap::new());
    let (_, d, _) = load(
        &user,
        LoadOptions {
            packages: Some(&with_package),
            ..std_opts("aarch64-apple-darwin")
        },
    );
    assert_eq!(d.len(), 2, "{:?}", messages(&d));
    assert_eq!(
        d[0].message,
        "package `user` has no conditional import `#t`"
    );
    assert!(d[0]
        .notes
        .iter()
        .any(|n| n.contains("the standard library's own")));
    assert!(
        d[1].message.contains("internal to `velt/x`"),
        "{}",
        d[1].message
    );
}

#[test]
fn std_variants_are_internal_files_and_its_map_is_checked() {
    let t = Tree::new();
    let root = t.write(
        "main.velt",
        "import { t } from \"velt/x\";\nfunction main() { t(); }\n",
    );
    // A module as a variant.
    let std = std_with_imports(
        &t,
        &STD_IMPORTS.replace("./x/t/native.velt", "./x/index.velt"),
    );
    let o = || LoadOptions {
        std_root: Some(std.clone()),
        target: Some("x86_64-unknown-linux-gnu".into()),
        ..Default::default()
    };
    let (_, d, _) = load(&root, o());
    assert_eq!(
        messages(&d),
        ["`#t` names `x/index.velt`, a module of the standard library"]
    );
    // A condition no build has, reported in std/imports.velt.
    t.write("std/imports.velt", &STD_IMPORTS.replace("js:", "node:"));
    let (_, d, sm) = load(&root, o());
    assert!(
        d[0].message.starts_with("unknown condition `node`"),
        "{:?}",
        messages(&d)
    );
    assert!(
        d[0].render(&sm).contains("imports.velt:4"),
        "{}",
        d[0].render(&sm)
    );
}

/// An npm package's `#name` resolves through its own `package.json` `imports`, as in Node, never
/// through the map of the Velt package around it.
#[test]
fn an_npm_package_resolves_its_own_names() {
    let t = Tree::new();
    t.write(
        "app/src/util.velt",
        "export function u(): i64 { return 1; }\n",
    );
    t.write(
        "app/node_modules/foo/package.json",
        r##"{ "name": "foo", "exports": "./dist/index.js", "imports": { "#util": "./dist/util.js", "#dep": "other-package" } }"##,
    );
    t.write(
        "app/node_modules/foo/src/index.ts",
        "import { u } from \"#util\";\nexport function f(): number { return u(); }\n",
    );
    t.write(
        "app/node_modules/foo/src/util.ts",
        "export function u(): number { return 2; }\n",
    );
    let root = t.write(
        "app/src/main.velt",
        "import { f } from \"foo\";\nimport { u } from \"#util\";\nfunction main() { f(); u(); }\n",
    );
    let g = graph(&t, map(&[("#util", &[("default", "./src/util.velt")])]));
    let (l, d, _) = load(&root, opts(&g, "aarch64-apple-darwin"));
    assert!(d.is_empty(), "{:?}", messages(&d));
    let files: Vec<String> = l.modules.iter().map(|m| m.path.clone()).collect();
    assert!(
        files.iter().any(|p| p.ends_with("foo/src/util")),
        "{files:?}"
    );
    assert!(files.iter().any(|p| p == "util"), "{files:?}");
    // A name the npm package does not define is its error, not a lookup in app's map.
    t.write(
        "app/node_modules/foo/src/index.ts",
        "import { u } from \"#dep\";\nexport function f(): number { return 1; }\n",
    );
    let (_, d, _) = load(&root, opts(&g, "aarch64-apple-darwin"));
    assert_eq!(
        messages(&d),
        ["npm package `foo` maps `#dep` to the package `other-package` (`imports` in its package.json), which Velt does not follow"]
    );
}

/// A Velt program at the root of an npm workspace (sigx's layout): a workspace package below it
/// (`pnpm-workspace.yaml` `packages/*`, or a `workspaces` field) resolves its own `#name`s through
/// its `package.json`, as Node does, whether or not it names itself.
#[test]
fn a_workspace_package_resolves_its_own_names() {
    for (named, listing) in [(true, "pnpm"), (false, "pnpm"), (false, "npm")] {
        let t = Tree::new();
        t.write(
            "app/src/util.velt",
            "export function u(): string { return \"app\"; }\n",
        );
        match listing {
            "pnpm" => t.write("app/pnpm-workspace.yaml", "packages:\n  - packages/*\n"),
            _ => t.write(
                "app/package.json",
                r#"{ "private": true, "workspaces": ["packages/*"] }"#,
            ),
        };
        let name = if named {
            r#""name": "foo", "exports": "./dist/index.js", "#
        } else {
            ""
        };
        t.write(
            "app/packages/foo/package.json",
            &format!(r##"{{ {name}"imports": {{ "#util": "./dist/util.js" }} }}"##),
        );
        t.write(
            "app/packages/foo/src/index.ts",
            "import { u } from \"#util\";\nexport function f(): number { return u(); }\n",
        );
        t.write(
            "app/packages/foo/src/util.ts",
            "export function u(): number { return 2; }\n",
        );
        let root = t.write(
            "app/src/main.velt",
            "import { f } from \"../packages/foo/src/index\";\nimport { u } from \"#util\";\nfunction main() { f(); u(); }\n",
        );
        let g = graph(&t, map(&[("#util", &[("default", "./src/util.velt")])]));
        let (l, d, _) = load(&root, opts(&g, "aarch64-apple-darwin"));
        assert!(d.is_empty(), "{named} {listing}: {:?}", messages(&d));
        let files: Vec<&str> = paths(&l);
        assert!(
            files.iter().any(|p| p.ends_with("foo/src/util")),
            "{named} {listing}: {files:?}"
        );
        assert!(files.contains(&"util"), "{files:?}");
    }
}

/// A file under a nameless folder `package.json` inside a workspace npm package belongs to that
/// package: its `#name`s are the npm package's business (Node: the folder file is its scope,
/// with no `imports`, so an error), never the Velt root's map.
#[test]
fn a_folder_package_json_inside_an_npm_package_is_not_the_velt_package() {
    let t = Tree::new();
    t.write(
        "app/src/util.velt",
        "export function u(): string { return \"app\"; }\n",
    );
    t.write("app/pnpm-workspace.yaml", "packages:\n  - packages/*\n");
    t.write(
        "app/packages/foo/package.json",
        r##"{ "name": "foo", "exports": "./dist/index.js", "imports": { "#util": "./dist/util.js" } }"##,
    );
    t.write(
        "app/packages/foo/src/esm/package.json",
        r#"{ "type": "module" }"#,
    );
    t.write(
        "app/packages/foo/src/esm/inner.ts",
        "import { u } from \"#util\";\nexport function v(): number { return u(); }\n",
    );
    let root = t.write(
        "app/src/main.velt",
        "import { v } from \"../packages/foo/src/esm/inner\";\nfunction main() { v(); }\n",
    );
    let g = graph(&t, map(&[("#util", &[("default", "./src/util.velt")])]));
    let (_, d, _) = load(&root, opts(&g, "aarch64-apple-darwin"));
    assert_eq!(d.len(), 1, "{:?}", messages(&d));
    assert!(
        d[0].message.contains("does not define `#util`"),
        "{}",
        d[0].message
    );
}

/// A `package.json` inside a Velt package that is not an npm package's (a folder's
/// `{ "type": "module" }`, the root's for TypeScript tools) leaves its files' `#name`s to
/// `package.velt`.
#[test]
fn a_package_json_in_a_velt_package_is_not_an_npm_scope() {
    let t = Tree::new();
    let root = io_package(&t);
    t.write("app/package.json", r#"{ "name": "app", "type": "module" }"#);
    t.write("app/src/sub/package.json", r#"{ "type": "module" }"#);
    t.write(
        "app/src/sub/w.velt",
        "import { name } from \"#io\";\nexport function w(): string { return name(); }\n",
    );
    t.write(
        "app/src/main.velt",
        "import { name } from \"#io\";\nimport { w } from \"./sub/w\";\nfunction main() { name(); w(); }\n",
    );
    let g = graph(&t, map(&[("#io", IO)]));
    let (l, d, _) = load(&root, opts(&g, "aarch64-apple-darwin"));
    assert!(d.is_empty(), "{:?}", messages(&d));
    assert!(paths(&l).contains(&"io/native"), "{:?}", paths(&l));
}

/// The variant check reads each file once and resolves each (file, name) once: many names
/// through two files that re-export from each other, entered at either one. Whichever variant
/// is read first, equal APIs compare equal.
#[test]
fn re_export_cycles_and_many_names_are_read_once() {
    let t = Tree::new();
    let root = io_package(&t);
    let (a, b): (Vec<String>, Vec<String>) = (
        (0..200).map(|i| format!("a{i}")).collect(),
        (0..200).map(|i| format!("b{i}")).collect(),
    );
    let decls = |names: &[String]| -> String {
        names
            .iter()
            .map(|n| format!("export function {n}(x: i64): i64 {{ return x; }}\n"))
            .collect()
    };
    let all = format!("{}, {}", a.join(", "), b.join(", "));
    // `c` declares the b's and takes the a's from `d`, which declares the a's and takes the
    // b's from `c`.
    t.write(
        "app/src/io/c.velt",
        &format!("export {{ {} }} from \"./d\";\n{}", a.join(", "), decls(&b)),
    );
    t.write(
        "app/src/io/d.velt",
        &format!("export {{ {} }} from \"./c\";\n{}", b.join(", "), decls(&a)),
    );
    let tail = "export function name(): string { return \"\"; }\n";
    t.write(
        "app/src/io/js.velt",
        &format!("export {{ {all} }} from \"./c\";\n{tail}"),
    );
    t.write(
        "app/src/io/wasi.velt",
        &format!("export {{ {all} }} from \"./d\";\n{tail}"),
    );
    t.write(
        "app/src/io/native.velt",
        &format!("{}{}{tail}", decls(&a), decls(&b)),
    );
    let g = graph(&t, map(&[("#io", IO)]));
    let start = std::time::Instant::now();
    let (_, d, sm) = load(&root, check(&g));
    assert!(d.is_empty(), "{:?}", &messages(&d)[..d.len().min(3)]);
    assert!(start.elapsed() < std::time::Duration::from_secs(20));
    // Each of c and d was parsed once.
    let parsed = |name: &str| sm.files().filter(|(_, f)| f.path.ends_with(name)).count();
    assert_eq!((parsed("c.velt"), parsed("d.velt")), (1, 1));
}

/// A name that a re-export chain cannot pin down is an error, not a match by specifier: a cycle
/// that declares it nowhere.
#[test]
fn names_that_cannot_be_resolved_are_errors() {
    let t = Tree::new();
    let root = io_package(&t);
    let tail = "export function name(): string { return \"\"; }\n";
    t.write("app/src/io/c.velt", "export { f } from \"./e\";\n");
    t.write("app/src/io/e.velt", "export { f } from \"./c\";\n");
    t.write(
        "app/src/io/js.velt",
        &format!("export {{ f }} from \"./c\";\n{tail}"),
    );
    t.write(
        "app/src/io/wasi.velt",
        &format!("export function f(): i64 {{ return 1; }}\n{tail}"),
    );
    t.write(
        "app/src/io/native.velt",
        &format!("export function f(): i64 {{ return 1; }}\n{tail}"),
    );
    let g = graph(&t, map(&[("#io", IO)]));
    let (_, d, _) = load(&root, check(&g));
    assert_eq!(
        messages(&d),
        ["`#io`: `f` of `./src/io/js.velt` cannot be compared: `f` is re-exported in a cycle through `src/io/c.velt` and declared nowhere on it"]
    );
}

/// `export *` is expanded through the files it names: variants that each re-export a common
/// file agree; what one of them brings in through nested stars is compared like any name; a
/// name two stars bring in with different signatures, or a star from outside the package, is
/// an error.
#[test]
fn export_star_variants_are_expanded() {
    let t = Tree::new();
    let root = io_package(&t);
    let common = "export function shared(x: i64): i64 { return x; }\n";
    t.write("app/src/io/common.velt", common);
    t.write(
        "app/src/io/nested.velt",
        "export * from \"./common\";\nexport * from \"./nested\";\n",
    );
    for (v, from) in [
        ("native", "./common"),
        ("js", "./nested"),
        ("wasi", "./common"),
    ] {
        t.write(
            &format!("app/src/io/{v}.velt"),
            &format!(
                "export * from \"{from}\";\nexport function name(): string {{ return \"{v}\"; }}\n"
            ),
        );
    }
    let g = graph(&t, map(&[("#io", IO)]));
    let (_, d, _) = load(&root, check(&g));
    assert!(d.is_empty(), "{:?}", messages(&d));
    // A difference behind a star is found.
    t.write(
        "app/src/io/other.velt",
        "export function shared(x: f64): f64 { return x; }\n",
    );
    t.write(
        "app/src/io/wasi.velt",
        "export * from \"./other\";\nexport function name(): string { return \"\"; }\n",
    );
    let (_, d, _) = load(&root, check(&g));
    assert_eq!(
        messages(&d),
        ["`#io`: `shared` has another signature in `./src/io/wasi.velt` than in `./src/io/js.velt`"]
    );
    // Two stars bringing in one name with different signatures: ambiguous.
    t.write(
        "app/src/io/wasi.velt",
        "export * from \"./other\";\nexport * from \"./common\";\nexport function name(): string { return \"\"; }\n",
    );
    let (_, d, _) = load(&root, check(&g));
    assert!(
        d.iter()
            .any(|d| d.message.contains("`shared` is ambiguous")),
        "{:?}",
        messages(&d)
    );
    // A star from outside the package cannot be listed.
    t.write(
        "app/src/io/wasi.velt",
        "export * from \"lib\";\nexport function name(): string { return \"\"; }\n",
    );
    let (_, d, _) = load(&root, check(&g));
    assert!(
        d.iter().any(|d| d.message.contains("cannot be listed")),
        "{:?}",
        messages(&d)
    );
}

/// Stars and re-exports that come back to the variant itself: the name resolves through the
/// star that does declare it (ES `resolveSet`), with no endless recursion.
#[test]
fn a_cycle_through_a_star_resolves_through_the_other_star() {
    let t = Tree::new();
    let root = io_package(&t);
    let tail = "export function name(): string { return \"\"; }\n";
    t.write(
        "app/src/io/js.velt",
        &format!("export * from \"./a\";\nexport * from \"./b\";\n{tail}"),
    );
    t.write("app/src/io/a.velt", "export { x } from \"./js\";\n");
    t.write("app/src/io/b.velt", "export const x: i64 = 1;\n");
    t.write(
        "app/src/io/wasi.velt",
        &format!("export const x: i64 = 5;\n{tail}"),
    );
    t.write(
        "app/src/io/native.velt",
        &format!("export const x: i64 = 5;\n{tail}"),
    );
    let g = graph(&t, map(&[("#io", IO)]));
    let (_, d, _) = load(&root, check(&g));
    assert!(d.is_empty(), "{:?}", messages(&d));
    // The same with the variants read in the other order.
    let g = graph(
        &t,
        map(&[(
            "#io",
            &[
                ("wasi", "./src/io/wasi.velt"),
                ("js", "./src/io/js.velt"),
                ("default", "./src/io/native.velt"),
            ],
        )]),
    );
    let (_, d, _) = load(&root, check(&g));
    assert!(d.is_empty(), "{:?}", messages(&d));
}

/// Two stars of one file bringing in a name with different signatures make it ambiguous at any
/// depth (as Node's conflicting star exports), so the check never picks one the build doesn't.
#[test]
fn stars_at_different_depths_that_disagree_are_ambiguous() {
    let t = Tree::new();
    let root = io_package(&t);
    let tail = "export function name(): string { return \"\"; }\n";
    t.write(
        "app/src/io/native.velt",
        &format!("export * from \"./b\";\nexport * from \"./a\";\n{tail}"),
    );
    t.write("app/src/io/a.velt", "export const x: i64 = 1;\n");
    t.write("app/src/io/b.velt", "export * from \"./c\";\n");
    t.write("app/src/io/c.velt", "export const x: string = \"c\";\n");
    t.write(
        "app/src/io/js.velt",
        &format!("export const x: i64 = 1;\n{tail}"),
    );
    t.write(
        "app/src/io/wasi.velt",
        &format!("export const x: i64 = 1;\n{tail}"),
    );
    let g = graph(&t, map(&[("#io", IO)]));
    let (_, d, _) = load(&root, check(&g));
    assert!(
        d.iter().any(|d| d.message.contains("`x` is ambiguous")),
        "{:?}",
        messages(&d)
    );
}

/// A name re-exported under another one compares by what it is, not by its declared name.
#[test]
fn renamed_re_exports_compare_by_what_they_are() {
    let t = Tree::new();
    let root = io_package(&t);
    let tail = "export function name(): string { return \"\"; }\n";
    t.write(
        "app/src/io/m.velt",
        "export function f(): i64 { return 1; }\nexport class K { n: i64 = 0; }\nexport interface I { a: i64; }\n",
    );
    t.write(
        "app/src/io/js.velt",
        &format!("export {{ f as h, K as C, I as J }} from \"./m\";\n{tail}"),
    );
    for v in ["wasi", "native"] {
        t.write(
            &format!("app/src/io/{v}.velt"),
            &format!("export function h(): i64 {{ return 2; }}\nexport class C {{ n: i64 = 1; }}\nexport interface J {{ a: i64; }}\n{tail}"),
        );
    }
    let g = graph(&t, map(&[("#io", IO)]));
    let (_, d, _) = load(&root, check(&g));
    assert!(d.is_empty(), "{:?}", messages(&d));
}

/// `export { a }` of a binding the file imports is compared by where it is declared.
#[test]
fn a_local_export_of_an_import_is_followed() {
    let t = Tree::new();
    let root = io_package(&t);
    let tail = "export function name(): string { return \"\"; }\n";
    t.write(
        "app/src/io/a.velt",
        "export function f(x: i64): i64 { return x; }\n",
    );
    t.write(
        "app/src/io/b.velt",
        "export function f(x: f64): f64 { return x; }\n",
    );
    t.write(
        "app/src/io/js.velt",
        &format!("import {{ f }} from \"./a\";\nexport {{ f }};\n{tail}"),
    );
    t.write(
        "app/src/io/wasi.velt",
        &format!("import {{ f as g }} from \"./a\";\nexport {{ g as f }};\n{tail}"),
    );
    t.write(
        "app/src/io/native.velt",
        &format!("import {{ f }} from \"./b\";\nexport {{ f }};\n{tail}"),
    );
    let g = graph(&t, map(&[("#io", IO)]));
    let (_, d, _) = load(&root, check(&g));
    assert_eq!(
        messages(&d),
        ["`#io`: `f` has another signature in `./src/io/native.velt` than in `./src/io/js.velt`"]
    );
}

/// A variant file that does not exist is the import's error, not a second one from the check.
#[test]
fn a_missing_variant_is_one_error() {
    let t = Tree::new();
    let root = io_package(&t);
    std::fs::remove_file(t.path("app/src/io/native.velt")).unwrap();
    let g = graph(&t, map(&[("#io", IO)]));
    let (_, d, _) = load(&root, check(&g));
    assert_eq!(d.len(), 1, "{:?}", messages(&d));
}
