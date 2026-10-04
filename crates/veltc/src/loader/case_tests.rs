//! Resolution that behaves the same on every OS: names match the disk in case, a file module
//! hiding a folder module is reported, and package modules may be `.ts` or `.tsx` files.

use std::collections::BTreeMap;

use velt_common::Severity;

use super::tests::{load, messages, paths, MapResolver, Tree};
use super::*;

#[test]
fn an_import_must_match_the_file_name_in_case() {
    let t = Tree::new();
    t.write("app/util.vlt", "export function util() {}\n");
    t.write("app/lib/math.vlt", "export function math() {}\n");
    t.write("app/ui/Index.ts", "export function ui() {}\n");
    let root = t.write(
        "app/main.vlt",
        "import \"./util\";\nimport \"./Util\";\nimport \"./Lib/math\";\nimport \"./ui\";\n",
    );
    let (l, diags, _) = load(&root, LoadOptions::default());
    assert_eq!(paths(&l), ["main", "util"]);
    assert_eq!(
        messages(&diags),
        [
            "module `./Util` names `Util.vlt`, but the file is `util.vlt`: file names must match in case",
            "module `./Lib/math` names `Lib/math.vlt`, but the file is `lib/math.vlt`: file names must match in case",
            "module `./ui` names `ui/index.ts`, but the file is `ui/Index.ts`: file names must match in case",
        ]
    );
    let fixes: Vec<&str> = diags.iter().map(|d| d.notes[0].as_str()).collect();
    assert_eq!(
        fixes,
        [
            "import it as `./util`",
            "import it as `./lib/math`",
            "rename `ui/Index.ts` to `ui/index.ts`",
        ]
    );
}

#[test]
fn files_differing_only_in_case_are_different_modules() {
    // On a file system that ignores case, `./Util` would find both `Util.ts` and `util.vlt`.
    let t = Tree::new();
    t.write("app/util.vlt", "export function lower() {}\n");
    t.write("app/Util.ts", "export function upper() {}\n");
    let root = t.write("app/main.vlt", "import { upper } from \"./Util\";\n");
    let (l, diags, sm) = load(&root, LoadOptions::default());
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert!(sm.get(l.modules[1].file).path.ends_with("Util.ts"));
}

#[test]
fn a_file_module_hiding_a_folder_module_of_another_extension_is_a_warning() {
    let t = Tree::new();
    t.write("app/ui.ts", "export function ui() {}\n");
    t.write("app/ui/index.vlt", "export function ui() {}\n");
    t.write("app/same.vlt", "");
    t.write("app/same/index.vlt", "");
    let root = t.write(
        "app/main.vlt",
        "import { ui } from \"./ui\";\nimport \"./same\";\n",
    );
    let (l, diags, sm) = load(&root, LoadOptions::default());
    assert!(sm.get(l.modules[1].file).path.ends_with("ui.ts"));
    assert_eq!(
        messages(&diags),
        ["module `./ui` is the file `ui.ts`, which hides the folder module `ui/index.vlt`"]
    );
    assert_eq!(diags[0].severity, Severity::Warning);
    assert!(
        diags[0].notes[0].contains("import the folder as `./ui/index`"),
        "{:?}",
        diags[0].notes
    );
}

#[test]
fn package_modules_may_be_typescript_files() {
    let t = Tree::new();
    t.write("json/src/lib.ts", "export function parse() {}\n");
    t.write("json/src/lexer/index.tsx", "export function lex() {}\n");
    t.write("json/src/value.vlt", "export function value() {}\n");
    let root = t.write(
        "app/main.vlt",
        "import \"json\";\nimport \"json/lexer\";\nimport \"json/value\";\nimport \"json/Value\";\n",
    );
    let resolver = MapResolver(BTreeMap::from([("json".to_string(), t.path("json"))]));
    let opts = LoadOptions {
        packages: Some(&resolver),
        ..Default::default()
    };
    let (l, diags, _) = load(&root, opts);
    assert_eq!(paths(&l), ["main", "json", "json/lexer", "json/value"]);
    assert_eq!(messages(&diags).len(), 1, "{:?}", messages(&diags));
    assert!(messages(&diags)[0].starts_with("module `json/Value` names"));
    assert_eq!(diags[0].notes[0], "import it as `json/value`");
}
