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
         function f(u: string | null): number[] {
           if (u != null) { return [1].map((x) => u.length); } return []; }
         function main() { console.log(keep(true, [true]), f(\"a\")); }",
    );
    // A closure that assigns the variable: the check does not narrow it (#435).
    ok_src(
        "function f(u: string | null) {
           if (u != null) { [1].forEach((x) => { u = null; }); } }
         function main() { f(\"a\"); }",
    );
}

#[test]
fn variables_closures_assign_are_not_narrowed() {
    // A null check narrows until the next assignment in the same function (#1199); the read
    // checks the value again, as `clear()` may have run the closure.
    ok_src(
        "function f(u: string | null): number {
           const clear = () => { u = null; };
           if (u != null) { clear(); return u.length; } return 0; }
         function main() { console.log(f(\"a\")); }",
    );
    // Not in a closure that captures it: the closure may run after the other one did.
    let r = err_src(
        "function f(u: string | null): usize {
           const clear = () => { u = null; };
           if (u != null) { const len = (): usize => u.length; clear(); return len(); }
           return 0; }
         function main() { console.log(f(\"a\")); }",
    );
    assert!(
        r.contains("`u` is assigned in a closure, so it is not narrowed to `string` here"),
        "{r}"
    );
    assert!(
        r.contains("`const current = u; if (current !== null)"),
        "{r}"
    );
    // Inside a closure too, for a variable another closure assigns.
    let r = err_src(
        "class A {} class B extends A { x: i64 = 1; }
         function main() {
           let a: A = new B();
           const reset = () => { a = new A(); };
           const read = (): i64 => { if (a instanceof B) { reset(); return a.x; } return 0; };
           console.log(read()); }",
    );
    assert!(r.contains("no field `x` on type `A`"), "{r}");
    assert!(r.contains("`const b = a; if (b instanceof B)"), "{r}");
    // A `const` copy, a closure's own variable, and one only read by closures still narrow.
    ok_src(
        "class A {} class B extends A { x: i64 = 1; }
         function main() {
           let a: A = new B();
           const reset = () => { a = new A(); };
           const b = a;
           if (b instanceof B) { reset(); console.log(b.x); }
           const own = (): i64 => { let c: A = new B(); if (c instanceof B) { c.x = 2; return c.x; } return 0; };
           let d: A = new B();
           const read = (): bool => d instanceof B;
           if (d instanceof B) { console.log(read(), d.x, own()); } }",
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

#[test]
fn instanceof_downcasts_test_the_class_and_read_as_the_subclass() {
    use common::hir_walk::{exprs, func, pats};
    use velt_sema::hir::{ExprKind, PatKind};
    let p = ok_src(
        "class A {} class B extends A { x: i64 = 1; }
         function f(a: A): i64 { if (a instanceof B) { return a.x; } return 0; }
         function main() { console.log(f(new B())); }",
    );
    let f = func(&p, "f");
    assert!(pats(f)
        .iter()
        .any(|p| matches!(p.kind, PatKind::InstanceOf(_))));
    assert!(exprs(f)
        .iter()
        .any(|e| matches!(e.kind, ExprKind::Downcast(_))));
    // Reassigned, the local is the base class again.
    let r = err_src(
        "class A {} class B extends A { x: i64 = 1; }
         function f(a: A): i64 { if (a instanceof B) { a = new A(); return a.x; } return 0; }
         function main() { console.log(f(new B())); }",
    );
    assert!(r.contains("no field `x` on type `A`"), "{r}");
}

#[test]
fn nullable_unions_narrow_to_some_members() {
    ok_src(
        "interface V { t: string }
         function f(x: V | string | number | null): V | string | null {
           if (typeof x === \"number\") { return null; } return x; }
         function g(x: string | null | undefined): string | null { const y = x ?? null; return y; }
         function main() { console.log(f(1), g(undefined)); }",
    );
    // What the test leaves must fit: `string` is still possible here.
    let r = err_src(
        "interface V { t: string }
         function f(x: V | string | number | null): V | null {
           if (typeof x === \"number\") { return null; } return x; }
         function main() { console.log(f(1)); }",
    );
    assert!(r.contains("mismatched types"), "{r}");
}

#[test]
fn union_fields_narrow_to_some_members_and_call_their_function() {
    ok_src(
        "interface P { f?: string | number | (() => string) }
         function f(p: P): string {
           if (typeof p.f === \"function\") { return p.f(); }
           const r: string | number | undefined = p.f;
           return `${r}`;
         }
         function main() { console.log(f({ f: () => \"a\" })); }",
    );
    let r = err_src(
        "interface P { f: string | (() => string) }
         function f(p: P): string { return p.f(); }
         function main() { console.log(f({ f: \"a\" })); }",
    );
    assert!(r.contains("cannot call `f` of type"), "{r}");
}

#[test]
fn any_converts_to_a_union_with_one_interface_member() {
    ok_src(
        "interface V { t: string }
         function f(a: any): V | string | number { return a; }
         function main() { console.log(f(\"s\")); }",
    );
    // Two interfaces cannot be told apart at run time.
    let r = err_src(
        "interface V { t: string } interface W { w: number }
         function f(a: any): V | W | string { return a; }
         function main() { console.log(f(\"s\")); }",
    );
    assert!(r.contains("no run-time test for"), "{r}");
}

#[test]
fn a_field_narrowed_to_a_function_and_another_member_reads_without_an_expected_type() {
    // The union `f64 | () => f64` is complete once the function type's errors are solved.
    ok_src(
        "interface I { v: string | number | (() => number) }
         function g(b: I): string {
           if (typeof b.v === \"string\") { return \"str\"; }
           const r = b.v;
           return `${typeof r}`;
         }
         function main() { console.log(g({ v: 3 })); }",
    );
}
