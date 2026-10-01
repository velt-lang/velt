//! Flow narrowing: literal comparisons that narrow unions (and the impossible ones that are
//! rejected), values narrowed to `never`, and narrowing of captured variables and fields.

mod common;

use common::programs::{err_src, ok_src};

#[test]
fn impossible_literal_comparison_is_an_error() {
    let r = err_src(
        "type Sh = { kind: \"pt\"; x: i64 } | { kind: \"box\"; x: i64 };
         function f(v: Sh): i64 {
           if (v.kind === \"box\") { if (v.kind != \"pt\") { return 1; } else { return v.x; } }
           return 0;
         }
         function g(k: \"box\"): bool { return k == \"pt\"; }
         function main() { console.log(f({ kind: \"box\", x: 2 }), g(\"box\")); }",
    );
    assert!(
        r.contains("`\"pt\"` is not a possible value of `v.kind`"),
        "{r}"
    );
    assert!(r.contains("`\"pt\"` is not a possible value of `k`"), "{r}");
}

#[test]
fn reading_through_never_is_unreachable_not_an_error() {
    ok_src(
        "type Sh = { kind: \"pt\"; x: i64 } | { kind: \"box\"; x: i64 };
         function f(v: Sh): i64 {
           if (v.kind === \"box\") { if (v.kind === \"box\") { return 1; } else { return v.x; } }
           return 0;
         }
         function main() { console.log(f({ kind: \"box\", x: 2 })); }",
    );
}

#[test]
fn switch_default_after_case_null_is_non_null() {
    ok_src(
        "function describe(x: string | null): string {
           switch (x) { case null: return \"none\"; default: return x; } }
         function main() { console.log(describe(null)); }",
    );
    let r = err_src(
        "function f(x: string | null): string {
           switch (x) { case null: default: return x; } }
         function main() { console.log(f(null)); }",
    );
    assert!(r.contains("mismatched types"), "{r}");
}

#[test]
fn narrowing_applies_to_captures() {
    ok_src(
        "function keep(want: bool | null, xs: bool[]): bool[] {
           return xs.filter((x) => want == null || x === want); }
         function f(u: string | null): usize[] {
           if (u != null) { return [1].map((x) => u.length); } return []; }
         function main() { console.log(keep(true, [true]), f(\"a\")); }",
    );
    let r = err_src(
        "function f(u: string | null) {
           if (u != null) { [1].forEach((x) => { u = null; }); } }
         function main() { f(\"a\"); }",
    );
    assert!(
        r.contains("it is narrowed where the closure is created"),
        "{r}"
    );
}

#[test]
fn nullable_fields_narrow() {
    ok_src(
        "class N { left: N | null = null; v: i64 = 1;
           sum(): i64 { if (this.left === null) { return this.v; } return this.v + this.left.sum(); } }
         function f(n: N): i64 { n.left = new N(); return n.left.v; }
         function main() { const n = new N(); console.log(n.sum(), f(n)); }",
    );
    let r = err_src(
        "class N { left: N | null = null; v: i64 = 1; }
         function f(n: N): i64 { if (n.left != null) { n.left = null; return n.left.v; } return 0; }
         function main() { console.log(f(new N())); }",
    );
    assert!(r.contains("may be null"), "{r}");
}
