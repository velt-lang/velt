//! Closure captures: stored closures keep their own copies, so a later assignment to a
//! captured variable is an error; closures handed to storing intrinsics (`push`) escape.

mod common;

use common::programs::{err_src, ok_src};

#[test]
fn assigning_after_an_escaping_capture_is_an_error() {
    let r = err_src(
        "function main() { let k = 1; const f = (x: i64): i64 => x + k; k = 5;
           console.log(f(1), k); }",
    );
    assert!(
        r.contains("cannot assign to `k` after a stored closure captured it"),
        "{r}"
    );
    let r = err_src(
        "function main() { let n = 0; const h = () => n;
           [1, 2].forEach((x) => { n += x; }); console.log(h()); }",
    );
    assert!(r.contains("cannot assign to `n`"), "{r}");
}

#[test]
fn for_loop_step_and_non_escaping_captures_are_fine() {
    ok_src(
        "function main() { const gs: (() => i64)[] = [];
           for (let i = 0; i < 3; i++) { gs.push(() => i); }
           let m = 10; const ys = [1].map((x) => x + m); m = 20; console.log(ys, m, gs.length); }",
    );
}

#[test]
fn closure_pushed_into_an_array_escapes() {
    let r = err_src(
        "function main() { let w = 0; const fs: (() => i64)[] = [];
           fs.push(() => w); w++; console.log(fs[0]()); }",
    );
    assert!(r.contains("cannot assign to `w`"), "{r}");
}

#[test]
fn a_closure_that_went_out_of_scope_does_not_pin_its_captures() {
    ok_src(
        "function main() { let w = 0; let t = 0;
           while (w < 3) { w++; const f = (x: i64): i64 => x + w; t += f(1); } console.log(t); }",
    );
}
