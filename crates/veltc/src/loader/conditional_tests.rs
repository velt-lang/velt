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
    ("js", "./src/io/js.vlt"),
    ("wasi", "./src/io/wasi.vlt"),
    ("default", "./src/io/native.vlt"),
];

fn io_package(t: &Tree) -> PathBuf {
    for v in ["js", "wasi", "native"] {
        t.write(
            &format!("app/src/io/{v}.vlt"),
            &format!("export function name(): string {{ return \"{v}\"; }}\n"),
        );
    }
    t.write(
        "app/src/main.vlt",
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
    assert_eq!(found.map(|f| f.ends_with("src/io/wasi.vlt")), Some(true));
}

#[test]
fn a_build_no_variant_names_without_default_is_an_error() {
    let t = Tree::new();
    let root = io_package(&t);
    let g = graph(&t, map(&[("#io", &[("js", "./src/io/js.vlt")])]));
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
        "lib/src/lib.vlt",
        "import { name } from \"#io\";\nexport function f() {}\n",
    );
    let root = t.write(
        "app/src/main.vlt",
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
        "lone/main.vlt",
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
        "app/src/io/js.vlt",
        "export function name(): string { return \"js\"; }\nexport function extra() {}\n",
    );
    t.write(
        "app/src/io/wasi.vlt",
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
            "`#io`: `extra` is exported by `./src/io/js.vlt` but not by `./src/io/wasi.vlt`",
            "`#io`: `name` has another signature in `./src/io/wasi.vlt` than in `./src/io/js.vlt`",
            "`#io`: `extra` is exported by `./src/io/js.vlt` but not by `./src/io/native.vlt`",
        ]
    );
    // Located at the export, with the reference's signature beside it.
    let rendered = d[1].render(&sm);
    assert!(rendered.contains("wasi.vlt:1:17"), "{rendered}");
    assert!(rendered.contains("js.vlt"), "{rendered}");
    assert!(
        d[1].notes
            .iter()
            .any(|n| n == "`./src/io/wasi.vlt`: function name(n:i64):string"),
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
    t.write("app/src/io/native.vlt", &class("native", ""));
    // Another private field, another layout and comments: the same API.
    t.write(
        "app/src/io/js.vlt",
        &class("js", "\n  private handle: i64 = 1; // the host's")
            .replace("send(data: string)", "send( data : string /* utf-8 */ )"),
    );
    // Re-exported from a file of its own.
    t.write("app/src/io/wasi/conn.vlt", &class("wasi", ""));
    t.write(
        "app/src/io/wasi.vlt",
        "export { Conn, name, LIMIT } from \"./wasi/conn\";\n",
    );
    let g = graph(&t, map(&[("#io", IO)]));
    let (_, d, _) = load(&root, check(&g));
    assert!(d.is_empty(), "{:?}", messages(&d));
    // A public member that differs does count.
    t.write(
        "app/src/io/js.vlt",
        &class("js", "").replace("send(data: string): i64", "send(data: string): i32"),
    );
    let (_, d, _) = load(&root, check(&g));
    assert_eq!(
        messages(&d),
        [
            "`#io`: `Conn` has another signature in `./src/io/wasi.vlt` than in `./src/io/js.vlt`",
            "`#io`: `Conn` has another signature in `./src/io/native.vlt` than in `./src/io/js.vlt`",
        ]
    );
}

/// A std root whose `imports.vlt` maps `#t` to internal files of `velt/x`, which `velt/x`
/// imports.
fn std_with_imports(t: &Tree, imports: &str) -> PathBuf {
    t.write("std/imports.vlt", imports);
    t.write("std/x/index.vlt", "export { t } from \"./api\";\n");
    t.write(
        "std/x/api.vlt",
        "import { t } from \"#t\";\nexport { t };\n",
    );
    t.write(
        "std/x/t/native.vlt",
        "export function t(): i64 { return 1; }\n",
    );
    t.write("std/x/t/js.vlt", "export function t(): i64 { return 2; }\n");
    t.path("std")
}

const STD_IMPORTS: &str = "import type { Imports } from \"velt/package\";\n\nexport const imports: Imports = {\n  \"#t\": { js: \"./x/t/js.vlt\", default: \"./x/t/native.vlt\" },\n};\n";

#[test]
fn std_has_conditional_imports_of_its_own() {
    let t = Tree::new();
    let std = std_with_imports(&t, STD_IMPORTS);
    let root = t.write(
        "main.vlt",
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
        "user.vlt",
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
        "main.vlt",
        "import { t } from \"velt/x\";\nfunction main() { t(); }\n",
    );
    // A module as a variant.
    let std = std_with_imports(
        &t,
        &STD_IMPORTS.replace("./x/t/native.vlt", "./x/index.vlt"),
    );
    let o = || LoadOptions {
        std_root: Some(std.clone()),
        target: Some("x86_64-unknown-linux-gnu".into()),
        ..Default::default()
    };
    let (_, d, _) = load(&root, o());
    assert_eq!(
        messages(&d),
        ["`#t` names `x/index.vlt`, a module of the standard library"]
    );
    // A condition no build has, reported in std/imports.vlt.
    t.write("std/imports.vlt", &STD_IMPORTS.replace("js:", "node:"));
    let (_, d, sm) = load(&root, o());
    assert!(
        d[0].message.starts_with("unknown condition `node`"),
        "{:?}",
        messages(&d)
    );
    assert!(
        d[0].render(&sm).contains("imports.vlt:4"),
        "{}",
        d[0].render(&sm)
    );
}
