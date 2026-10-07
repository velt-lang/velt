//! TS member shorthands: class fields typed by their initializer (`count = 0;`) and constructor
//! parameter properties (`constructor(private readonly x: f64)`).

mod common;

use common::*;

fn class(m: &Module) -> &TypeDecl {
    match &m.items[0].kind {
        ItemKind::Class(c) => c,
        _ => panic!("expected a class"),
    }
}

#[test]
fn field_types_come_from_initializers() {
    let m = parse_ok(
        "class P { done = false; n = 0; x = -1.5; s = \"a\"; t = `b`; b = new Map<string, i64>(); xs = [1, 2]; }",
    );
    let tys: Vec<String> = class(&m).fields.iter().map(|f| ty(&f.ty)).collect();
    assert_eq!(
        tys,
        [
            "boolean",
            "i64",
            "f64",
            "string",
            "string",
            "Map<string, i64>",
            "i64[]"
        ]
    );
    let e = errors("class P { x = f(); } function f(): i64 { return 1; }");
    assert!(
        e.iter()
            .any(|m| m.contains("cannot infer the type of field `x`")),
        "{e:?}"
    );
}

#[test]
fn constructor_parameter_properties_declare_fields() {
    let m = parse_ok(
        "class B extends A { constructor(public x: f64, private readonly m: f64, y: i64) { super(); } }",
    );
    let c = class(&m);
    let fields: Vec<(&str, bool, bool)> = c
        .fields
        .iter()
        .map(|f| (f.name.name.as_str(), f.is_private, f.readonly))
        .collect();
    assert_eq!(fields, [("x", false, false), ("m", true, true)]);
    let ctor = c.constructor.as_ref().expect("constructor");
    assert_eq!(ctor.sig.params.len(), 3);
    // `super()` stays first; the stores follow it.
    assert_eq!(ctor.body.stmts.len(), 3);
    let StmtKind::Expr(e) = &ctor.body.stmts[0].kind else {
        panic!("super call first")
    };
    assert!(matches!(e.kind, ExprKind::Call { .. }));
}

#[test]
fn constructor_visibility() {
    let vis = |src: &str| class(&parse_ok(src)).ctor_visibility;
    assert_eq!(vis("class A { constructor() {} }"), CtorVisibility::Public);
    assert_eq!(
        vis("class A { public constructor() {} }"),
        CtorVisibility::Public
    );
    assert_eq!(
        vis("class A { protected constructor() {} }"),
        CtorVisibility::Protected
    );
    assert_eq!(vis("class A {}"), CtorVisibility::Public);
    // Parameter properties still declare fields.
    let m = parse_ok("class A { private constructor(private readonly x: i64) {} }");
    let c = class(&m);
    assert_eq!(c.ctor_visibility, CtorVisibility::Private);
    assert_eq!(c.fields.len(), 1);
    assert!(c.fields[0].is_private && c.fields[0].readonly);
}

#[test]
fn constructor_modifiers_and_protected_members_are_rejected() {
    let e = errors("class A { static constructor() {} }");
    assert!(
        e.iter()
            .any(|m| m.contains("a constructor can only be `public`, `protected` or `private`")),
        "{e:?}"
    );
    let e = errors("class A { private protected constructor() {} }");
    assert!(!e.is_empty());
    for src in [
        "class A { protected x: i64 = 0; }",
        "class A { protected f() {} }",
    ] {
        let e = errors(src);
        assert!(
            e.iter()
                .any(|m| m.contains("Velt has no `protected` members")),
            "{src}: {e:?}"
        );
    }
}

#[test]
fn private_names_are_class_members() {
    let m = parse_ok(
        "class A { #x = 1; readonly #y: i64; #m(): i64 { return this.#x; } get #v(): i64 { return 1; } static #s(): void {} static readonly #K: i64 = 2; has(o: A): boolean { return #x in o && o.#y > 0; } }",
    );
    let c = class(&m);
    let fields: Vec<(&str, bool)> = c
        .fields
        .iter()
        .map(|f| (f.name.name.as_str(), f.is_private))
        .collect();
    assert_eq!(fields, [("#x", true), ("#y", true), ("#K", true)]);
    let methods: Vec<(&str, bool)> = c
        .methods
        .iter()
        .map(|m| (m.decl.sig.name.name.as_str(), m.is_private))
        .collect();
    assert_eq!(
        methods,
        [("#m", true), ("#v", true), ("#s", true), ("has", false)]
    );
    assert!(c.fields[0].name.is_private_name());
}

#[test]
fn private_names_outside_class_bodies_are_errors() {
    for (src, want) in [
        (
            "class A { private #x = 1; }",
            "an accessibility modifier cannot be used with a private name",
        ),
        (
            "interface I { #f: i64; }",
            "private names are only allowed in class bodies",
        ),
        (
            "type O = { #g: i64 };",
            "private names are only allowed in class bodies",
        ),
        (
            "struct S { #h: i64; }",
            "private names are only allowed in class bodies",
        ),
        (
            "const o = { #k: 1 };",
            "private names are only allowed in class bodies",
        ),
        (
            "class P { constructor(#q: i64) {} }",
            "private names cannot be parameters",
        ),
        (
            "class Q { #constructor(): void {} }",
            "'#constructor' is a reserved word",
        ),
        (
            "function f() { return #x; }",
            "private names are only allowed in class bodies",
        ),
    ] {
        let e = errors(src);
        assert!(e.iter().any(|m| m.contains(want)), "{src}: {e:?}");
    }
}
