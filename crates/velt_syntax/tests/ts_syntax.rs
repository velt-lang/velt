//! Newer TypeScript syntax: `satisfies`, `let x!: T`, `const` type parameters, tuple labels,
//! several declarators, `declare` fields, `readonly` array types, type predicates, method
//! signatures in object types and methods in object literals.

mod common;

use common::*;

fn var(item: &Item) -> &VarDecl {
    match &item.kind {
        ItemKind::Var(v) => v,
        k => panic!("expected a variable, got {k:?}"),
    }
}

fn alias_ty(m: &Module, i: usize) -> String {
    match &m.items[i].kind {
        ItemKind::TypeAlias(a) => ty(&a.ty),
        k => panic!("expected a type alias, got {k:?}"),
    }
}

#[test]
fn satisfies_is_a_call_of_the_prelude_function() {
    let e = expr("x = { a: 1 } satisfies T");
    let ExprKind::Assign { value, .. } = &e.kind else {
        panic!("expected an assignment")
    };
    let ExprKind::Call {
        callee, type_args, ..
    } = &value.kind
    else {
        panic!("expected a call, got {:?}", value.kind)
    };
    assert!(matches!(&callee.kind, ExprKind::Ident(i) if i.name == "__satisfies"));
    assert_eq!(ty(&type_args[0]), "T");
}

#[test]
fn several_declarators_and_definite_assignment() {
    let m = parse_ok("export let a: number, b = 2, c!: string;");
    assert_eq!(m.items.len(), 3);
    assert!(m.items.iter().all(|i| i.exported));
    assert_eq!(ty(var(&m.items[2]).ty.as_ref().unwrap()), "string");
    let m = parse_ok("function f() { let i: number, j = 1; const k = 2, l = 3; }");
    assert_eq!(body(&m).len(), 4);
}

#[test]
fn const_type_parameters_and_tuple_labels() {
    let m = parse_ok("function f<T, const K>(x: K): T[] {} type W = [kind: string, b64: string];");
    let ItemKind::Function(f) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(f.sig.generics[1].name.name, "K");
    assert_eq!(alias_ty(&m, 1), "[string, string]");
}

#[test]
fn declare_fields() {
    let m = parse_ok("class E { declare readonly cause?: Error; }");
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(c.fields[0].name.name, "cause");
    assert!(c.fields[0].readonly && c.fields[0].default.is_none());
    assert_eq!(
        errors("class E { declare x: number = 1; }"),
        vec!["initializers are not allowed in ambient contexts"]
    );
}

#[test]
fn readonly_array_and_tuple_types() {
    let m = parse_ok(
        "type A = readonly number[]; type B = readonly [string, number]; type C = readonly (readonly [string, V])[];",
    );
    assert_eq!(alias_ty(&m, 0), "ReadonlyArray<number>");
    assert_eq!(alias_ty(&m, 1), "[string, number]");
    assert_eq!(alias_ty(&m, 2), "ReadonlyArray<[string, V]>");
    assert_eq!(
        errors("type R = readonly string;"),
        vec!["'readonly' type modifier is only permitted on array and tuple literal types"]
    );
}

#[test]
fn type_predicates() {
    let m = parse_ok(
        "function a(x: unknown): x is Foo { return true; } function b(x: number): asserts x is Foo {} function c(x: number): asserts x {} type F = (x: unknown) => x is Foo;",
    );
    let rets: Vec<String> = m.items[..3]
        .iter()
        .map(|i| match &i.kind {
            ItemKind::Function(f) => ty(f.sig.ret.as_ref().unwrap()),
            _ => panic!(),
        })
        .collect();
    assert_eq!(rets, vec!["x is Foo", "asserts x is Foo", "asserts x"]);
    assert_eq!(alias_ty(&m, 3), "fn(unknown) => x is Foo");
    parse_ok("const g = (n: Node | null): n is Element => n !== null;");
}

#[test]
fn method_signatures_in_object_types_and_optional_interface_methods() {
    let m = parse_ok(
        "type S = { stop(): void; size(n: number): number; label?(p: string): string }; interface H { q?(s: string): string | null; run(x: number): number }",
    );
    assert_eq!(
        alias_ty(&m, 0),
        "{stop: fn() => void; size: fn(number) => number; label: (fn(string) => string | null)}"
    );
    let ItemKind::Interface(i) = &m.items[1].kind else {
        panic!()
    };
    assert_eq!(i.fields[0].name.name, "q");
    assert!(i.fields[0].optional);
    assert_eq!(i.methods[0].sig.name.name, "run");
    assert_eq!(
        errors("type G = { run<T>(f: () => T): T };"),
        vec!["generic method signatures are only supported in interfaces: declare `run` in an interface"]
    );
    assert_eq!(
        errors("interface I { wrap?<A>(a: A): A }"),
        vec!["an optional method signature can't be generic yet: `wrap?` is a field of function type, and function types have no type parameters"]
    );
}

#[test]
fn object_literal_methods_are_arrow_properties() {
    let e = expr("x = { get(obj, prop) { return 1; }, async load(n: number) { return n; } }");
    let ExprKind::Assign { value, .. } = &e.kind else {
        panic!()
    };
    let ExprKind::Object(ps) = &value.kind else {
        panic!()
    };
    for p in ps {
        let ObjectProp::KeyValue(_, v) = p else {
            panic!("expected a property, got {p:?}")
        };
        assert!(matches!(v.kind, ExprKind::Arrow { .. }));
    }
    assert_eq!(
        errors("const o = { n: 1, m() { return this.n; } };"),
        vec!["`this` in the object-literal method `m` is not supported yet"]
    );
    assert_eq!(
        errors("const o = { get v() { return 1; } };"),
        vec!["getters and setters in object literals are not supported yet"]
    );
}
