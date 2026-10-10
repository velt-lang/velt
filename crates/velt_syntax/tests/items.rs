//! Items: imports/exports, functions, `declare`, struct/class, interface, enum, aliases.

mod common;

use common::*;

#[test]
fn imports_and_exports() {
    let m = parse_ok("import { a, b as c } from \"velt:fs\"; import \"./side\"; export function f() {} export const X = 1; function g() {}");
    let ItemKind::Import(i) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(i.from, "velt:fs");
    assert_eq!(i.names.len(), 2);
    assert_eq!(i.names[1].name.name, "b");
    assert_eq!(i.names[1].alias.as_ref().unwrap().name, "c");
    assert_eq!(
        &"import { a, b as c } from \"velt:fs\";"[i.from_span.lo as usize..i.from_span.hi as usize],
        "\"velt:fs\""
    );
    let ItemKind::Import(i) = &m.items[1].kind else {
        panic!()
    };
    assert!(i.names.is_empty());
    assert!(m.items[2].exported);
    assert!(m.items[3].exported);
    assert!(matches!(m.items[3].kind, ItemKind::Var(_)));
    assert!(!m.items[4].exported);
}

#[test]
fn functions() {
    let m = parse_ok(
        "function a<T extends Show & Eq, U>(x: T, y: U[], z: i64 = 5): Map<T, U> {} async function b(): Promise<void> {} function c(): void {}",
    );
    let ItemKind::Function(f) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(f.sig.generics.len(), 2);
    assert_eq!(
        f.sig.generics[0].bounds.iter().map(ty).collect::<Vec<_>>(),
        vec!["Show", "Eq"]
    );
    assert!(f.sig.generics[1].bounds.is_empty());
    assert_eq!(f.sig.params.len(), 3);
    assert_eq!(ty(&f.sig.params[1].ty), "U[]");
    assert_eq!(sx(f.sig.params[2].default.as_ref().unwrap()), "5");
    assert_eq!(ty(f.sig.ret.as_ref().unwrap()), "Map<T, U>");
    assert!(!f.sig.is_async);
    let ItemKind::Function(f) = &m.items[1].kind else {
        panic!()
    };
    assert!(f.sig.is_async);
    let ItemKind::Function(f) = &m.items[2].kind else {
        panic!()
    };
    assert!(matches!(
        f.sig.ret.as_ref().unwrap().kind,
        TypeExprKind::Void
    ));
    assert!(errors("function f(x) {}")[0].contains("expected `:`"));
}

#[test]
fn declare_function() {
    let m = parse_ok(
        "declare function velt_print(s: string, n: i64): void; export declare function g(): i32;",
    );
    let ItemKind::ExternFn(sig) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(sig.name.name, "velt_print");
    assert_eq!(sig.params.len(), 2);
    assert!(m.items[1].exported);
}

#[test]
fn structs_and_classes() {
    let m = parse_ok(
        "struct Point<T> implements Show, Eq { x: f64, y: f64; readonly z: T = 0; len(): f64 { return 1.0; } scale(k: f64) {} static origin(): Point { return Point { x: 0.0 }; } static: i32; }
         class User { name: string; constructor(name: string, n: i64) { this.name = name; } async greet<U>(u: U): string { return this.name; } static async make(): User { return new User(\"a\", 1); } }",
    );
    let ItemKind::Struct(s) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(s.name.name, "Point");
    assert_eq!(s.generics.len(), 1);
    assert_eq!(s.implements.len(), 2);
    assert_eq!(s.fields.len(), 4);
    assert!(s.fields[2].readonly);
    assert!(s.fields[2].default.is_some());
    assert_eq!(s.fields[3].name.name, "static");
    assert_eq!(s.methods.len(), 3);
    assert!(!s.methods[0].is_static && !s.methods[1].is_static);
    assert!(s.methods[2].is_static);
    assert!(s.constructor.is_none());
    let ItemKind::Class(c) = &m.items[1].kind else {
        panic!()
    };
    assert_eq!(c.fields.len(), 1);
    let ctor = c.constructor.as_ref().unwrap();
    assert_eq!(ctor.sig.name.name, "constructor");
    assert_eq!(ctor.sig.params.len(), 2);
    assert!(c.methods[0].decl.sig.is_async);
    assert_eq!(c.methods[0].decl.sig.generics.len(), 1);
    assert!(c.methods[1].is_static && c.methods[1].decl.sig.is_async);
    assert!(errors("class A { constructor() {} constructor() {} }")[0]
        .contains("duplicate constructor"));
}

#[test]
fn interfaces_enums_aliases() {
    let m = parse_ok(
        "interface Shape<T> extends Base, Other<T> { area(): f64; name(): string, async load<U>(p: U): T }
         enum Dir { Up = \"UP\", Down = \"DOWN\", Left = \"LEFT\" }
         enum Color { Red, Green = 5, Blue = 1 << 3, }
         type Shape = { kind: \"circle\"; r: f64 } | { kind: \"empty\", n: -1 };
         type Id = u64;
         type Pair<A, B> = [A, B];",
    );
    let ItemKind::Interface(i) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(i.extends.len(), 2);
    assert_eq!(i.methods.len(), 3);
    assert!(i.methods[2].sig.is_async);
    assert_eq!(i.methods[2].sig.generics.len(), 1);
    let ItemKind::Enum(e) = &m.items[1].kind else {
        panic!()
    };
    assert_eq!(e.variants.len(), 3);
    assert_eq!(sx(e.variants[1].discriminant.as_ref().unwrap()), "\"DOWN\"");
    let ItemKind::Enum(e) = &m.items[2].kind else {
        panic!()
    };
    assert!(e.variants[0].discriminant.is_none());
    assert_eq!(sx(e.variants[1].discriminant.as_ref().unwrap()), "5");
    assert_eq!(sx(e.variants[2].discriminant.as_ref().unwrap()), "(<< 1 3)");
    let ItemKind::TypeAlias(a) = &m.items[3].kind else {
        panic!()
    };
    assert_eq!(
        ty(&a.ty),
        "({kind: \"circle\"; r: f64} | {kind: \"empty\"; n: -1})"
    );
    let ItemKind::TypeAlias(a) = &m.items[5].kind else {
        panic!()
    };
    assert_eq!(a.generics.len(), 2);
    assert_eq!(ty(&a.ty), "[A, B]");
    assert!(errors("interface I { x; }")[0].contains("expected `:` or `(`"));
    assert!(errors("enum S { Circle(f64) }")[0].contains("cannot have payloads"));
    assert!(errors("enum O<T> { A }")[0].contains("cannot be generic"));
    let m = parse_ok("type T = { a?: i64; b?: A | null };");
    let ItemKind::TypeAlias(a) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(ty(&a.ty), "{a: (i64 | null); b: (A | null)}");
    let m = parse_ok("function f(a?: string) {}");
    let ItemKind::Function(f) = &m.items[0].kind else {
        panic!()
    };
    assert!(f.sig.params[0].optional && f.sig.params[0].default.is_some());
    assert_eq!(ty(&f.sig.params[0].ty), "(string | null)");
    assert!(errors("function f(a?: i64 = 1) {}")[0].contains("optional and have a default"));
}

#[test]
fn top_level_vars() {
    let m = parse_ok("const X: i64 = 1; let y = \"s\"; export const Z = [1, 2];");
    assert_eq!(m.items.len(), 3);
    let ItemKind::Var(v) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(v.kind, VarKind::Const);
}

#[test]
fn soft_keywords_as_identifiers() {
    parse_ok("function f(type: i64, from: string, of: i32) { const static = 1; let readonly = shared(x); console.log(type, from, of, static); }");
    parse_ok("function f() { for (const of of ofs) {} }");
}

#[test]
fn mut_is_rejected_with_a_fix() {
    let want = "`mut` is not needed: mutation is inferred";
    for src in [
        "function f(mut xs: i64[]) {}",
        "class C { n: i64 = 0; mut bump() { this.n += 1; } }",
        "interface I { mut reset(): void; }",
        "type F = (mut a: i64[]) => void;",
        "function g() { const h = (mut x: i64[]) => x.length; }",
    ] {
        let errs = errors(src);
        assert_eq!(errs, vec![want.to_string()], "{src}");
    }
    // Still an ordinary identifier.
    parse_ok("function f(mut: i64): i64 { const mut2 = mut; return mut2; }");
}

#[test]
fn throws_clauses() {
    let m = parse_ok(
        "function a(): i64 throws NotFound | Forbidden {} function b() throws E {} async function c(): Promise<void> throws E {} class K { constructor() throws E {} m(): void throws E {} } interface I { m(): i64 throws E; }",
    );
    let sig = |i: usize| match &m.items[i].kind {
        ItemKind::Function(f) => f.sig.clone(),
        k => panic!("expected function, got {k:?}"),
    };
    assert_eq!(
        ty(sig(0).throws.as_ref().unwrap()),
        "(NotFound | Forbidden)"
    );
    assert_eq!(ty(sig(0).ret.as_ref().unwrap()), "i64");
    assert!(sig(1).ret.is_none());
    assert_eq!(ty(sig(1).throws.as_ref().unwrap()), "E");
    assert_eq!(ty(sig(2).ret.as_ref().unwrap()), "Promise<void>");
    assert_eq!(ty(sig(2).throws.as_ref().unwrap()), "E");
    let ItemKind::Class(k) = &m.items[3].kind else {
        panic!()
    };
    assert!(k.constructor.as_ref().unwrap().sig.throws.is_some());
    assert!(k.methods[0].decl.sig.throws.is_some());
    let ItemKind::Interface(i) = &m.items[4].kind else {
        panic!()
    };
    assert!(i.methods[0].sig.throws.is_some());
    // `throws` stays an ordinary identifier elsewhere.
    parse_ok("function f() { const throws = 1; console.log(throws); }");
}

#[test]
fn readonly_fields_in_object_types() {
    let m = parse_ok(
        "type U = { readonly id: i64; readonly name?: string; readonly: bool, readonly?: i64 };",
    );
    let ItemKind::TypeAlias(a) = &m.items[0].kind else {
        panic!()
    };
    let TypeExprKind::Object(fields) = &a.ty.kind else {
        panic!()
    };
    let got: Vec<(&str, bool, bool)> = fields
        .iter()
        .map(|f| (f.name.name.as_str(), f.readonly, f.optional))
        .collect();
    assert_eq!(
        got,
        [
            ("id", true, false),
            ("name", true, true),
            ("readonly", false, false),
            ("readonly", false, true)
        ]
    );
}

#[test]
fn class_expression_bound_to_a_variable() {
    let m = parse_ok(
        "export const A = class extends B { x = 1; }; let C = class C {}
function f() { const D = class { m() {} }; }",
    );
    let ItemKind::Class(a) = &m.items[0].kind else {
        panic!()
    };
    assert!(m.items[0].exported);
    assert_eq!(a.name.name, "A");
    assert!(a.extends.is_some());
    assert_eq!(a.fields.len(), 1);
    let ItemKind::Class(c) = &m.items[1].kind else {
        panic!()
    };
    assert_eq!(c.name.name, "C");
    let ItemKind::Function(f) = &m.items[2].kind else {
        panic!()
    };
    let StmtKind::Item(item) = &f.body.stmts[0].kind else {
        panic!()
    };
    let ItemKind::Class(d) = &item.kind else {
        panic!()
    };
    assert_eq!((d.name.name.as_str(), d.methods.len()), ("D", 1));
}
