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

/// The function type alias `T` in `src`: (rest, type parameter count, construct).
fn fn_type_flags(src: &str) -> (bool, usize, bool) {
    let m = parse_ok(&format!("type T = {src};"));
    let ItemKind::TypeAlias(a) = &m.items[0].kind else {
        panic!("expected a type alias")
    };
    let (construct, t) = match &a.ty.kind {
        TypeExprKind::Construct(t) => (true, &**t),
        _ => (false, &a.ty),
    };
    let (n, t) = match &t.kind {
        TypeExprKind::GenericFunction { type_params, ty } => (type_params.len(), &**ty),
        _ => (0, t),
    };
    match &t.kind {
        TypeExprKind::Function { rest, .. } => (*rest, n, construct),
        k => panic!("expected a function type, got {k:?}"),
    }
}

#[test]
fn rest_generic_and_construct_function_types() {
    assert_eq!(
        fn_type_flags("(event: string, ...args: number[]) => void"),
        (true, 0, false)
    );
    assert_eq!(
        fn_type_flags("<T>(description: string) => Token<T>"),
        (false, 1, false)
    );
    assert_eq!(
        fn_type_flags("new (buffer: ArrayBuffer) => View"),
        (false, 0, true)
    );
    assert_eq!(
        fn_type_flags("new <T>(...xs: T[]) => Box<T>"),
        (true, 1, true)
    );
    assert_eq!(fn_type_flags("(a: A) => R"), (false, 0, false));
    assert_eq!(
        alias_ty("(e: string, ...xs: i64[]) => void"),
        "fn(string, i64[]) => void"
    );
    assert!(errors("type T = (...xs: i64[], y: i64) => void;")
        .iter()
        .any(|e| e.contains("a rest parameter must be the last parameter")));
}

#[test]
fn function_type_parameters_may_be_patterns() {
    assert_eq!(
        alias_ty("([a, b]: [i64, i64]) => i64"),
        "fn([i64, i64]) => i64"
    );
    assert_eq!(
        alias_ty("(x: string, { a, b: [c] }: P) => void"),
        "fn(string, P) => void"
    );
    // Without a `:`, `[`/`{` starts the parameter's type.
    assert_eq!(
        alias_ty("([i64, string]) => void"),
        "fn([i64, string]) => void"
    );
}
