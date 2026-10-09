//! Object and array spread.

mod common;

use common::hir_walk::{exprs, func};
use common::programs::{err_src, ok_src};
use velt_sema::hir::{ExprKind as E, Intrinsic, UseMode};

#[test]
fn object_spread_of_a_local_moves_its_fields() {
    let p = ok_src(
        "struct S { a: string; b: i64; }
         function main() { const s = S { a: \"x\", b: 1 }; const t = { ...s, b: 2 }; console.log(t.a, t.b); }",
    );
    let moved = exprs(func(&p, "main"))
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                E::Field {
                    mode: UseMode::Move,
                    ..
                }
            )
        })
        .count();
    assert_eq!(moved, 1, "`a` is moved out of `s`; `b` is overridden");
    // A string field is copied when the source is still used (strings are values)...
    ok_src(
        "struct S { a: string; b: i64; }
         function main() { const s = S { a: \"x\", b: 1 }; const t = { ...s }; console.log(s.a, t.b); }",
    );
    // ... and so is an array field (semantics stage 2: shared).
    ok_src(
        "struct S { a: i64[]; b: i64; }
         function main() { const s = S { a: [1], b: 1 }; const t = { ...s }; console.log(s.a, t.b); }",
    );
}

#[test]
fn object_spread_of_a_class_clones() {
    let p = ok_src(
        "class C { n: string = \"x\"; }
         function main() { const c = new C(); const o = { ...c }; console.log(o.n, c.n); }",
    );
    let clones = exprs(func(&p, "main"))
        .iter()
        .filter(|e| {
            matches!(
                &e.kind,
                E::Call {
                    callee: velt_sema::hir::Callee::Intrinsic(Intrinsic::Share),
                    ..
                }
            )
        })
        .count();
    assert_eq!(clones, 1);
}

#[test]
fn object_spread_typed_and_errors() {
    ok_src(
        "struct P { x: i64; y: i64; } struct Q { x: i64; y: i64; z: i64; }
         function main() { const q = Q { x: 1, y: 2, z: 3 }; const p: P = { ...q, y: 0 };
           const r = P { ...p, x: 9 }; console.log(p.x, r.x); }",
    );
    let r = err_src("function main() { const xs = [1]; const o = { ...xs }; }");
    assert!(
        r.contains("cannot spread a value of type `f64[]` into an object"),
        "{r}"
    );
    let r =
        err_src("struct P { x: i64; } function main() { const p: P = { ...P { x: 1 }, z: 1 }; }");
    assert!(r.contains("no field `z` on type `P`"), "{r}");
    let r = err_src("function main() { const o = { a: 1, a: 2 }; }");
    assert!(r.contains("duplicate field `a`"), "{r}");
}

#[test]
fn array_spread_builds_with_capacity_and_pushes() {
    let p = ok_src(
        "function main() { const xs = [\"a\"]; const ys = [...xs, \"b\"]; console.log(ys, xs); }",
    );
    let intrinsics: Vec<Intrinsic> = exprs(func(&p, "main"))
        .iter()
        .filter_map(|e| match &e.kind {
            E::Call {
                callee: velt_sema::hir::Callee::Intrinsic(i),
                ..
            } => Some(*i),
            _ => None,
        })
        .collect();
    assert!(intrinsics.contains(&Intrinsic::ArrayWithCapacity));
    assert!(
        intrinsics.contains(&Intrinsic::Share),
        "string elements are cloned"
    );
    assert_eq!(
        intrinsics
            .iter()
            .filter(|i| **i == Intrinsic::ArrayPush)
            .count(),
        2
    );
    let r = err_src("function main() { const n = 1; const ys = [...n]; }");
    assert!(
        r.contains("cannot spread a value of type `f64` into an array"),
        "{r}"
    );
    let r = err_src("function f(a: i64) {} function main() { const xs = [1]; f(...xs); }");
    assert!(
        r.contains("a spread argument must have a tuple type"),
        "{r}"
    );
}
