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
            "bool",
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
