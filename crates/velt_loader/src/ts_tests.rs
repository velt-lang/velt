//! `.ts` and `.tsx` modules next to `.velt` ones: resolution order, ambiguity, explicit
//! extensions, JSX only in `.tsx`.

use super::tests::{imports, jsx_runtime, load, messages, paths, AliasResolver, JsxResolver, Tree};
use super::*;
use velt_common::Severity;

#[test]
fn relative_imports_find_velt_ts_and_tsx_files() {
    let t = Tree::new();
    t.write("app/model.ts", "export function model() {}\n");
    t.write("app/view.tsx", "export function view() {}\n");
    t.write("app/shapes/index.ts", "export function square() {}\n");
    t.write(
        "app/util.velt",
        "import { model } from \"./model\";\nexport function util() {}\n",
    );
    let root = t.write(
        "app/main.ts",
        "import { util } from \"./util\";\nimport { view } from \"./view\";\nimport { square } from \"./shapes\";\nfunction main() {}\n",
    );
    let (l, diags, sm) = load(&root, LoadOptions::default());
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(paths(&l), ["main", "util", "view", "shapes", "model"]);
    assert_eq!(imports(&l, "util"), [("./model".into(), "model".into())]);
    let file = |m: &str| {
        let module = l.modules.iter().find(|x| x.path == m).unwrap();
        sm.get(module.file).path.clone()
    };
    assert!(file("view").ends_with("view.tsx"));
    assert!(file("shapes").ends_with("shapes/index.ts"));
}

#[test]
fn two_candidates_for_one_import_are_ambiguous() {
    let t = Tree::new();
    t.write("app/a.velt", "export function a() {}\n");
    t.write("app/a.ts", "export function a() {}\n");
    t.write("app/a.tsx", "export function a() {}\n");
    t.write("app/b/index.velt", "");
    t.write("app/b/index.tsx", "");
    let src = "import { a } from \"./a\";\nimport \"./b\";\nfunction main() {}\n";
    let root = t.write("app/main.velt", src);
    let (l, diags, sm) = load(&root, LoadOptions::default());
    assert_eq!(paths(&l), ["main"]);
    let msgs = messages(&diags);
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert_eq!(
        msgs,
        [
            "module `./a` is ambiguous: it could be `a.velt`, `a.ts` or `a.tsx`",
            "module `./b` is ambiguous: it could be `b/index.velt` or `b/index.tsx`",
        ]
    );
    let rendered = diags[0].render(&sm);
    assert!(rendered.contains("main.velt:1:19"), "{rendered}");
}

#[test]
fn explicit_extensions_name_one_file() {
    let t = Tree::new();
    for f in ["a.velt", "a.ts", "b.ts", "c.tsx"] {
        t.write(&format!("app/{f}"), "export function f() {}\n");
    }
    // `./a.ts` names `a.ts` even though `a.velt` exists; `./b.js` and `./c.jsx` are TypeScript's
    // spelling of `b.ts` and `c.tsx`.
    let root = t.write(
        "app/main.velt",
        "import { f } from \"./a.ts\";\nimport \"./b.js\";\nimport \"./c.jsx\";\nimport \"./b.tsx\";\nimport \"./c.js\";\n",
    );
    let (l, diags, sm) = load(&root, LoadOptions::default());
    assert_eq!(paths(&l), ["main", "a", "b", "c"]);
    assert!(sm.get(l.modules[1].file).path.ends_with("a.ts"));
    assert_eq!(
        messages(&diags),
        ["cannot find module `./b.tsx`"],
        "`./c.js` finds `c.tsx`"
    );

    // `./a.velt` names `a.velt`; the same module path as `a.ts` loaded through another import.
    let root = t.write(
        "app/both.velt",
        "import \"./a.velt\";\nimport \"./a.ts\";\n",
    );
    let (l, diags, _) = load(&root, LoadOptions::default());
    assert_eq!(paths(&l), ["main", "a"]);
    assert!(
        messages(&diags)[0].contains("same module path `a`"),
        "{:?}",
        messages(&diags)
    );
}

#[test]
fn package_imports_name_modules_without_an_extension() {
    let resolve = |spec: &str| resolve_spec(spec, Path::new("."));
    for spec in ["json/parse.ts", "json/parse.velt", "json/x.tsx"] {
        let err = resolve(spec).unwrap_err();
        assert!(err.contains("should not include"), "{spec}: {err}");
    }
}

#[test]
fn path_aliases_find_ts_files() {
    let t = Tree::new();
    t.write("p/src/util/strings.ts", "export function up() {}\n");
    t.write("p/src/ui/index.tsx", "export function draw() {}\n");
    let root = t.write(
        "p/src/main.velt",
        "import { up } from \"@app/util/strings\";\nimport { draw } from \"@app/ui\";\n",
    );
    let resolver = AliasResolver(t.path("p"));
    let opts = LoadOptions {
        packages: Some(&resolver),
        ..Default::default()
    };
    let (l, diags, _) = load(&root, opts);
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(paths(&l), ["main", "util/strings", "ui"]);
}

#[test]
fn jsx_in_tsx_files_uses_the_package_provider_and_is_an_error_in_ts_files() {
    let t = Tree::new();
    t.write("app/ui/jsx-runtime.velt", "export class Element {}\n");
    t.write(
        "app/card.tsx",
        "export function Card() { return <div />; }\n",
    );
    t.write("app/list.ts", "export function List() { return <ul />; }\n");
    let root = t.write(
        "app/main.ts",
        "import { Card } from \"./card\";\nimport { List } from \"./list\";\nfunction main() {}\n",
    );
    let resolver = JsxResolver("./ui".into());
    let opts = LoadOptions {
        packages: Some(&resolver),
        ..Default::default()
    };
    let (l, diags, sm) = load(&root, opts);
    assert_eq!(jsx_runtime(&l, "card"), Some("ui/jsx-runtime"));
    assert_eq!(messages(&diags), ["JSX is not allowed in a `.ts` file"]);
    assert!(diags[0].notes[0].contains("rename it to `list.tsx`"));
    let rendered = diags[0].render(&sm);
    assert!(rendered.contains("list.ts:1:"), "{rendered}");
}

#[test]
fn canonical_paths_drop_every_source_extension() {
    let root = Origin::Root(PathBuf::from("/p"));
    assert_eq!(root.canonical(Path::new("/p/util.ts")), "util");
    assert_eq!(root.canonical(Path::new("/p/ui/card.tsx")), "ui/card");
    assert_eq!(root.canonical(Path::new("/p/shapes/index.tsx")), "shapes");
    assert_eq!(root.canonical(Path::new("/p/a.test.ts")), "a.test");
}

#[test]
fn declaration_files_are_not_modules() {
    let t = Tree::new();
    t.write("app/types.d.ts", "export interface Point { x: number }\n");
    let root = t.write(
        "app/main.velt",
        "import \"./types.d\";\nimport \"./types.d.ts\";\n",
    );
    let (l, diags, _) = load(&root, LoadOptions::default());
    assert_eq!(paths(&l), ["main"]);
    assert_eq!(
        messages(&diags),
        [
            "cannot find module `./types.d`",
            "declaration files (`.d.ts`) are not modules",
        ]
    );
    let tried: Vec<&str> = diags[0].notes.iter().map(String::as_str).collect();
    assert_eq!(
        tried,
        [
            "tried `types.d.velt`",
            "tried `types.d.tsx`",
            "tried `types.d/index.velt`",
            "tried `types.d/index.ts`",
            "tried `types.d/index.tsx`",
        ]
    );
    assert!(diags[1].notes[0].contains("`.ts`"), "{:?}", diags[1].notes);
}

#[test]
fn type_assertions_are_reported_in_ts_files_only() {
    let t = Tree::new();
    t.write("app/cast.ts", "export const n = <number>x;\n");
    let root = t.write("app/main.velt", "import \"./cast\";\n");
    let (_, diags, sm) = load(&root, LoadOptions::default());
    assert_eq!(
        messages(&diags),
        ["type assertions `<T>x` are not supported"]
    );
    assert!(diags[0].render(&sm).contains("cast.ts:1:18"));
}

#[test]
fn a_file_with_the_old_extension_loads_with_a_warning() {
    let t = Tree::new();
    t.write("app/util.vlt", "export function util() {}\n"); // velt-ext: keep
    let root = t.write(
        "app/main.velt",
        "import { util } from \"./util\";\nfunction main() {}\n",
    );
    let (l, diags, _) = load(&root, LoadOptions::default());
    assert_eq!(paths(&l), ["main", "util"]);
    let messages = messages(&diags);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("rename"), "{messages:?}");
    assert!(
        diags.iter().all(|d| d.severity != Severity::Error),
        "{messages:?}"
    );
}

#[test]
fn a_module_with_both_extensions_is_ambiguous() {
    let t = Tree::new();
    t.write("app/util.velt", "export function util() {}\n");
    t.write("app/util.vlt", "export function util() {}\n"); // velt-ext: keep
    let root = t.write(
        "app/main.velt",
        "import { util } from \"./util\";\nfunction main() {}\n",
    );
    let (_, diags, _) = load(&root, LoadOptions::default());
    let errors = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    assert_eq!(errors, 1, "{:?}", messages(&diags));
    let messages = messages(&diags);
    assert!(
        messages.iter().any(|m| m.contains("is ambiguous")),
        "{messages:?}"
    );
}

#[test]
fn a_dependency_with_the_old_extension_is_named_once() {
    let t = Tree::new();
    let old = vpm::sources::OLD_EXTENSION;
    t.write(
        &format!("dep/src/lib.{old}"),
        "import { a } from \"./a\";\nexport function lib() {}\n",
    );
    t.write(&format!("dep/src/a.{old}"), "export function a() {}\n");
    let root = t.write(
        "app/src/main.velt",
        "import { lib } from \"dep\";\nfunction main() {}\n",
    );
    let resolver = super::tests::MapResolver(std::collections::BTreeMap::from([(
        "dep".to_string(),
        t.path("dep"),
    )]));
    let (_, diags, _) = load(
        &root,
        LoadOptions {
            packages: Some(&resolver),
            ..Default::default()
        },
    );
    let messages = messages(&diags);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("package `dep`"), "{messages:?}");
    assert!(messages[0].contains("its author"), "{messages:?}");
}
