//! Recursion in functions whose return types are inferred (#75): uses outside `return`
//! expressions need no annotation; uses the `return`s depend on do.

mod common;

use common::hir_walk::func;
use common::programs::{err_src, ok_src};
use velt_sema::hir::TyKind;

const NODE: &str = "class Node { kids: Node[] = []; }";

fn ret_is_int(src: &str, name: &str) {
    let p = ok_src(&format!("{NODE} {src} function main() {{}}"));
    let f = func(&p, name);
    assert!(matches!(p.types.kind(f.ret), TyKind::Int(_)), "{name}");
}

#[test]
fn self_use_outside_returns_is_inferred() {
    ret_is_int(
        "function count(n: Node) { let total = 1; for (const c of n.kids) total += count(c); return total; }",
        "count",
    );
    ret_is_int(
        "function depth(n: Node) { let d = 0; if (n.kids.length > 0 && depth(n.kids[0]) > 3) { d = 1; } return d; }",
        "depth",
    );
    ret_is_int(
        "function size<T>(xs: T[], i: i64) { let n = 0; if (i < 3) { n = 1 + size(xs, i + 1); } return n; }",
        "size",
    );
    ret_is_int(
        "class W { walk(n: Node) { let s = 1; for (const c of n.kids) { s += this.walk(c); } return s; } }",
        "W.walk",
    );
    ret_is_int(
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
