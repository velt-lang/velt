//! M2 surface: inheritance (`extends`, `override`, `super`), optional fields, interface fields and
//! default methods, `extend` blocks and optional call/index chaining.

mod common;

use common::*;

fn class(m: &Module, i: usize) -> &TypeDecl {
    match &m.items[i].kind {
        ItemKind::Class(c) | ItemKind::Struct(c) => c,
        k => panic!("expected class/struct, got {:?}", k),
    }
}

#[test]
fn class_extends_override_super() {
    let m = parse_ok(
        "class Dog<T> extends Animal<T> implements A, B {
           tricks?: i64;
           constructor(name: string) { super(name); }
           override speak(): string { return super.speak(); }
           static override make(): Dog<T> { return new Dog(\"x\"); }
         }",
    );
    let c = class(&m, 0);
    assert_eq!(ty(c.extends.as_ref().unwrap()), "Animal<T>");
    assert_eq!(c.implements.len(), 2);
    assert!(c.fields[0].optional);
    assert!(c.methods[0].is_override && !c.methods[0].is_static);
    let make = &c.methods[1];
    assert!(make.is_static && make.is_override);
    let ctor = c.constructor.as_ref().unwrap();
    let StmtKind::Expr(e) = &ctor.body.stmts[0].kind else {
        panic!()
    };
    assert_eq!(sx(e), "(call super [name])");
    let StmtKind::Return(Some(e)) = &c.methods[0].decl.body.stmts[0].kind else {
        panic!()
    };
    assert_eq!(sx(e), "(call (. super speak) [])");
}

#[test]
fn override_is_still_a_name() {
    let m = parse_ok("class C { override: i64; override(): i64 { return 1; } }");
    let c = class(&m, 0);
    assert_eq!(c.fields[0].name.name, "override");
    assert_eq!(c.methods[0].decl.sig.name.name, "override");
    assert!(!c.methods[0].is_override);
}

#[test]
fn inheritance_errors() {
    let (m, d) = parse("struct P extends Q { x: i64; }");
    assert!(
        d[0].message.contains("a struct cannot use `extends`"),
        "{:?}",
        d
    );
    assert!(class(&m, 0).extends.is_none());
    assert_eq!(class(&m, 0).fields.len(), 1);
    assert!(errors("class A extends B, C {}")[0].contains("at most one class"));
    assert!(errors("class A { override x: i64; }")[0].contains("not allowed on fields"));
    assert!(errors("function f() { super; }")[0].contains("`super` must be followed"));
}

#[test]
fn optional_fields() {
    let m = parse_ok("struct S { a?: string; readonly b?: i64 = 1, c: f64 }");
    let f = &class(&m, 0).fields;
    assert!(f[0].optional && f[1].optional && !f[2].optional);
    assert!(f[1].readonly && f[1].default.is_some());
    assert!(errors("struct S { a?(): i64 {} }")[0].contains("expected `:`"));
}

#[test]
fn private_and_static_members() {
    let m = parse_ok(
        "class C { private a: i64; public b: i64; static readonly PI: f64 = 3.14; \
         private static helper(): i64 { return 1; } private bump() {} private: i64; }",
    );
    let c = class(&m, 0);
    assert!(c.fields[0].is_private && !c.fields[0].is_static);
    assert!(!c.fields[1].is_private);
    assert!(c.fields[2].is_static && c.fields[2].readonly && !c.fields[2].is_private);
    assert_eq!(c.fields[3].name.name, "private");
    assert!(c.methods[0].is_private && c.methods[0].is_static);
    assert!(c.methods[1].is_private);
    assert!(errors("interface I { private f(): i64; }")[0].contains("not allowed on interface"));
    assert!(errors("interface I { private x: i64; }")[0].contains("cannot be `private`"));
}

#[test]
fn getters() {
    let m = parse_ok(
        "class C { get size(): usize { return 1; } get(k: i64): i64 { return k; } get: i64; \
         private get n(): i64 { return 0; } }",
    );
    let c = class(&m, 0);
    assert!(c.methods[0].is_getter && c.methods[0].decl.sig.name.name == "size");
    assert!(!c.methods[1].is_getter && c.methods[1].decl.sig.name.name == "get");
    assert_eq!(c.fields[0].name.name, "get");
    assert!(c.methods[2].is_getter && c.methods[2].is_private);
    assert!(errors("class C { get x(a: i64): i64 { return a; } }")[0].contains("parameters"));
    assert!(errors("class C { get x() {} }")[0].contains("return type"));
    assert!(errors("class C { static get x(): i64 { return 1; } }")[0].contains("`static`"));
    let i = parse_ok("interface I { get area(): f64; }");
    let ItemKind::Interface(d) = &i.items[0].kind else {
        panic!("interface")
    };
    assert!(d.methods[0].is_getter);
}

#[test]
fn setters() {
    let m = parse_ok(
        "class C { get size(): i64 { return 1; } set size(v: i64) {} set(k: i64) {} set: i64; \
         private set n(v: i64) {} }",
    );
    let c = class(&m, 0);
    assert!(c.methods[0].is_getter && !c.methods[0].is_setter);
    assert!(c.methods[1].is_setter && c.methods[1].decl.sig.name.name == "size");
    assert!(!c.methods[2].is_setter && c.methods[2].decl.sig.name.name == "set");
    assert_eq!(c.fields[0].name.name, "set");
    assert!(c.methods[3].is_setter && c.methods[3].is_private);
    assert!(errors("class C { set x() {} }")[0].contains("exactly one parameter"));
    assert!(errors("class C { set x(a: i64, b: i64) {} }")[0].contains("exactly one parameter"));
    assert!(errors("class C { set x(a: i64): void {} }")[0].contains("return type"));
    assert!(errors("class C { static set x(a: i64) {} }")[0].contains("`static`"));
    let i = parse_ok("interface I { set area(v: f64); }");
    let ItemKind::Interface(d) = &i.items[0].kind else {
        panic!("interface")
    };
    assert!(d.methods[0].is_setter);
}

#[test]
fn interface_fields_and_default_methods() {
    let m = parse_ok(
        "interface Named extends Base {
           name: string;
           nick?: string,
           greet(): string { return `hi ${this.name}`; }
           area(): f64;
           rename(n: string) { this.name = n; }
           async load(): void
         }",
    );
    let ItemKind::Interface(i) = &m.items[0].kind else {
        panic!()
    };
    let fields: Vec<_> = i
        .fields
        .iter()
        .map(|f| (f.name.name.as_str(), f.optional))
        .collect();
    assert_eq!(fields, vec![("name", false), ("nick", true)]);
    let methods: Vec<_> = i
        .methods
        .iter()
        .map(|m| (m.sig.name.name.as_str(), m.body.is_some(), m.sig.is_async))
        .collect();
    assert_eq!(
        methods,
        vec![
            ("greet", true, false),
            ("area", false, false),
            ("rename", true, false),
            ("load", false, true),
        ]
    );
    assert!(errors("interface I { x: i64 = 1; }")[0].contains("cannot have a default"));
    assert!(errors("interface I { static f(): i64; }")[0].contains("not allowed on interface"));
}

#[test]
fn extend_blocks() {
    let m = parse_ok(
        "extend<T> Array<T> { first(): T | null { return this[0]; } clear() {} }
         extend string { shout(): string { return this; } }
         function extend(x: i64): i64 { return extend(x); }",
    );
    let ItemKind::Extend(e) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(e.generics.len(), 1);
    assert_eq!(ty(&e.target), "Array<T>");
    assert_eq!(e.methods.len(), 2);
    assert!(matches!(&m.items[1].kind, ItemKind::Extend(e) if ty(&e.target) == "string"));
    assert!(matches!(m.items[2].kind, ItemKind::Function(_)));

    let (m, d) = parse("export extend Foo { f() {} }");
    assert!(d[0].message.contains("cannot be exported"), "{:?}", d);
    assert!(!m.items[0].exported);
    let msgs = errors("extend Foo { x: i64; constructor() {} f() {} }");
    assert_eq!(msgs.len(), 2, "{:?}", msgs);
    assert!(msgs.iter().all(|m| m.contains("can only add methods")));
}

#[test]
fn optional_call_and_index() {
    let e = expr("f?.(1)?.[i]?.x");
    let ExprKind::Member {
        object, optional, ..
    } = &e.kind
    else {
        panic!()
    };
    assert!(optional);
    let ExprKind::Index {
        object, optional, ..
    } = &object.kind
    else {
        panic!()
    };
    assert!(optional);
    let ExprKind::Call { optional, .. } = &object.kind else {
        panic!()
    };
    assert!(optional);
    assert_eq!(sx(&e), "(?. ([] (call f [1]) i) x)");
    let ExprKind::Call { optional, .. } = &expr("f(1)").kind else {
        panic!()
    };
    assert!(!optional);
    assert!(errors("function f() { a?.; }")[0].contains("after `?.`"));
}
