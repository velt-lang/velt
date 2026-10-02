//! Moves dataflow: soft (string) moves that become copies when the place is used again.

mod common;

use common::hir_walk::{calls, func};
use common::programs::ok_src;
use velt_sema::hir::{Callee, Intrinsic};

#[test]
fn string_moves_after_an_earlier_soft_move_become_copies() {
    ok_src(
        "function main() { const s = `a${1}`; const c = true;
           console.log((c ? s : \"b\").length, s);
           const none: string | null = null; console.log(`${none ?? s}`, s); }",
    );
}

/// Number of `Intrinsic::Share` calls in function `name`.
fn shares(src: &str, name: &str) -> usize {
    let p = ok_src(src);
    calls(func(&p, name))
        .into_iter()
        .filter(|(c, _)| matches!(c, Callee::Intrinsic(Intrinsic::Share)))
        .count()
}

#[test]
fn a_soft_move_in_a_loop_after_one_before_it_is_shared() {
    // Both moves of `a` reach the loop's next iteration: each one is used again (#131).
    let src = "async function one(xs: i64[]): Promise<usize> { return xs.length; }
         async function main() {
           const a = [1]; const k = a;
           for (let w = 0; w < 2; w++) { console.log(await one(a)); }
           console.log(k); }";
    assert_eq!(shares(src, "main"), 2);
}

#[test]
fn soft_moves_in_both_branches_are_shared_when_used_again() {
    let src = "async function one(xs: i64[]): Promise<usize> { return xs.length; }
         async function main() {
           const a = [1]; const k = a; const c = k.length == 1;
           if (c) { console.log(await one(a)); } else { console.log(await one(a)); }
           console.log(a); }";
    assert_eq!(shares(src, "main"), 3);
}

#[test]
fn a_finally_after_a_return_keeps_what_it_uses() {
    // The `finally` runs after `return keep(t)`: `t` is shared with `keep`, not moved (#144).
    let src = "class T { n: i64 = 1; }
         function keep(t: T): i64 { const kept = [t]; return kept[0].n; }
         function f(): i64 { const t = new T(); try { return keep(t); } finally { console.log(t.n); } }
         function g(): i64 { const t = new T(); try { return keep(t); } finally { console.log(1); } }
         function main() { console.log(f(), g()); }";
    assert_eq!(shares(src, "f"), 1);
    assert_eq!(shares(src, "g"), 0);
}

#[test]
fn a_finally_after_a_break_keeps_what_it_uses() {
    let src = "class T { n: i64 = 1; }
         function keep(t: T): i64 { const kept = [t]; return kept[0].n; }
         function f(): i64 {
           const t = new T(); let out = 0;
           for (let i = 0; i < 3; i++) { try { out = keep(t); break; } finally { console.log(t.n); } }
           return out; }
         function main() { console.log(f()); }";
    assert_eq!(shares(src, "f"), 1);
}
