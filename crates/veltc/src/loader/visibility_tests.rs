//! Which std files a module may load ([`super::visibility`]).

use super::tests::{load, messages, paths, Tree};
use super::*;

/// A std root with the module `velt/x` (`x/index.vlt`), its internal file `x/inner.vlt` and a
/// white-box test of it next to them.
fn std_with_a_test(t: &Tree) -> PathBuf {
    t.write("std/x/index.vlt", "export { one } from \"velt/x/inner\";\n");
    t.write(
        "std/x/inner.vlt",
        "export function one(): i64 { return 1; }\n",
    );
    t.write(
        "std/x/inner.test.vlt",
        "import { one } from \"velt/x/inner\";\nexport function test_one() { one(); }\n",
    );
    t.path("std")
}

#[test]
fn std_test_files_are_std_code_wherever_they_are_imported_from() {
    // `velt test std/`: its generated harness, a virtual file in std/, imports the test files
    // relatively, and they import the internals they test.
    let t = Tree::new();
    let std = std_with_a_test(&t);
    let harness = std.join("harness.vlt");
    let (l, d, _) = load(
        &harness,
        LoadOptions {
            std_root: Some(std.clone()),
            root_source: Some(
                "import * as t from \"./x/inner.test.vlt\";\nfunction main() { t.test_one(); }\n"
                    .into(),
            ),
            ..Default::default()
        },
    );
    assert!(d.is_empty(), "{:?}", messages(&d));
    assert_eq!(paths(&l), ["main", "std/x/inner.test", "std/x/inner"]);
    assert!(l.modules[1].is_std && l.modules[2].is_std);

    // Any other std file reached that way is still an error.
    let (_, d, _) = load(
        &harness,
        LoadOptions {
            std_root: Some(std),
            root_source: Some("import { one } from \"./x/inner\";\nfunction main() {}\n".into()),
            ..Default::default()
        },
    );
    assert_eq!(
        messages(&d),
        ["module `./x/inner` is a file of the standard library: import it as `velt/x/inner`"]
    );
}

/// Only `velt test`'s harness gets std's test files as std code: a program (a written root, or
/// a module the harness imports) that imports one is refused, by specifier or relative path.
#[test]
fn std_test_files_are_not_importable_by_programs() {
    let t = Tree::new();
    let std = std_with_a_test(&t);
    let main = t.path("main.vlt");
    // A written root.
    t.write(
        "main.vlt",
        "import * as t from \"velt/x/inner.test\";\nfunction main() {}\n",
    );
    let (l, d, _) = load(
        &main,
        LoadOptions {
            std_root: Some(std.clone()),
            ..Default::default()
        },
    );
    assert!(!d.is_empty(), "{:?}", paths(&l));
    assert!(
        l.modules.iter().all(|m| !m.path.ends_with(".test")),
        "{:?}",
        paths(&l)
    );
    t.write(
        "main.vlt",
        "import * as t from \"./std/x/inner.test.vlt\";\nfunction main() {}\n",
    );
    let (_, d, _) = load(
        &main,
        LoadOptions {
            std_root: Some(std.clone()),
            ..Default::default()
        },
    );
    assert!(!d.is_empty());
    // A user test file the harness imports, which imports std's test file.
    t.write(
        "mine.test.vlt",
        "import * as t from \"velt/x/inner.test\";\nexport function f() {}\n",
    );
    let (l, d, _) = load(
        &t.path("harness.vlt"),
        LoadOptions {
            std_root: Some(std),
            root_source: Some(
                "import * as m from \"./mine.test.vlt\";\nfunction main() {}\n".into(),
            ),
            ..Default::default()
        },
    );
    assert!(!d.is_empty(), "{:?}", paths(&l));
}

/// A std root with modules, their internal files, the prelude and `lang/`.
fn std_tree(t: &Tree) -> PathBuf {
    t.write(
        "std/encoding/index.vlt",
        "export { hex } from \"velt/encoding/hex\";\n",
    );
    t.write(
        "std/encoding/hex.vlt",
        "export function hex(): i64 { return 1; }\n",
    );
    t.write(
        "std/encoding/json/index.vlt",
        "export { scan } from \"velt/encoding/json/scan\";\n",
    );
    t.write(
        "std/encoding/json/scan.vlt",
        "import { hex } from \"velt/encoding/hex\";\nexport function scan(): i64 { return hex(); }\n",
    );
    t.write(
        "std/encoding/json/tape/cells.vlt",
        "export const CELLS: i64 = 2;\n",
    );
    t.write("std/stray/loose.vlt", "export const LOOSE: i64 = 3;\n");
    t.write("std/prelude/index.vlt", "import \"./core\";\n");
    t.write(
        "std/prelude/core.vlt",
        "export function id(): i64 { return 0; }\n",
    );
    t.write("std/lang/symbol/index.vlt", "export class Symbol {}\n");
    t.write(
        "std/lang/intrinsics/index.vlt",
        "export { strLen } from \"velt/lang/intrinsics/string\";\n",
    );
    t.write(
        "std/lang/intrinsics/string.vlt",
        "export declare intrinsic function strLen(s: string): usize;\n",
    );
    t.path("std")
}

fn errors(t: &Tree, std: &Path, main: &str) -> Vec<String> {
    let root = t.write("p/main.vlt", main);
    let opts = LoadOptions {
        std_root: Some(std.to_path_buf()),
        ..Default::default()
    };
    let (_, d, _) = load(&root, opts);
    d.iter()
        .map(|d| {
            let notes: Vec<&str> = d.notes.iter().map(|n| n.as_str()).collect();
            format!("{} / {}", d.message, notes.join(" / "))
        })
        .collect()
}

#[test]
fn user_code_imports_only_public_std_modules() {
    let t = Tree::new();
    let std = std_tree(&t);
    // Modules, and std's own imports of each other's internals.
    let ok =
        "import { scan } from \"velt/encoding/json\";\nimport { hex } from \"velt/encoding\";\n\
              function main() {}\n";
    assert_eq!(errors(&t, &std, ok), Vec::<String>::new());
    assert_eq!(
        errors(
            &t,
            &std,
            "import { scan } from \"velt/encoding/json/scan\";\n"
        ),
        [
            "`velt/encoding/json/scan` is internal to `velt/encoding/json` / import what you need \
          from \"velt/encoding/json\", the module's public API"
        ]
    );
    // An implementation folder's file belongs to the nearest module above it.
    assert_eq!(
        errors(&t, &std, "import { CELLS } from \"velt/encoding/json/tape/cells\";\n"),
        ["`velt/encoding/json/tape/cells` is internal to `velt/encoding/json` / import what you \
          need from \"velt/encoding/json\", the module's public API"]
    );
    assert_eq!(
        errors(&t, &std, "import { LOOSE } from \"velt/stray/loose\";\n"),
        ["`velt/stray/loose` is internal to the standard library / user code imports a module's \
          `index.vlt` (the modules: https://veltlang.com/docs/std/)"]
    );
    assert_eq!(
        errors(&t, &std, "import { Symbol } from \"velt/lang/symbol\";\n"),
        [
            "`velt/lang/symbol` is internal to the standard library: `lang/` implements the \
          language / its classes and functions are globals (`Symbol`, `Proxy`, `Uint8Array`, …): \
          use them without an import"
        ]
    );
    // The intrinsics are std's own, whichever file of theirs is named.
    for spec in ["velt/lang/intrinsics", "velt/lang/intrinsics/string"] {
        assert_eq!(
            errors(&t, &std, &format!("import {{ strLen }} from \"{spec}\";\n")),
            [format!(
                "`{spec}` is internal to the standard library: the compiler's intrinsics are \
                 std's own building blocks / use the public API std builds on them (an array's \
                 `reserve`, `truncate` and `swap`, velt/encoding's `utf8ByteAt`, …)"
            )]
        );
    }
    assert_eq!(
        errors(&t, &std, "import { id } from \"velt/prelude/core\";\n"),
        ["`velt/prelude/core` is part of the prelude, which no module imports / its names are in \
          scope in every module without an import"]
    );
    assert_eq!(
        errors(&t, &std, "import \"velt/prelude\";\n"),
        [
            "`velt/prelude` is part of the prelude, which no module imports / its names are in \
          scope in every module without an import"
        ]
    );
}

#[test]
fn a_package_is_user_code() {
    let t = Tree::new();
    let std = std_tree(&t);
    t.write(
        "deps/json/src/lib.vlt",
        "import { scan } from \"velt/encoding/json/scan\";\nexport const X: i64 = 1;\n",
    );
    let root = t.write(
        "p/main.vlt",
        "import { X } from \"json\";\nfunction main() {}\n",
    );
    let resolver = super::tests::MapResolver([("json".to_string(), t.path("deps/json"))].into());
    let (_, d, _) = load(
        &root,
        LoadOptions {
            std_root: Some(std),
            packages: Some(&resolver),
            ..Default::default()
        },
    );
    assert_eq!(
        messages(&d),
        ["`velt/encoding/json/scan` is internal to `velt/encoding/json`"]
    );
}

#[test]
fn the_import_the_parser_adds_for_import_meta_is_not_the_users() {
    let t = Tree::new();
    let std = std_tree(&t);
    t.write(
        "std/lang/import_meta/index.vlt",
        "export class ImportMeta {\n  url: string;\n  constructor(d: string, f: string, m: bool, url: string) {\n    this.url = url;\n  }\n}\n",
    );
    let main = "function main() {\n  console.log(import.meta.url);\n}\n";
    assert_eq!(errors(&t, &std, main), Vec::<String>::new());
    // Written by hand, the same import is an error.
    let main = "import { ImportMeta } from \"velt/lang/import_meta\";\nfunction main() {}\n";
    assert_eq!(errors(&t, &std, main).len(), 1);
}
