//! Module syntax beyond named imports: namespace imports, `import type`, re-exports, local export
//! lists, default exports and imports (#1916).

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

/// The alias, name and item span of the one-name export list `item`.
fn list_of(m: &Module, i: usize) -> (String, String, Span) {
    let (imp, exported) = import(m, i);
    assert!(exported && imp.from.is_empty() && imp.names.len() == 1);
    let n = &imp.names[0];
    let alias = n.alias.as_ref().map_or("", |a| &a.name).to_string();
    (alias, n.name.name.clone(), m.items[i].span)
}

/// `export default <declaration>` (#1916): the declaration, not exported, then `export { f as
/// default }` spanning the `default` keyword; without a name, the declaration named `default`.
#[test]
fn export_default_declarations() {
    let src = "export default function main() {}
";
    let m = parse_ok(src);
    assert!(!m.items[0].exported && matches!(m.items[0].kind, ItemKind::Function(_)));
    assert_eq!(m.items[0].span.lo, 0, "the item starts at `export`");
    let (alias, name, span) = list_of(&m, 1);
    assert_eq!((alias.as_str(), name.as_str()), ("default", "main"));
    assert_eq!(&src[span.lo as usize..span.hi as usize], "default");
    assert!(velt_syntax::written_default(import(&m, 1).0));

    for (src, kind) in [
        ("export default class C {}", "class"),
        ("export default abstract class C {}", "class"),
        ("export default interface I { x: number }", "interface"),
        ("export default struct S { x: f64 }", "struct"),
        ("export default enum E { A }", "enum"),
        ("export default async function f() {}", "function"),
    ] {
        let m = parse_ok(src);
        assert_eq!(m.items.len(), 2, "{src}");
        assert!(!m.items[0].exported, "{kind}: {src}");
        assert_eq!(list_of(&m, 1).0, "default", "{src}");
    }

    for src in [
        "export default function () {}",
        "export default function* () {}",
        "export default async function () {}",
        "export default class {}",
        "export default class extends Base {}",
    ] {
        let m = parse_ok(src);
        assert_eq!(m.items.len(), 1, "{src}");
        assert!(m.items[0].exported, "{src}");
        let name = match &m.items[0].kind {
            ItemKind::Function(f) => &f.sig.name,
            ItemKind::Class(c) => &c.name,
            k => panic!("{src}: {k:?}"),
        };
        assert_eq!(name.name, "default", "{src}");
        assert_eq!(
            &src[name.span.lo as usize..name.span.hi as usize],
            "default"
        );
    }
}

/// `export default expr;` is the exported constant `default` (a snapshot); `export default x;`
/// the live `export { x as default }`, unless `x` is a module-level `let`.
#[test]
fn export_default_expressions() {
    let m = parse_ok(
        "export default make(1);
function make(n: number) { return n; }
",
    );
    let ItemKind::Var(v) = &m.items[0].kind else {
        panic!("{:?}", m.items[0].kind)
    };
    assert!(m.items[0].exported && v.kind == VarKind::Const && v.init.is_some());
    assert!(matches!(&v.pattern.kind, PatternKind::Ident(n) if n.name == "default"));

    let m = parse_ok(
        "const f = 1;
export default f;
",
    );
    let (alias, name, span) = list_of(&m, 1);
    assert_eq!((alias.as_str(), name.as_str()), ("default", "f"));
    assert_eq!(span.hi - span.lo, "export default f;".len() as u32);

    let m = parse_ok(
        "let n = 1;
export default n;
",
    );
    let ItemKind::Var(v) = &m.items[1].kind else {
        panic!("{:?}", m.items[1].kind)
    };
    assert!(matches!(&v.init.as_ref().unwrap().kind, ExprKind::Ident(n) if n.name == "n"));

    // A written `export { x as default }` is not a desugared one.
    let m = parse_ok(
        "const x = 1;
export { x as default };
",
    );
    assert!(!velt_syntax::written_default(import(&m, 1).0));
}

#[test]
fn default_imports() {
    let m = parse_ok(
        "import a from \"./m\"; import b, { c, d as e } from \"./m\"; import f, * as ns from \"./m\"; import type T from \"./m\"; import type from \"./m\"; import { default as g } from \"./m\";",
    );
    let names = |i: usize| {
        let (imp, _) = import(&m, i);
        let names: Vec<(String, String)> = (imp.names.iter())
            .map(|n| {
                (
                    n.name.name.clone(),
                    n.alias.as_ref().map_or(String::new(), |a| a.name.clone()),
                )
            })
            .collect();
        (imp.default_import, names)
    };
    let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
    assert_eq!(names(0), (true, vec![pair("default", "a")]));
    assert_eq!(
        names(1),
        (
            true,
            vec![pair("default", "b"), pair("c", ""), pair("d", "e")]
        )
    );
    assert_eq!(names(2), (true, vec![pair("default", "f")]));
    assert_eq!(import(&m, 2).0.namespace.as_ref().unwrap().name, "ns");
    assert_eq!(names(3), (true, vec![pair("default", "T")]));
    assert!(import(&m, 3).0.names[0].type_only);
    assert_eq!(names(4), (true, vec![pair("default", "type")]));
    assert!(!import(&m, 4).0.names[0].type_only);
    assert_eq!(names(5), (false, vec![pair("default", "g")]));
}

#[test]
fn export_star_as_and_re_exported_defaults() {
    let m = parse_ok(
        "export * as ns from \"./m\"; export { default } from \"./a\"; export { default as B, x as y } from \"./b\";",
    );
    let (ns, exported) = import(&m, 0);
    assert!(exported && !ns.all && ns.names.is_empty());
    assert_eq!(ns.namespace.as_ref().unwrap().name, "ns");
    assert_eq!(import(&m, 1).0.names[0].name.name, "default");
}

#[test]
fn default_export_errors() {
    let two = "a module cannot have multiple default exports";
    assert_eq!(
        errors(
            "export default 1;
export default 2;
"
        ),
        [two]
    );
    assert_eq!(
        errors(
            "const x = 1;
export default function f() {}
export { x as default };
"
        ),
        [two]
    );
    assert_eq!(
        errors(
            "export default class {}
export { default } from \"./m\";
"
        ),
        [two]
    );
    let reserved = "`default` is a reserved word and cannot be used as a name";
    assert_eq!(
        errors(
            "import { default } from \"./m\";
"
        ),
        [reserved]
    );
    assert_eq!(
        errors(
            "let default = 1;
"
        ),
        [reserved]
    );
    // `export * as default` is the module's default, valid alone and not next to another.
    assert_eq!(errors("export * as default from \"./m\";\n"), [] as [&str; 0]);
    assert_eq!(
        errors("export * as default from \"./m\";\nexport default 1;\n"),
        [two]
    );
    let errs = errors(
        "function f() {
  export default 1;
}
",
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("only allowed at the top level")),
        "{errs:?}"
    );

    // TypeScript has no `export default enum` (a `.vlt` file may, as for a struct).
    let (_, diags) = velt_syntax::parse_ts_file(
        FileId(0),
        "export default enum E { A }
",
    );
    let messages: Vec<_> = diags.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        messages,
        ["`export default` cannot be followed by an `enum` declaration in TypeScript"]
    );
}

/// The top-level variables of a script that stay at module level, by name (the others move into
/// the generated `main`).
fn module_vars(src: &str) -> Vec<String> {
    parse_ok(src)
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Var(v) => match &v.pattern.kind {
                PatternKind::Ident(id) => Some(id.name.clone()),
                _ => Some("<pattern>".into()),
            },
            _ => None,
        })
        .collect()
}

/// A top-level variable that a closure the statements make refers to stays module state, as in
/// Node, where the closure sees the module's variable (#1872), when the code assigns it (a request
/// handler or a test's callback, or a statement after the closure was made) or it holds a new
/// object the closure may change (`new Map()`, `[]`). One only the statements use moves into
/// `main`, and so do one nothing assigns (a copy is the variable), a constant only its own
/// initializer's closure names, a destructuring declaration, and one the closure's own parameter
/// shadows.
#[test]
fn script_variables_closures_capture_stay_module_state() {
    let src = "let hits = 0;\nconst byPath = new Map<string, number>();\nlet label = \"a\";\n\
               let local = 1;\nconst fact = (n: number): number => (n <= 1 ? 1 : n * fact(n - 1));\n\
               let { a } = { a: 1 };\nlet shadowed = 2;\nlet step = 3;\nconst events: string[] = [];\n\
               serve(async (req) => { hits++; byPath.set(\"/\", 1); events.push(label); return label + step; });\n\
               const show = function () { return a; };\n\
               [1].forEach((shadowed) => { shadowed += 1; });\n\
               local += 1;\nlabel = \"b\";\nconsole.log(local, fact(3), shadowed, show());\n";
    assert_eq!(module_vars(src), ["hits", "byPath", "label", "events"]);
    // A method of an object literal and a function declared in a block capture too, and so does
    // the closure in another variable's initializer; a destructuring assignment assigns.
    let src = "let n = 0;\nlet m = 0;\nlet k = 0;\nlet j = 0;\nconst o = { inc() { n++; } };\n\
               { function bump() { m++; } bump(); }\nconst get = () => k + j;\no.inc();\n\
               [k] = [1];\nconsole.log(get());\n";
    assert_eq!(module_vars(src), ["n", "m", "k"]);
    // Not yet an object literal, a new object whose declared type checks narrow, or one a
    // closure in its own initializer names.
    let src = "const stats = { n: 0 };\nconst grow: object = new Map<number, number>();\n\
               const p = new Proxy(t, { get: () => p });\nconst set = new Set<number>();\n\
               spawn((async () => { stats.n++; set.add(1); })());\n\
               if (grow instanceof Map) { grow.forEach((v, k) => grow.set(k, v)); }\n";
    assert_eq!(module_vars(src), ["set"]);
    // Not a new promise: module state cannot hold one, and a closure awaits the same promise.
    let src = "const ready = new Promise<number>((r) => r(5));\nconst list = [1];\n\
               const use = async () => (await ready) + list.length;\nconsole.log(await use());\n";
    assert_eq!(module_vars(src), ["list"]);
}

/// The default export a parsed `src` gives: `(is it the constant, the name in its list)`.
fn default_of(src: &str) -> (bool, Option<String>) {
    let parsed = velt_syntax::parse_with(
        velt_common::FileId(0),
        src,
        &velt_syntax::ParseOptions::default(),
    );
    assert!(parsed.diags.is_empty(), "{:?}", parsed.diags);
    for item in parsed.module.items.iter().filter(|i| i.exported) {
        match &item.kind {
            velt_syntax::ast::ItemKind::Var(_) => return (true, None),
            velt_syntax::ast::ItemKind::Import(i) => {
                return (false, i.names.first().map(|n| n.name.name.clone()))
            }
            _ => {}
        }
    }
    panic!("no default export in {src}");
}

#[test]
fn export_default_of_a_name_is_a_list_only_where_it_may_be_live() {
    // What the module binds, whose value is there and can't change; an import, which sema decides.
    for src in [
        "const x = 1;\nexport default x;\n",
        "export default f;\nfunction f() {}\n",
        "class C {}\nexport default C;\n",
        "interface I {}\nexport default I;\n",
        "type T = number;\nexport default T;\n",
        "import { x } from \"./a\";\nexport default x;\n",
        "import * as x from \"./a\";\nexport default x;\n",
    ] {
        assert!(!default_of(src).0, "{src}");
    }
    // A `let`, a declaration below and a name the module doesn't bind are read when it runs.
    for src in [
        "let x = 1;\nexport default x;\n",
        "export default x;\nconst x = 1;\n",
        "export default C;\nclass C {}\n",
        "export default E;\nenum E { A }\n",
        "export default NaN;\n",
    ] {
        assert!(default_of(src).0, "{src}");
    }
}
