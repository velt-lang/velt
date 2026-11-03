//! The storage floats `f16` and `bf16` (#2049): what converts implicitly (widening to `f32`,
//! `f64`, `number`) and what needs `as` (narrowing, between the two), the literal range, and the
//! operators that reject them.

mod common;

use common::programs::{err_src, ok_src};

#[test]
fn storage_floats_widen_implicitly_and_compute_in_f32() {
    ok_src(
        "function main() {
           const w: f16 = 0.1;
           const b: bf16 = -2;
           const x: f32 = w;
           const y: number = b;
           const p: f32 = w * w;
           const q: number = w * 2.5;
           const c: boolean = w < b;
           let s: f32 = 0;
           s += w;
           const n: f16 = -w as f16;
           console.log(x, y, p, q, c, s, n, w as bf16, b as f16, 300 as f16, w as i32);
         }",
    );
}

#[test]
fn narrowing_needs_as() {
    let r = err_src("function main() { const x: f32 = 1.5; const h: f16 = x; }");
    assert!(
        r.contains("`f32` doesn't convert to `f16` implicitly: write `as f16`"),
        "{r}"
    );
    let r = err_src("function main() { const d = 2.5; const h: bf16 = d; }");
    assert!(
        r.contains("`f64` doesn't convert to `bf16` implicitly: write `as bf16`"),
        "{r}"
    );
    let r = err_src("function main() { const w: f16 = 1; const b: bf16 = w; }");
    assert!(
        r.contains("`f16` and `bf16` don't convert to each other implicitly"),
        "{r}"
    );
    // The result of arithmetic is an `f32`.
    let r = err_src("function main() { const w: f16 = 1; const v: f16 = w + w; }");
    assert!(r.contains("write `as f16`"), "{r}");
}

#[test]
fn a_literal_that_rounds_to_infinity_is_an_error() {
    let r = err_src("function main() { const h: f16 = 70000; }");
    assert!(r.contains("literal out of range for `f16`"), "{r}");
    assert!(r.contains("the largest finite `f16` is `65504`"), "{r}");
    let r = err_src("function main() { const h: bf16 = -1e39; }");
    assert!(r.contains("literal out of range for `bf16`"), "{r}");
    // 65519 rounds to 65504; 65520 is past the last finite value's midpoint.
    ok_src("function main() { const h: f16 = 65519; const g: f16 = -65519.9; console.log(h, g); }");
    let r = err_src("function main() { const h: f16 = 65520; }");
    assert!(r.contains("literal out of range for `f16`"), "{r}");
}

#[test]
fn bitwise_operators_reject_storage_floats() {
    let r = err_src("function main() { const w: f16 = 1; const k = w & 1; }");
    assert!(
        r.contains("bitwise operators take integers, not `f16`"),
        "{r}"
    );
    let r = err_src("function main() { const w: bf16 = 1; const k = 1 << w; }");
    assert!(
        r.contains("bitwise operators take integers, not `bf16`"),
        "{r}"
    );
}

#[test]
fn compound_assignment_takes_the_type_or_a_literal() {
    ok_src(
        "function main() {
           let a: f16 = 1;
           const w: f16 = 0.5;
           a += w;
           a *= 2;
           a -= 0.1;
           a++;
           --a;
           let s: f32 = 0;
           s += w;
           console.log(a, s);
         }",
    );
    let r = err_src("function main() { let a: f16 = 1; const x: f32 = 2; a += x; }");
    assert!(
        r.contains("an `f32` doesn't convert to `f16` implicitly"),
        "{r}"
    );
    assert!(r.contains("write `x = (x op v) as f16`"), "{r}");
    let r = err_src("function main() { let a: bf16 = 1; const w: f16 = 1; a *= w; }");
    assert!(
        r.contains("an `f16` doesn't convert to `bf16` implicitly"),
        "{r}"
    );
    let r = err_src("function main() { let a: f16 = 1; a |= 1; }");
    assert!(
        r.contains("bitwise operators take integers, not `f16`"),
        "{r}"
    );
}

#[test]
fn an_any_converts_only_with_as() {
    ok_src("function main() { const a: any = 1.5; const w = a as f16; console.log(w); }");
    let r = err_src("function main() { const a: any = 1.5; const w: f16 = a; }");
    assert!(
        r.contains("an `any` doesn't convert to `f16` implicitly: write `x as f16`"),
        "{r}"
    );
}
