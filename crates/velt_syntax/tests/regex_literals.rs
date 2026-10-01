//! Regular expression literals: `/re/flags` where an operand may start, division elsewhere.

mod common;

use common::*;

#[test]
fn regex_literals_become_regexp_constructions() {
    let e = expr("/a[/]b\\/c/gi");
    let ExprKind::New { class, args } = &e.kind else {
        panic!("expected new RegExp, got {}", sx(&e))
    };
    assert_eq!(ty(class), "RegExp");
    assert_eq!(lit_str(&args[0]), "a[/]b\\/c");
    assert_eq!(lit_str(&args[1]), "gi");
    let e = expr("f(/x+/, y)");
    assert!(sx(&e).contains("new"), "{}", sx(&e));
}

#[test]
fn slash_after_an_operand_is_division() {
    for src in [
        "a / b / c",
        "(a + b) / 2",
        "xs[0] / 2",
        "f() / g() / h()",
        "1 / 2 / 3",
    ] {
        let e = expr(src);
        assert!(!sx(&e).contains("new"), "{src}: {}", sx(&e));
    }
}

fn lit_str(e: &Expr) -> &str {
    match &e.kind {
        ExprKind::Lit(Lit::Str(s)) => s,
        _ => panic!("expected a string literal"),
    }
}
