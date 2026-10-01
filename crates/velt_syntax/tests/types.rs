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
