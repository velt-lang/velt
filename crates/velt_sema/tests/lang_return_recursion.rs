//! Recursion in functions whose return types are inferred (#75): uses outside `return`
//! expressions need no annotation; uses the `return`s depend on do.

mod common;

use common::hir_walk::func;
use common::programs::{err_src, ok_src};
use velt_sema::hir::TyKind;

const NODE: &str = "class Node { kids: Node[] = []; }";

/// The function returns a number (its result is inferred from locals declared from literals).
fn ret_is_number(src: &str, name: &str) {
    let p = ok_src(&format!("{NODE} {src} function main() {{}}"));
    let f = func(&p, name);
    assert!(matches!(p.types.kind(f.ret), TyKind::Float(_)), "{name}");
}

#[test]
fn self_use_outside_returns_is_inferred() {
    ret_is_number(
        "function count(n: Node) { let total = 1; for (const c of n.kids) total += count(c); return total; }",
        "count",
    );
    ret_is_number(
        "function depth(n: Node) { let d = 0; if (n.kids.length > 0 && depth(n.kids[0]) > 3) { d = 1; } return d; }",
        "depth",
    );
    ret_is_number(
        "function size<T>(xs: T[], i: i64) { let n = 0; if (i < 3) { n = 1 + size(xs, i + 1); } return n; }",
        "size",
    );
    ret_is_number(
        "class W { walk(n: Node) { let s = 1; for (const c of n.kids) { s += this.walk(c); } return s; } }",
        "W.walk",
    );
    ret_is_number(
        "function a(n: Node) { let w = 1; for (const c of n.kids) w += b(c); return w; } function b(n: Node) { return a(n) * 2; }",
        "b",
    );
}

#[test]
fn self_use_in_returns_needs_an_annotation() {
    for (src, want) in [
        (
            "function f(n: i64) { if (n < 2) { return n; } return f(n - 1) + 1; }",
            "function `f` needs a return type annotation",
        ),
        (
            "function f(n: i64) { if (n < 2) { return n; } const m = f(n - 1); return m; }",
            "function `f` needs a return type annotation",
        ),
        (
            "function even(n: i64) { if (n == 0) { return true; } return odd(n - 1); } function odd(n: i64) { if (n == 0) { return false; } return even(n - 1); }",
            "`even` → `odd` → `even`",
        ),
    ] {
        let r = err_src(&format!("{src} function main() {{}}"));
        assert!(r.contains(want), "{src}: {r}");
        assert_eq!(r.matches("needs a return type annotation").count(), 1, "{r}");
    }
}

/// A chain of unannotated functions, each needing the next one's result inside nested `if`s
/// and parentheses: inferring it nests every body in the one before, which is reported once the
/// stack budget is used up rather than overflowing the checking thread's stack.
fn deep_chain(n: usize, depth: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        let mut body = String::new();
        if i + 1 < n {
            let call = format!("g{}(n - 1)", i + 1);
            let mut inner = format!(
                "const v = {}{call} + 1{};\n",
                "(".repeat(depth),
                ")".repeat(depth)
            );
            for d in 0..depth {
                inner = format!("if (n > {d}) {{\n{inner}}}\n");
            }
            body = inner;
        }
        out.push_str(&format!(
            "function g{i}(n: number) {{\n{body}  return n + 1;\n}}\n"
        ));
    }
    out.push_str("function main() {\n  console.log(g0(3));\n}\n");
    out
}

#[test]
fn deep_chains_of_inferred_functions_are_reported_not_overflowed() {
    let r = err_src(&deep_chain(3000, 10));
    assert!(r.contains("nests too many functions"), "{r}");
    // A short chain is inferred.
    ok_src(&deep_chain(50, 10));
}
