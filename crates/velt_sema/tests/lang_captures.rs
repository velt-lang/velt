//! Closure captures: stored closures keep their own copies unless the variable is assigned while
//! another party still sees it; then it lives in a shared cell (`LocalDef::boxed`, semantics
//! stage 2). Closures handed to storing intrinsics (`push`) escape.

mod common;

use common::hir_walk::func;
use common::programs::{err_src, ok_src};

/// Is local `name` of function `f` of program `p` a shared cell?
fn boxed(p: &velt_sema::hir::Program, f: &str, name: &str) -> bool {
    func(p, f)
        .body
        .locals
        .iter()
        .any(|l| l.name == name && l.boxed)
}

#[test]
fn assigning_after_an_escaping_capture_makes_a_cell() {
    let p = ok_src(
        "function main() { let k = 1; const f = (x: i64): i64 => x + k; k = 5;
           console.log(f(1), k); }",
    );
    assert!(boxed(&p, "main", "k"));
    let p = ok_src(
        "function main() { let n = 0; const h = () => n;
           [1, 2].forEach((x) => { n += x; }); console.log(h()); }",
    );
    assert!(boxed(&p, "main", "n"));
    let p = ok_src(
        "function main() { let c = 0; const inc = () => { c += 1; }; inc(); console.log(c); }",
    );
    assert!(boxed(&p, "main", "c"));
    // The closure is the only user afterwards: it keeps its own copy, no cell.
    let p = ok_src(
        "function make(): () => i64 { let n = 0; return () => { n += 1; return n; }; }
         function main() { console.log(make()()); }",
    );
    assert!(!boxed(&p, "make", "n"));
}

#[test]
fn async_closures_keep_their_own_copies() {
    let r = err_src(
        "async function main() { let k = 1; const f = async (): Promise<i64> => k; k = 5;
           console.log(await f(), k); }",
    );
    assert!(
        r.contains("cannot assign to `k` after a stored closure captured it"),
        "{r}"
    );
}

#[test]
fn for_loop_step_and_non_escaping_captures_are_fine() {
    let p = ok_src(
        "function main() { const gs: (() => i64)[] = [];
           for (let i = 0; i < 3; i++) { gs.push(() => i); }
           let m = 10; const ys = [1].map((x) => x + m); m = 20; console.log(ys, m, gs.length); }",
    );
    assert!(!boxed(&p, "main", "i"));
    assert!(!boxed(&p, "main", "m"));
}

#[test]
fn closure_pushed_into_an_array_escapes() {
    let p = ok_src(
        "function main() { let w = 0; const fs: (() => i64)[] = [];
           fs.push(() => w); w++; console.log(fs[0]()); }",
    );
    assert!(boxed(&p, "main", "w"));
}

#[test]
fn a_closure_that_went_out_of_scope_does_not_pin_its_captures() {
    let p = ok_src(
        "function main() { let w = 0; let t = 0;
           while (w < 3) { w++; const f = (x: i64): i64 => x + w; t += f(1); } console.log(t); }",
    );
    assert!(!boxed(&p, "main", "w"));
}
