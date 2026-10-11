//! Type expressions.

mod common;

use common::*;

fn alias_ty(src: &str) -> String {
    let m = parse_ok(&format!("type T = {};", src));
    let ItemKind::TypeAlias(a) = &m.items[0].kind else {
        panic!()
    };
    ty(&a.ty)
}

#[test]
fn type_expressions() {
    assert_eq!(alias_ty("i64"), "i64");
    assert_eq!(alias_ty("fs.File"), "fs.File");
    assert_eq!(alias_ty("Map<string, i64>"), "Map<string, i64>");
    assert_eq!(
        alias_ty("Map<K, Array<Array<V>>>"),
        "Map<K, Array<Array<V>>>"
    );
    assert_eq!(alias_ty("i64[]"), "i64[]");
    assert_eq!(alias_ty("i64[][]"), "i64[][]");
    assert_eq!(alias_ty("[i64, string]"), "[i64, string]");
    assert_eq!(alias_ty("[]"), "[]");
    assert_eq!(
        alias_ty("(a: i64, b: string) => void"),
        "fn(i64, string) => void"
    );
    assert_eq!(alias_ty("() => i64"), "fn() => i64");
    assert_eq!(
        alias_ty("(i64) => (x: i64) => i64"),
        "fn(i64) => fn(i64) => i64"
    );
    assert_eq!(alias_ty("T | null"), "(T | null)");
    assert_eq!(alias_ty("| A | B | C"), "(A | B | C)");
    assert_eq!(alias_ty("(A | B)[]"), "(A | B)[]");
    assert_eq!(alias_ty("(i64)"), "i64");
    assert_eq!(alias_ty("void"), "void");
    assert_eq!(alias_ty("null"), "null");
    assert_eq!(
        alias_ty("Promise<Result<i64, string>>"),
        "Promise<Result<i64, string>>"
    );
    assert_eq!(alias_ty("(a: T) => void"), "fn(T) => void");
    // Number literal types may be negative, as in TypeScript.
    assert_eq!(alias_ty("-1"), "-1");
    assert_eq!(alias_ty("-1 | 0 | 1.5"), "(-1 | 0 | 1.5)");
}

#[test]
fn function_types_with_throws() {
    assert_eq!(
        alias_ty("(x: i64) => string throws E"),
        "fn(i64) => string throws E"
    );
    assert_eq!(
        alias_ty("(x: i64) => string throws A | B"),
        "fn(i64) => string throws (A | B)"
    );
    assert_eq!(
        alias_ty("() => Promise<void> throws E"),
        "fn() => Promise<void> throws E"
    );
    check(
        "(x: i64): i64 throws E => x",
        "(arrow (x: i64): i64 throws E x)",
    );
    check(
        "async (): Promise<void> throws E => {}",
        "(async arrow (): Promise<void> throws E {0 stmts})",
    );
}

#[test]
fn intersection_types() {
    assert_eq!(alias_ty("A & B"), "(A & B)");
    assert_eq!(alias_ty("A & B & { c: i64 }"), "(A & B & {c: i64})");
    // `&` binds tighter than `|`, and may lead like `|`.
    assert_eq!(alias_ty("A & B | C"), "((A & B) | C)");
    assert_eq!(alias_ty("A | B & C"), "(A | (B & C))");
    assert_eq!(alias_ty("(A | B) & C"), "((A | B) & C)");
    assert_eq!(alias_ty("& A & B"), "(A & B)");
    assert_eq!(alias_ty("(A & B)[]"), "(A & B)[]");
    assert_eq!(
        alias_ty("string & { __brand: \"Id\" }"),
        "(string & {__brand: \"Id\"})"
    );
    // A cast keeps its restricted grammar: `x as A & B` is a bitwise and.
    check("x as i64 & m", "(& (as x i64) m)");
}

#[test]
fn indexed_access_types() {
    assert_eq!(alias_ty("T[\"k\"]"), "T[\"k\"]");
    assert_eq!(alias_ty("T[\"a\" | \"b\"]"), "T[(\"a\" | \"b\")]");
    assert_eq!(alias_ty("T[\"k\"][]"), "T[\"k\"][]");
    assert_eq!(alias_ty("A & B[\"k\"]"), "(A & B[\"k\"])");
    // Only string keys: `T[]` stays an array type.
    assert_eq!(alias_ty("T[]"), "T[]");
}

#[test]
fn type_queries_and_keyof() {
    assert_eq!(alias_ty("typeof x"), "typeof x");
    assert_eq!(alias_ty("typeof cfg.port"), "typeof cfg.port");
    assert_eq!(alias_ty("ReturnType<typeof f>"), "ReturnType<typeof f>");
    assert_eq!(alias_ty("typeof a | null"), "(typeof a | null)");
    assert_eq!(alias_ty("typeof xs[]"), "typeof xs[]");
    assert_eq!(alias_ty("keyof typeof o"), "keyof typeof o");
    assert_eq!(alias_ty("keyof T | null"), "(keyof T | null)");
    // `keyof` binds looser than `[]`; a type named `keyof` stays a name.
    assert_eq!(alias_ty("keyof T[]"), "keyof T[]");
    assert_eq!(alias_ty("keyof"), "keyof");
    // In a cast too: `x as typeof y`, `k as keyof T`.
    check("x as typeof y", "(as x typeof y)");
    check("k as keyof T", "(as k keyof T)");
    // Not supported yet: reported, parsed as an empty query.
    assert_eq!(
        errors("type T = typeof import(\"./m\");"),
        ["`typeof import(...)` types are not supported yet (#936)"]
    );
    assert_eq!(
        errors("type T = typeof f<string>;"),
        ["type arguments after `typeof f` (an instantiation expression) are not supported yet (#936)"]
    );
}

#[test]
fn index_signatures() {
    assert_eq!(alias_ty("{ [key: string]: i64 }"), "{[string]: i64}");
    assert_eq!(
        alias_ty("{ readonly [k: string]: V[] }"),
        "{readonly [string]: V[]}"
    );
    assert_eq!(
        alias_ty("{ a: i64; [k: string]: i64, }"),
        "{a: i64; [string]: i64}"
    );
    // An interface with an index signature is an alias of the object type it spells out.
    let m = parse_ok("interface D<T> { readonly [key: string]: T; n?: T }");
    let ItemKind::TypeAlias(a) = &m.items[0].kind else {
        panic!("expected an alias")
    };
    assert_eq!(a.name.name, "D");
    assert_eq!(a.generics.len(), 1);
    assert_eq!(ty(&a.ty), "{n: (T | null); readonly [string]: T}");
    assert_eq!(
        errors("interface W { [k: string]: i64; size(): i64; }"),
        ["an interface with an index signature and methods is not supported yet (#937)"]
    );
    assert_eq!(
        errors("type T = { [k: string]: i64; [i: number]: i64 };"),
        ["more than one index signature in a type is not supported yet (#937)"]
    );
    assert_eq!(
        errors("type T = { [k: string]?: i64 };"),
        ["an index signature cannot be optional"]
    );
}
