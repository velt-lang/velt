//! Module syntax beyond named imports: namespace imports, `import type`, re-exports, local export
//! lists, and the errors for `export default` and default imports.

mod common;

use common::*;

fn import(m: &Module, i: usize) -> (&Import, bool) {
    let ItemKind::Import(imp) = &m.items[i].kind else {
        panic!("item {i} is not an import")
    };
    (imp, m.items[i].exported)
}

#[test]
fn namespace_and_type_only_imports() {
    let m = parse_ok(
        "import * as geo from \"./geo\"; import type { Shape, Size as S } from \"./types\"; import { type Id, make } from \"./ids\"; import { type } from \"./kw\";",
    );
    let (ns, exported) = import(&m, 0);
    assert!(!exported && !ns.all && ns.names.is_empty());
    assert_eq!(ns.namespace.as_ref().unwrap().name, "geo");
    assert_eq!(ns.from, "./geo");
    let (types, _) = import(&m, 1);
    assert!(types.names.iter().all(|n| n.type_only));
    assert_eq!(types.names[1].alias.as_ref().unwrap().name, "S");
    let (mixed, _) = import(&m, 2);
    assert!(mixed.names[0].type_only && mixed.names[0].name.name == "Id");
    assert!(!mixed.names[1].type_only);
    let (kw, _) = import(&m, 3);
    assert!(!kw.names[0].type_only && kw.names[0].name.name == "type");
}

#[test]
fn re_exports_and_export_lists() {
    let m = parse_ok(
        "export { a, b as c } from \"./m\"; export * from \"./all\"; export type { T } from \"./t\"; export { x, y as z }; export type Id = i64; const x = 1; const y = 2;",
    );
    let (named, exported) = import(&m, 0);
    assert!(exported && !named.all && named.from == "./m");
    assert_eq!(named.names[1].alias.as_ref().unwrap().name, "c");
    let (all, exported) = import(&m, 1);
    assert!(exported && all.all && all.names.is_empty() && all.from == "./all");
    let (types, _) = import(&m, 2);
    assert!(types.names[0].type_only);
    let (local, exported) = import(&m, 3);
    assert!(exported && local.from.is_empty() && local.names.len() == 2);
    assert!(matches!(m.items[4].kind, ItemKind::TypeAlias(_)) && m.items[4].exported);
}

#[test]
fn export_default_is_an_error_with_a_named_export_hint() {
    let (m, diags) = parse("export default function main() {}\n");
    assert_eq!(
        diags[0].message,
        "`export default` is not supported: Velt has named exports only"
    );
    assert_eq!(
        diags[0].notes,
        ["use a named export: `export function main`"]
    );
    assert!(m.items[0].exported && matches!(m.items[0].kind, ItemKind::Function(_)));

    let (_, diags) = parse("const f = 1;\nexport default f;\nfunction g() {}\n");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].notes, ["use a named export: `export { f };`"]);
}

#[test]
fn default_imports_and_export_star_as_are_errors() {
    let errs = errors("import fs from \"velt:fs\";\nfunction main() {}\n");
    assert_eq!(
        errs,
        ["default imports are not supported: Velt has named exports only"]
    );
    let errs = errors("export * as ns from \"./m\";\n");
    assert!(
        errs[0].starts_with("`export * as ns from` is not supported"),
        "{errs:?}"
    );
}
