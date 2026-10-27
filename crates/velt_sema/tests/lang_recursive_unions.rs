//! Union aliases that name themselves (`crate::recursive_unions`): the ones TypeScript rejects
//! too are errors at each alias, never a crash.

mod common;

use common::programs::err_src;

/// How many times `alias` is reported as referring to itself.
fn reported(e: &str, alias: &str) -> usize {
    e.matches(&format!("type alias `{alias}` refers to itself"))
        .count()
}

#[test]
fn aliases_holding_each_other_directly_are_errors() {
    // TS2456 on both; this overflowed the stack once.
    let e = err_src(
        "type A = number | B | A[]; type B = string | A | B[];\n\
         const a: A = 1; console.log(a);",
    );
    assert_eq!((reported(&e, "A"), reported(&e, "B")), (1, 1), "{e}");
    let e = err_src(
        "type A<T> = T | B<T> | A<T>[]; type B<T> = string | A<T> | B<T>[];\n\
         const a: A<number> = 1; console.log(a);",
    );
    assert_eq!((reported(&e, "A"), reported(&e, "B")), (1, 1), "{e}");
}

#[test]
fn an_alias_named_by_a_member_keeps_its_own_error() {
    // `B` is reported where it is declared, as on main and in tsc, not only `A`.
    let e = err_src("type B = string | B; type A = number | B | A[]; const a: A = 1;");
    assert!(e.contains("1:6"), "{e}");
    assert_eq!(reported(&e, "B"), 1, "{e}");
    let e = err_src(
        "type B<T> = T | B<T>; type A<T> = number | B<T> | A<T>[]; const a: A<string> = 1;",
    );
    assert_eq!(reported(&e, "B"), 1, "{e}");
    let e = err_src("type A = number | Nope | A[]; const a: A = 1;");
    assert!(e.contains("cannot find type `Nope`"), "{e}");
}
