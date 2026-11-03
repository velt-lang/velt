//! The storage floats `f16` and `bf16` (#2049): a `number` with 16-bit storage, as a
//! `Float16Array` element is in JavaScript. Reading one widens it to `number` (and to `f32`/`f64`
//! implicitly), so every operator computes in f64 and gives a `number`; storing into one rounds
//! once (`=` and compound assignment), and only `as` narrows a value into one.

mod common;

use common::programs::{err_src, ok_src};

#[test]
fn storage_floats_widen_implicitly_and_compute_as_numbers() {
    ok_src(
        "function main() {
           const w: f16 = 0.1;
           const b: bf16 = -2;
           const x: f32 = w;
           const y: number = b;
           const p: number = w * w;
           const q: number = w * 2.5;
           const c: boolean = w < b;
           const k: number = w | 0;
           const plus: number = +w;
           let s: f32 = 0;
           s += w;
           const n: f16 = -w as f16;
           console.log(x, y, p, q, c, k, plus, s, n, w as bf16, b as f16, 300 as f16, w as i32);
         }",
    );
    // Arithmetic gives a `number`, which needs `as` into an `f32` as into an `f16`.
    let r = err_src("function main() { const w: f16 = 1; const v: f32 = w * w; }");
    assert!(r.contains("expected f32, found f64"), "{r}");
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
    // The result of arithmetic is a `number`.
    let r = err_src("function main() { const w: f16 = 1; const v: f16 = w + w; }");
    assert!(
        r.contains("`f64` doesn't convert to `f16` implicitly: write `as f16`"),
        "{r}"
    );
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
    // An assignment used as a value checks its literal as the statement does, and a right side
    // that is not a plain literal needs `as` there too.
    let r = err_src("function main() { let x: f16 = 0; const r = (x = 70000); }");
    assert!(r.contains("literal out of range for `f16`"), "{r}");
    let r = err_src("function main() { let x: f16 = 0; const r = (x = 0.1 + 0.2); }");
    assert!(r.contains("expected f16, found f64"), "{r}");
}

#[test]
fn compound_assignment_takes_a_number() {
    ok_src(
        "function main() {
           let a: f16 = 1;
           const w: f16 = 0.5;
           const x: f32 = 2;
           const n = 2.5;
           const i: i32 = 3;
           const b: bf16 = 1;
           a += w;
           a *= x;
           a -= n;
           a /= i;
           a += b;
           a |= 4;
           a++;
           --a;
           let s: f32 = 0;
           s += w;
           console.log(a, s);
         }",
    );
    // A value that doesn't convert to `number` exactly needs `as number`, as for `n += v`.
    let r = err_src("function main() { let a: f16 = 1; const big: i64 = 2; a += big; }");
    assert!(r.contains("found i64"), "{r}");
}

#[test]
fn an_any_converts_only_with_as() {
    ok_src("function main() { const a: any = 1.5; const w = a as f16; console.log(w); }");
    let r = err_src("function main() { let w: f16 = 1; const a: any = 2; w -= a; }");
    assert!(
        r.contains("an `any` doesn't convert to `f16` implicitly"),
        "{r}"
    );
    let r = err_src("function main() { const a: any = 1.5; const w: f16 = a; }");
    assert!(
        r.contains("an `any` doesn't convert to `f16` implicitly: write `x as f16`"),
        "{r}"
    );
}
