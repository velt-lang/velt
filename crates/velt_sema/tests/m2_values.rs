//! M2 values and control: arrays, `T | null` (narrowing, `??`, `?.`), closures, errors
//! (`throw`/`try`/`Result`/`?`), printing.

mod common;

use common::hir_walk::{calls, exprs, func};
use common::programs::{err_src, ok_src};
use velt_sema::hir::{
    Callee, Def, ExprKind as E, Intrinsic, PassMode, PatKind, StmtKind, TyKind, UseMode,
};

// ───────────────────────────── arrays ─────────────────────────────

#[test]
fn array_literals_index_and_intrinsics() {
    let p = ok_src(
        "function main() { let xs = [1, 2]; xs.push(3); xs[0] = 5; const n = xs.pop(); const i: i64 = 1; console.log(xs[i], xs.length, n ?? 0); }",
    );
    let main = func(&p, "main");
    assert!(exprs(main)
        .iter()
        .any(|e| matches!(&e.kind, E::Index { index, .. } if matches!(index.kind, E::Cast(_)))));
    let r = err_src("function main() { const xs = []; }");
    assert!(r.contains("cannot infer the element type of `[]`"), "{r}");
    let r = err_src("function main() { const xs = [1, \"a\"]; }");
    assert!(r.contains("mismatched types"), "{r}");
    let r = err_src("function main() { const xs = [1]; xs[\"a\"]; }");
    assert!(r.contains("array index must be an integer"), "{r}");
}

#[test]
fn for_of_borrows_or_copies_elements() {
    let p = ok_src(
        "function main() { const xs = [\"a\"]; for (const s of xs) { console.log(s); } for (const n of [1, 2]) { console.log(n); } }",
    );
    let main = func(&p, "main");
    let binds: Vec<UseMode> = main
        .body
        .block
        .stmts
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::ForOf { binding, .. } => match binding.kind {
                PatKind::Binding(_, m) => Some(m),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(binds, vec![UseMode::Borrow, UseMode::Copy]);
    let r = err_src("function main() { const xs = [\"a\"]; for (const s of xs) { s = \"b\"; } }");
    assert!(r.contains("cannot assign to `s`"), "{r}");
    let r = err_src("function main() { for (const s of 5) { } }");
    assert!(
        r.contains("cannot iterate over a value of type `i64`"),
        "{r}"
    );
}

#[test]
fn for_of_over_a_map_iterates_entries() {
    let p = ok_src(
        "function main() { const m = new Map<string, i64>(); for (const [k, v] of m) { console.log(k, v); } }",
    );
    let entries = p
        .defs
        .iter()
        .position(|d| matches!(d, Def::Fn(f) if f.name.ends_with("Map.entries")))
        .unwrap();
    assert!(calls(func(&p, "main"))
        .iter()
        .any(|(c, _)| matches!(c, Callee::Def(d, _) if d.0 as usize == entries)));
}

#[test]
fn array_methods_from_the_prelude() {
    ok_src(
        "function main() { const xs = [3, 1, 2]; let ys = xs.map((x) => x * 2).filter((x) => x > 2); ys.sort(); console.log(ys.indexOf(4), xs.includes(9), [\"a\", \"b\"].join(\"-\"), xs.slice(1).length, xs.reduce((a, x) => a + x, 0)); }",
    );
    let r = err_src("function main() { const xs = [[1]]; xs.sort(); }");
    assert!(
        r.contains("method `sort` takes 1 argument but 0 arguments were supplied"),
        "{r}"
    );
}

// ───────────────────────────── T | null ─────────────────────────────

#[test]
fn null_and_wrap_some() {
    let p = ok_src("function f(b: bool): string | null { if (b) return \"x\"; return null; } function main() { const x: i64 | null = 5; console.log(f(true), x); }");
    assert!(exprs(func(&p, "f"))
        .iter()
        .any(|e| matches!(e.kind, E::WrapSome(_))));
    assert!(exprs(func(&p, "f"))
        .iter()
        .any(|e| matches!(e.kind, E::Lit(velt_sema::hir::Lit::Null))));
    let r = err_src("function main() { let x = null; }");
    assert!(r.contains("cannot infer the type of `null`"), "{r}");
    let r = err_src("function main() { const x: i64 = null; }");
    assert!(
        r.contains("mismatched types") && r.contains("found null"),
        "{r}"
    );
}

#[test]
fn narrowing_by_null_tests() {
    let p = ok_src(
        "function f(x: i64 | null): i64 { if (x == null) return 0; return x + 1; }
         function g(x: i64 | null): i64 { if (x != null && x > 2) { return x; } return x != null ? x : 0; }
         function h(s: string | null): usize { while (s != null) { return s.length; } return 0; }
         function main() { console.log(f(1), g(null), h(\"a\")); }",
    );
    for name in ["f", "g", "h"] {
        assert!(
            exprs(func(&p, name))
                .iter()
                .any(|e| matches!(e.kind, E::UnwrapSome(..))),
            "{name}"
        );
    }
    let r = err_src("function f(x: i64 | null): i64 { return x + 1; } function main() {}");
    assert!(r.contains("mismatched types"), "{r}");
    let r = err_src("function f(x: string | null): usize { return x.length; } function main() {}");
    assert!(r.contains("may be null"), "{r}");
    let r = err_src("function f(x: i64 | null): i64 { if (x != null) { x = null; return x; } return 0; } function main() {}");
    assert!(
        r.contains("mismatched types"),
        "narrowing ends at reassignment: {r}"
    );
}

#[test]
fn nullish_and_optional_chaining() {
    let p = ok_src(
        "class N { v: i64 = 1; next: N | null = null; get(): i64 { return this.v; } }
         function main() { const n: N | null = new N(); console.log(n?.v ?? 0, n?.get(), n?.next?.v ?? -1); const xs: i64[] | null = [1]; console.log(xs?.[0]); }",
    );
    let main = func(&p, "main");
    let matches = exprs(main)
        .iter()
        .filter(|e| matches!(e.kind, E::Match { .. }))
        .count();
    assert!(matches >= 6, "{matches}");
    let r = err_src("function main() { const x = 5; console.log(x ?? 1); }");
    assert!(
        r.contains("`??` needs a `T | null` value on the left"),
        "{r}"
    );
    let r = err_src("function main() { const x = 5; console.log(x == null); }");
    assert!(r.contains("is never null"), "{r}");
}

#[test]
fn result_is_removed() {
    let r = err_src("function b(): Result<i64, string> { return 1; } function main() {}");
    assert!(r.contains("`Result` was removed"), "{r}");
    let r = err_src("function main() { const r = Ok(1); }");
    assert!(r.contains("`Ok` was removed with `Result`"), "{r}");
}

// ───────────────────────────── closures ─────────────────────────────

#[test]
fn closures_infer_params_and_returns() {
    let p = ok_src(
        "function apply(f: (x: i64) => i64, v: i64): i64 { return f(v); }
         function main() { const inc = (x: i64) => x + 1; console.log(apply(inc, 1), apply((x) => { return x * 2; }, 2), ((y: i64) => y)(3)); }",
    );
    let closures: Vec<_> = p
        .defs
        .iter()
        .filter_map(|d| match d {
            Def::Fn(f) if f.name.starts_with("main::{closure#") => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(closures.len(), 3);
    let r = err_src("function main() { const f = (x) => x; }");
    assert!(
        r.contains("type annotations needed for parameter `x`"),
        "{r}"
    );
    let r = err_src("function apply(f: (x: i64) => i64): i64 { return f(1); } function main() { apply((x) => \"s\"); }");
    assert!(r.contains("mismatched types"), "{r}");
}

#[test]
fn closure_capture_rules() {
    let r = err_src("function main() { const total = 0; [1].forEach((x) => { total += x; }); }");
    assert!(r.contains("cannot assign twice to const `total`"), "{r}");
    // A closure modifying a param makes the param modified (inferred `BorrowMut`).
    let p = ok_src(
        "function f(n: i64[]) { [1].forEach((x) => { n.push(x); }); } function main() { f([]); }",
    );
    assert_eq!(func(&p, "f").params[0].mode, PassMode::BorrowMut);
    let p = ok_src(
        "class C { n: i64 = 0; bump() { [1, 2].forEach((x) => { this.n += x; }); } } function main() { let c = new C(); c.bump(); }",
    );
    let bump = func(&p, "C.bump");
    let closure = p
        .defs
        .iter()
        .find_map(|d| match d {
            Def::Fn(f) if f.name == "C.bump::{closure#0}" => Some(f),
            _ => None,
        })
        .unwrap();
    assert_eq!(closure.captures[0].mode, PassMode::BorrowMut);
    assert_eq!(
        bump.body.locals[closure.captures[0].outer.0 as usize].name,
        "this"
    );
    assert_eq!(bump.params[0].mode, PassMode::BorrowMut);
}

#[test]
fn nested_closures_capture_through_the_middle() {
    let p = ok_src(
        "function main() { let t = 0; [1].forEach((x) => { [2].forEach((y) => { t += x + y; }); }); console.log(t); }",
    );
    let inner = p
        .defs
        .iter()
        .find_map(|d| match d {
            Def::Fn(f) if f.name == "main::{closure#1}" => Some(f),
            _ => None,
        })
        .unwrap();
    assert_eq!(inner.captures.len(), 2);
    let outer = p
        .defs
        .iter()
        .find_map(|d| match d {
            Def::Fn(f) if f.name == "main::{closure#0}" => Some(f),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        outer.captures.len(),
        1,
        "the middle closure captures `t` for the inner one"
    );
    assert_eq!(outer.captures[0].mode, PassMode::BorrowMut);
}

#[test]
fn named_functions_as_values() {
    let p = ok_src(
        "function dbl(x: i64): i64 { return x * 2; } function show<T>(x: T): string { return `${x}`; }
         function main() { const f = dbl; const g: (x: string) => string = show; console.log([1].map(dbl).length, f(2), g(\"a\")); }",
    );
    assert!(exprs(func(&p, "main"))
        .iter()
        .any(|e| matches!(e.kind, E::FnRef(..))));
}

// ───────────────────────────── errors ─────────────────────────────

#[test]
fn throws_are_inferred_and_propagate() {
    let p = ok_src(
        "class E { message: string = \"e\"; }
         function a(): i64 { throw new E(); }
         function b(): i64 { return a() + 1; }
         function c(): i64 { try { return b(); } catch (e) { console.log(e.message); return 0; } }
         function main() { console.log(c()); }",
    );
    assert!(func(&p, "a").throws.is_some());
    assert!(func(&p, "b").throws.is_some());
    assert!(func(&p, "c").throws.is_none(), "caught");
    assert!(func(&p, "main").throws.is_none());
}

#[test]
fn catch_type_is_the_union_of_thrown_types() {
    let p = ok_src(
        "class B { message: string = \"b\"; } class X extends B {} class Y extends B {}
         function f(n: i64): i64 { if (n > 0) throw new X(); throw new Y(); }
         function main() { try { f(1); } catch (e) { if (e instanceof X) { console.log(e.message); } } }",
    );
    let main = func(&p, "main");
    let caught = main
        .body
        .block
        .stmts
        .iter()
        .find_map(|s| match &s.kind {
            StmtKind::Try {
                catch: Some((Some(l), _)),
                ..
            } => Some(main.body.locals[l.0 as usize].ty),
            _ => None,
        })
        .expect("catch local");
    assert!(
        matches!(p.types.kind(caught), TyKind::Adt(d, _) if matches!(p.def(*d), Def::Enum(e) if e.is_union && e.variants.len() == 2)),
        "`X | Y`"
    );
    let p = ok_src(
        "function f(n: i64): i64 { if (n > 0) throw \"s\"; throw 5; }
         function main() { try { f(1); } catch (e) { if (typeof e === \"string\") { console.log(e); } else { console.log(e + 1); } } }",
    );
    assert!(func(&p, "main").throws.is_none());
}

#[test]
fn closures_and_function_values_carry_error_types() {
    let p = ok_src(
        "function f(): i64 { throw \"x\"; } function main() { [1].forEach((x) => { f(); }); }",
    );
    assert!(func(&p, "main").throws.is_some(), "forEach rethrows");
    let p = ok_src("function main() { const f = () => { throw \"x\"; }; try { f(); } catch (e) { console.log(e); } }");
    assert!(func(&p, "main").throws.is_none());
    let p =
        ok_src("function f(x: i64): i64 { throw \"x\"; } function main() { const g = f; g(1); }");
    assert!(func(&p, "main").throws.is_some());
    let r = err_src(
        "function run(f: () => void) { f(); } function main() { run(() => { throw \"x\"; }); }",
    );
    assert!(
        r.contains("but the function type it is used as does not allow throwing"),
        "{r}"
    );
}

#[test]
fn try_finally_without_catch_propagates() {
    let p = ok_src("function f(): i64 { throw \"x\"; } function g(): i64 { try { return f(); } finally { console.log(1); } } function main() { try { g(); } catch (e) { console.log(e); } }");
    assert!(func(&p, "g").throws.is_some());
}

#[test]
fn throws_clauses_bound_the_body() {
    let p = ok_src(
        "class A { message: string = \"a\"; } class B extends A {}
         function f(x: i64): i64 throws A { if (x < 0) throw new B(); return x; }
         function main() { try { f(1); } catch (e) { console.log(e.message); } }",
    );
    assert!(func(&p, "f").throws.is_some());
    let r = err_src(
        "class A { message: string = \"a\"; }
         function f(x: i64): i64 throws A { if (x < 0) throw \"neg\"; return x; } function main() {}",
    );
    assert!(
        r.contains("`f` throws `string`, which its `throws` clause does not allow"),
        "{r}"
    );
}

// ───────────────────────────── printing & templates ─────────────────────────────

#[test]
fn printing_and_formatting() {
    let p = ok_src(
        "struct P { x: i64; } enum C { R } function main() { const xs = [1]; const o: i64 | null = null; console.log(xs, P { x: 1 }, C.R, o, `${xs} ${P { x: 2 }}`); }",
    );
    assert!(calls(func(&p, "main"))
        .iter()
        .any(|(c, _)| matches!(c, Callee::Intrinsic(Intrinsic::ToString))));
    let r = err_src("function main() { const f = (x: i64) => x; console.log(f); }");
    assert!(
        r.contains("cannot print a value of type `(i64) => i64`"),
        "{r}"
    );
    let r = err_src("interface I { f(): i64; } struct S implements I { f(): i64 { return 1; } } function main() { const i: I = S {}; console.log(`${i}`); }");
    assert!(r.contains("cannot format a value of type `I`"), "{r}");
}

#[test]
fn shared_values() {
    let p = ok_src("function main() { const s = shared(\"x\"); }");
    assert!(calls(func(&p, "main"))
        .iter()
        .any(|(c, _)| matches!(c, Callee::Intrinsic(Intrinsic::SharedNew))));
}

#[test]
fn array_constructors() {
    common::programs::ok_src(
        "function main() { const z: f64[] = new Array<f64>(4).fill(0.0);
           const sq: i64[] = Array.from({ length: 5 }, (_, i) => i * i); console.log(z, sq); }",
    );
    let r = common::programs::err_src("function main() { const a = new Array<i64>(3); }");
    assert!(
        r.contains("`new Array(n)` would hold `n` empty slots"),
        "{r}"
    );
}
