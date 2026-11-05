//! [`load_more`] over a test's own std root.

use std::fs;
use std::path::Path;

use velt_common::SourceMap;

use super::*;
use crate::load_program;

/// A std root with `files` (path below it, source), and a prelude of one small module.
fn std_root(files: &[(&str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let mut all = vec![("prelude/index.velt", "import \"./p\";\n")];
    if !files.iter().any(|(rel, _)| *rel == "prelude/p.velt") {
        all.push(("prelude/p.velt", "export function p(): i64 { return 0; }\n"));
    }
    all.extend_from_slice(files);
    for (rel, src) in all {
        let p = tmp.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, src).unwrap();
    }
    tmp
}

fn options(std: &Path, demand_std: bool, src: &str) -> LoadOptions<'static> {
    LoadOptions {
        std_root: Some(std.to_path_buf()),
        root_source: Some(src.to_string()),
        demand_std,
        ..Default::default()
    }
}

fn paths(ms: &[SourceModule]) -> Vec<&str> {
    ms.iter().map(|m| m.path.as_str()).collect()
}

const HOOKS: &[(&str, &str)] = &[
    ("lang/hooks/number_text.velt", "import { i } from \"velt/lang/number/integer\";\nexport function t(): i64 { return i(); }\n"),
    ("lang/hooks/number_float.velt", "import { t } from \"velt/lang/hooks/number_text\";\nimport { f } from \"velt/lang/number/format\";\nexport function g(): i64 { return t() + f(); }\n"),
    ("lang/hooks/any_record.velt", "export function r(): i64 { return 1; }\n"),
    ("lang/number/integer.velt", "export function i(): i64 { return 1; }\n"),
    ("lang/number/format.velt", "export function f(): i64 { return 1; }\n"),
];

#[test]
fn a_demand_serves_the_module_and_what_it_imports_once() {
    let tmp = std_root(HOOKS);
    let opts = options(tmp.path(), true, "function main() {}\n");
    let (mut sm, mut diags) = (SourceMap::default(), vec![]);
    let root = tmp.path().join("main.velt");
    let mut l = load_program(&mut sm, &root, opts, &mut diags).unwrap();
    assert!(!paths(&l.modules).iter().any(|p| p.starts_with("std/lang")));
    let opts = options(tmp.path(), true, "");
    let mut more = |d: &str, l: &mut crate::Loaded| {
        let d = [d.to_string()];
        load_more(&mut sm, &mut l.state, &l.modules, &d, &opts, &mut diags)
    };
    let a = more("std/lang/hooks/number_text", &mut l);
    assert_eq!(
        paths(&a.modules),
        ["std/lang/hooks/number_text", "std/lang/number/integer"]
    );
    assert_eq!(
        a.modules[0].imports,
        [(
            "velt/lang/number/integer".to_string(),
            "std/lang/number/integer".to_string()
        )]
    );
    // What the first served is not served again; an import of it resolves.
    let b = more("std/lang/hooks/number_float", &mut l);
    assert_eq!(
        paths(&b.modules),
        ["std/lang/hooks/number_float", "std/lang/number/format"]
    );
    assert!(b.modules[0]
        .imports
        .iter()
        .any(|(_, t)| t == "std/lang/hooks/number_text"));
    assert!(more("std/lang/hooks/number_text", &mut l)
        .modules
        .is_empty());
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn without_demand_every_module_a_demand_could_ask_for_loads_up_front_in_table_order() {
    let tmp = std_root(HOOKS);
    let opts = options(tmp.path(), false, "function main() {}\n");
    let (mut sm, mut diags) = (SourceMap::default(), vec![]);
    let root = tmp.path().join("main.velt");
    let l = load_program(&mut sm, &root, opts, &mut diags).unwrap();
    let got: Vec<&str> = paths(&l.modules)
        .into_iter()
        .filter(|p| p.starts_with("std/lang"))
        .collect();
    assert_eq!(
        got,
        [
            "std/lang/hooks/number_text",
            "std/lang/hooks/number_float",
            "std/lang/hooks/any_record",
            "std/lang/number/integer",
            "std/lang/number/format",
        ]
    );
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn a_std_root_without_them_loads_without_them() {
    let tmp = std_root(&[]);
    let opts = options(tmp.path(), false, "function main() {}\n");
    let (mut sm, mut diags) = (SourceMap::default(), vec![]);
    let root = tmp.path().join("main.velt");
    load_program(&mut sm, &root, opts, &mut diags).unwrap();
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn a_module_that_breaks_the_rule_is_an_internal_error() {
    let tmp = std_root(&[(
        "lang/hooks/any_record.velt",
        "class Mine extends Error {}\nexport function r(): i64 { return 1; }\n",
    )]);
    let opts = options(tmp.path(), true, "function main() {}\n");
    let (mut sm, mut diags) = (SourceMap::default(), vec![]);
    let root = tmp.path().join("main.velt");
    let mut l = load_program(&mut sm, &root, opts, &mut diags).unwrap();
    let opts = options(tmp.path(), true, "");
    let d = ["std/lang/hooks/any_record".to_string()];
    load_more(&mut sm, &mut l.state, &l.modules, &d, &opts, &mut diags);
    assert!(
        diags
            .iter()
            .any(|d| d.message.starts_with("ICE: std module")
                && d.message.contains("class `Mine` extends `Error`")),
        "{diags:?}"
    );
}

#[test]
fn a_re_export_skipped_until_now_is_reported_as_an_import_of_the_loaded_module() {
    let tmp = std_root(&[
        (
            "prelude/p.velt",
            "import { a } from \"velt/lang/shelf\";\nexport function p(): i64 { return a(); }\n",
        ),
        (
            "lang/shelf/index.velt",
            "export { a } from \"./a\";\nexport { b } from \"./b\";\n",
        ),
        (
            "lang/shelf/a.velt",
            "export function a(): i64 { return 1; }\n",
        ),
        (
            "lang/shelf/b.velt",
            "export function b(): i64 { return 1; }\n",
        ),
        (
            "lang/hooks/any_record.velt",
            "import { b } from \"velt/lang/shelf\";\nexport function r(): i64 { return b(); }\n",
        ),
    ]);
    let opts = options(tmp.path(), true, "function main() {}\n");
    let (mut sm, mut diags) = (SourceMap::default(), vec![]);
    let root = tmp.path().join("main.velt");
    let mut l = load_program(&mut sm, &root, opts, &mut diags).unwrap();
    // The prelude's import asks for `a` alone: the re-export of `b` is skipped.
    assert!(!paths(&l.modules).contains(&"std/lang/shelf/b"));
    let opts = options(tmp.path(), true, "");
    let d = ["std/lang/hooks/any_record".to_string()];
    let got = load_more(&mut sm, &mut l.state, &l.modules, &d, &opts, &mut diags);
    assert!(diags.is_empty(), "{diags:?}");
    assert!(paths(&got.modules).contains(&"std/lang/shelf/b"));
    assert_eq!(
        got.imports,
        [(
            "std/lang/shelf".to_string(),
            "./b".to_string(),
            "std/lang/shelf/b".to_string()
        )]
    );
}

#[test]
fn a_std_root_without_the_demanded_module_serves_nothing_and_says_nothing() {
    let without: Vec<_> = (HOOKS.iter().copied())
        .filter(|(rel, _)| *rel != "lang/hooks/number_float.velt")
        .collect();
    let tmp = std_root(&without);
    let opts = options(tmp.path(), true, "function main() {}\n");
    let (mut sm, mut diags) = (SourceMap::default(), vec![]);
    let root = tmp.path().join("main.velt");
    let mut l = load_program(&mut sm, &root, opts, &mut diags).unwrap();
    let opts = options(tmp.path(), true, "");
    let d = ["std/lang/hooks/number_float".to_string()];
    let got = load_more(&mut sm, &mut l.state, &l.modules, &d, &opts, &mut diags);
    assert!(got.modules.is_empty() && got.imports.is_empty());
    assert!(diags.is_empty(), "{diags:?}");
    // The load goes on: what the root does have is still served.
    let d = ["std/lang/hooks/any_record".to_string()];
    let got = load_more(&mut sm, &mut l.state, &l.modules, &d, &opts, &mut diags);
    assert_eq!(paths(&got.modules), ["std/lang/hooks/any_record"]);
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn a_program_without_std_serves_nothing_and_says_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let std = tmp.path().join("nonexistent");
    let opts = options(&std, true, "function main() {}\n");
    let (mut sm, mut diags) = (SourceMap::default(), vec![]);
    let root = tmp.path().join("main.velt");
    let mut l = load_program(&mut sm, &root, opts, &mut diags).unwrap();
    assert!(!l.modules.iter().any(|m| m.is_std));
    let opts = options(&std, true, "");
    let d = ["std/lang/hooks/number_float".to_string()];
    let got = load_more(&mut sm, &mut l.state, &l.modules, &d, &opts, &mut diags);
    assert!(got.modules.is_empty() && got.imports.is_empty());
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn a_demand_that_is_no_std_module_stays_an_internal_error() {
    let tmp = std_root(HOOKS);
    let opts = options(tmp.path(), true, "function main() {}\n");
    let (mut sm, mut diags) = (SourceMap::default(), vec![]);
    let root = tmp.path().join("main.velt");
    let mut l = load_program(&mut sm, &root, opts, &mut diags).unwrap();
    let opts = options(tmp.path(), true, "");
    let d = ["not/std".to_string()];
    load_more(&mut sm, &mut l.state, &l.modules, &d, &opts, &mut diags);
    assert!(
        diags
            .iter()
            .any(|d| d.message.starts_with("ICE: `not/std`")),
        "{diags:?}"
    );
}
