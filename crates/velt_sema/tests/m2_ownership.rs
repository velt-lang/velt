//! Ownership in M2: parameter / receiver / binding ownership inference, moves out of borrowed
//! places, partial moves, closures moving their captures. Semantics stage 2: a non-Copy value
//! used again (or taken out of a borrowed place) is shared (`Intrinsic::Share`) instead of being
//! a "use of moved value"; only promises keep move errors.

mod common;

use common::hir_walk::{calls, exprs, func, uses_of};
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Callee, ExprKind as E, PassMode, UseMode};

#[test]
fn param_that_is_moved_from_becomes_owned_and_callers_move() {
    let p = ok_src(
        "function keep(s: string): string { return s; }
         function show(s: string) { console.log(s); }
         function main() { const a = `x${1}`; show(a); const b = keep(a); console.log(b); }",
    );
    assert_eq!(func(&p, "keep").params[0].mode, PassMode::Owned);
    assert_eq!(func(&p, "show").params[0].mode, PassMode::Borrow);
    assert_eq!(
        uses_of(func(&p, "main"), "a"),
        vec![UseMode::Borrow, UseMode::Move]
    );
}

#[test]
fn ownership_propagates_through_calls() {
    let p = ok_src(
        "function sink(s: string): string { return s; }
         function relay(s: string): string { return sink(s); }
         function main() { console.log(relay(\"x\")); }",
    );
    assert_eq!(func(&p, "relay").params[0].mode, PassMode::Owned);
}

/// Number of `Intrinsic::Share` calls in `f`.
fn shares(f: &velt_sema::hir::FnDef) -> usize {
    calls(f)
        .into_iter()
        .filter(|(c, _)| matches!(c, Callee::Intrinsic(velt_sema::hir::Intrinsic::Share)))
        .count()
}

#[test]
fn use_after_passing_to_owned_param_shares() {
    let p = ok_src(
        "function take(s: i64[]): i64[] { return s; }
         function main() { const a = [1]; take(a); console.log(a); }",
    );
    assert_eq!(shares(func(&p, "main")), 1);
    let p = ok_src(
        "function take(s: string): string { return s; }
         function main() { const a = `x${1}`; take(a); console.log(a); }",
    );
    assert_eq!(shares(func(&p, "main")), 1);
    // Promises have one owner.
    let r = err_src(
        "async function f(): Promise<i64> { return 1; }
         function keep(p: Promise<i64>): Promise<i64> { return p; }
         async function main() { const a = f(); keep(a); console.log(await a); }",
    );
    assert!(r.contains("use of moved value `a`"), "{r}");
}

#[test]
fn modified_and_moved_params_are_owned() {
    let p = ok_src(
        "function f(xs: i64[]): i64[] { xs.push(1); return xs; }
         function main() { const a: i64[] = []; f(a); console.log(a); }",
    );
    assert_eq!(func(&p, "f").params[0].mode, PassMode::Owned);
    assert_eq!(shares(func(&p, "main")), 1);
}

#[test]
fn array_elements_and_for_of_bindings_are_shared() {
    let p = ok_src("function main() { const xs = [[1]]; let s = xs[0]; console.log(s); }");
    assert_eq!(shares(func(&p, "main")), 1);
    // A `const` refers to the element in place (`body::const_borrow`).
    let p = ok_src("function main() { const xs = [[1]]; const s = xs[0]; console.log(s); }");
    assert_eq!(shares(func(&p, "main")), 0);
    let p = ok_src(
        "function main() { const xs = [[1]]; const out: i64[][] = []; for (const s of xs) { out.push(s); } }",
    );
    assert_eq!(shares(func(&p, "main")), 1);
    ok_src("function main() { const xs = [[1]]; const out: i64[][] = []; for (const s of xs) { out.push(s.clone()); } }");
    ok_src("function main() { const xs = [\"a\"]; const s = xs[0]; const out: string[] = []; for (const t of xs) { out.push(t); } console.log(s, xs, out); }");
}

#[test]
fn class_fields_are_shared() {
    let p = ok_src(
        "class U { items: i64[] = []; }
         function main() { const u = new U(); const out: i64[][] = []; out.push(u.items); }",
    );
    assert_eq!(shares(func(&p, "main")), 1);
    ok_src(
        "class U { items: i64[] = []; }
         function main() { const u = new U(); const n = u.items; console.log(n); }",
    );
    // Replacing the field while `n` refers to it: `n` keeps the old array (a share).
    let p = ok_src(
        "class U { items: i64[] = []; }
         function main() { const u = new U(); const n = u.items; u.items = [2]; console.log(n); }",
    );
    assert_eq!(shares(func(&p, "main")), 1);
    ok_src(
        "class U { name: string = \"n\"; getName(): string { return this.name; } }
         function main() { const u = new U(); const n = u.name; console.log(n, u.getName(), u.name); }",
    );
}

#[test]
fn struct_fields_move_separately() {
    ok_src(
        "struct P { a: i64[]; b: i64[]; }
         function main() { const p = P { a: [1], b: [2] }; const a = p.a; const b = p.b; console.log(a, b); }",
    );
    for src in [
        "struct P { a: i64[]; b: i64[]; }
         function main() { const p = P { a: [1], b: [2] }; const a = p.a; console.log(p.a, a); }",
        "struct P { a: i64[]; b: i64[]; }
         function main() { const p = P { a: [1], b: [2] }; const a = p.a; console.log(p, a); }",
    ] {
        assert_eq!(shares(func(&ok_src(src), "main")), 1);
    }
    ok_src(
        "struct P { a: string; b: string; }
         function main() { const p = P { a: \"x\", b: \"y\" }; const a = p.a; console.log(p, p.a, a); }",
    );
}

#[test]
fn copy_structs_copy_and_classes_move() {
    ok_src("struct P { x: f64; } function main() { const p = P { x: 1.0 }; const q = p; console.log(p.x, q.x); }");
    let p = ok_src("class C { x: f64 = 1.0; } function main() { const p = new C(); const q = p; console.log(p.x, q.x); }");
    assert_eq!(shares(func(&p, "main")), 1);
}

#[test]
fn receivers_that_are_moved_from_are_owned() {
    let p = ok_src(
        "extend<T> Array<T> { take(): T[] { return this; } }
         function main() { const xs = [1, 2]; const ys = xs.take(); console.log(ys.length); }",
    );
    let take = p
        .defs
        .iter()
        .find_map(|d| match d {
            velt_sema::hir::Def::Fn(f) if f.name.ends_with(".take") => Some(f),
            _ => None,
        })
        .unwrap();
    assert_eq!(take.params[0].mode, PassMode::Owned);
    assert_eq!(uses_of(func(&p, "main"), "xs"), vec![UseMode::Move]);
    let p = ok_src(
        "extend<T> Array<T> { take(): T[] { return this; } }
         function main() { const xs = [1, 2]; const ys = xs.take(); console.log(xs.length, ys.length); }",
    );
    assert_eq!(shares(func(&p, "main")), 1);
}

#[test]
fn prelude_unwrap_or_consumes_non_copy_receivers() {
    let p = ok_src(
        "function f(): string | null { return \"v\"; }
         function main() { const r = f(); const v = r.unwrapOr(\"d\"); console.log(v); }",
    );
    assert_eq!(uses_of(func(&p, "main"), "r"), vec![UseMode::Move]);
    ok_src(
        "function main() { const x: i64 | null = 5; console.log(x.unwrapOr(1), x.unwrapOr(2)); }",
    );
}

#[test]
fn narrowed_members_move_from_places_when_moved() {
    let p = ok_src(
        "type T = { kind: \"w\"; s: i64[] } | { kind: \"n\" };
         function take(t: T): i64[] { switch (t.kind) { case \"w\": return t.s; default: return []; } }
         function peek(t: T): usize { switch (t.kind) { case \"w\": return t.s.length; default: return 0; } }
         function main() { console.log(take({ kind: \"w\", s: [1] }), peek({ kind: \"n\" })); }",
    );
    assert_eq!(func(&p, "take").params[0].mode, PassMode::Owned);
    assert_eq!(func(&p, "peek").params[0].mode, PassMode::Borrow);
    // A string taken out of a member is a copy: the param stays borrowed.
    let p = ok_src(
        "type T = { kind: \"w\"; s: string } | { kind: \"n\" };
         function take(t: T): string { switch (t.kind) { case \"w\": return t.s; default: return \"\"; } }
         function main() { console.log(take({ kind: \"w\", s: \"a\" })); }",
    );
    assert_eq!(func(&p, "take").params[0].mode, PassMode::Borrow);
}

#[test]
fn escaping_closures_move_or_share_captures() {
    let p = ok_src(
        "function main() { const s = [1]; const f = () => s; console.log(s); console.log(f()); }",
    );
    let shared = p
        .defs
        .iter()
        .any(|d| matches!(d, velt_sema::hir::Def::Fn(f) if f.captures.iter().any(|c| c.share)));
    assert!(shared, "the closure shares `s`");
    // A string variable used after the closure is created is copied into it.
    ok_src(
        "function main() { const s = `a${1}`; const f = () => s; console.log(s); console.log(f()); }",
    );
    ok_src("function main() { const s = `a${1}`; const f = () => `${s}`; console.log(f()); }");
    // Non-escaping closures borrow.
    ok_src(
        "function main() { const s = `a${1}`; const xs = [1]; xs.forEach((x) => console.log(s, x)); console.log(s); }",
    );
}

#[test]
fn owned_params_cannot_be_function_values() {
    let r = err_src(
        "function keep(s: string): string { return s; }
         function main() { const f: (s: string) => string = keep; console.log(f(\"x\")); }",
    );
    assert!(r.contains("takes ownership of `s`"), "{r}");
}

#[test]
fn closures_share_their_params_and_captures() {
    ok_src(
        "function main() { const out: i64[][] = []; const xs = [[1]]; xs.forEach((x) => { out.push(x); }); }",
    );
    ok_src("function main() { const s = [1]; const f = () => { const t = s; return t; }; console.log(f()); }");
    // Strings are copied instead.
    ok_src("function main() { const out: string[] = []; const xs = [\"a\"]; xs.forEach((x) => { out.push(x); }); }");
    ok_src("function main() { const s = `a${1}`; const f = () => { const t = s; return t; }; console.log(f()); }");
}

#[test]
fn virtual_method_params_are_borrowed() {
    let p = ok_src(
        "class A { items: i64[][] = []; add(s: i64[]) { this.items.push(s); } }
         class B extends A { override add(s: i64[]) { this.items.push(s); } }
         function main() { let b = new B(); b.add([1]); }",
    );
    assert_eq!(func(&p, "A.add").params[1].mode, PassMode::Borrow);
    assert_eq!(shares(func(&p, "A.add")), 1);
}

#[test]
fn owned_arg_through_upcast_and_wrap() {
    let p = ok_src(
        "class A { n: i64 = 1; } class B extends A {}
         function keep(a: A): A { return a; }
         function opt(s: string | null): string | null { return s; }
         function main() { const b = new B(); keep(b); const s = `x${1}`; opt(s); }",
    );
    let main = func(&p, "main");
    let args: Vec<_> = calls(main)
        .into_iter()
        .filter(|(c, _)| matches!(c, Callee::Def(..)))
        .collect();
    assert!(
        matches!(&args[0].1[0].kind, E::Upcast(x) if matches!(x.kind, E::Local(_, UseMode::Move)))
    );
    assert!(
        matches!(&args[1].1[0].kind, E::WrapSome(x) if matches!(x.kind, E::Local(_, UseMode::Move)))
    );
}

#[test]
fn moves_in_loops_and_branches() {
    let p = ok_src(
        "function take(s: i64[]): i64[] { return s; }
         function main() { const s = [1]; for (let i = 0; i < 2; i++) { take(s); } }",
    );
    assert_eq!(shares(func(&p, "main")), 1);
    ok_src(
        "function take(s: i64[]): i64[] { return s; }
         function main() { let s = [1]; for (let i = 0; i < 2; i++) { take(s); s = [2]; } }",
    );
}

#[test]
fn try_catch_moves_are_joined() {
    let p = ok_src(
        "class E { m: string = \"e\"; }
         function take(s: i64[]): i64[] { return s; }
         function risky(): i64 { throw new E(); }
         function main() { const s = [1]; try { take(s); risky(); } catch (e) { console.log(s); } }",
    );
    assert_eq!(shares(func(&p, "main")), 1);
}

#[test]
fn global_constants_are_not_movable() {
    let r = err_src(
        "struct P { s: string; } const K: P = P { s: \"k\" };
         function main() { const p = K; console.log(p.s); }",
    );
    assert!(r.contains("cannot move out of module constant `K`"), "{r}");
    ok_src("const S = \"k\"; function main() { const s = S; console.log(s, S); }");
}

#[test]
fn destructuring_moves_or_borrows() {
    let p = ok_src(
        "function main() { const o = { a: `x${1}`, n: 1 }; const { a, n } = o; const k = a; console.log(k, n); }",
    );
    let main = func(&p, "main");
    assert!(uses_of(main, "o").contains(&UseMode::Move));
    let p2 = ok_src("function main() { const o = { a: `x${1}`, n: 1 }; const { a, n } = o; console.log(a, n, o.a); }");
    assert!(
        !uses_of(func(&p2, "main"), "o").contains(&UseMode::Move),
        "bindings borrow"
    );
    ok_src(
        "function main() { const o = { a: 1, n: 2 }; const { a, n } = o; console.log(a, n, o.a); }",
    );
    assert!(exprs(main)
        .iter()
        .any(|e| matches!(e.kind, E::AdtLit { .. })));
}

#[test]
fn fields_of_borrowed_values_are_shared() {
    ok_src("function main() { const xs = [{ a: [1] }]; const ys = xs.map((t) => t.a); }");
    ok_src("function main() { const xs = [{ a: [1] }]; const ys = xs.map((t) => t.a.clone()); }");
    ok_src("function main() { const xs = [{ a: `x${1}` }]; const ys = xs.map((t) => t.a); }");
}

#[test]
fn object_types_are_not_copy() {
    let p = ok_src(
        "type P = { x: i64 };
         function main() { const a: P = { x: 1 }; const b = a; console.log(a.x, b.x); }",
    );
    assert_eq!(shares(func(&p, "main")), 1);
    ok_src(
        "struct S { x: i64; }
         function main() { const a = S { x: 1 }; const b = a; console.log(a.x, b.x); }",
    );
}
