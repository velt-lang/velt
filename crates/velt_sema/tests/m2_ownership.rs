//! Ownership in M2: parameter / receiver / binding ownership inference, moves out of borrowed
//! places, partial moves, closures moving their captures.

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

#[test]
fn use_after_passing_to_owned_param_is_an_error() {
    let r = err_src(
        "function take(s: i64[]): i64[] { return s; }
         function main() { const a = [1]; take(a); console.log(a); }",
    );
    assert!(r.contains("use of moved value `a`"), "{r}");
    assert!(r.contains("main.vlt:2:64"), "{r}");
    // A string argument still used afterwards is copied instead.
    let p = ok_src(
        "function take(s: string): string { return s; }
         function main() { const a = `x${1}`; take(a); console.log(a); }",
    );
    let clones = calls(func(&p, "main"))
        .into_iter()
        .filter(|(c, _)| matches!(c, Callee::Intrinsic(velt_sema::hir::Intrinsic::Clone)))
        .count();
    assert_eq!(clones, 1);
}

#[test]
fn modified_and_moved_params_are_owned() {
    let r = err_src(
        "function f(xs: i64[]): i64[] { xs.push(1); return xs; }
         function main() { const a: i64[] = []; f(a); console.log(a); }",
    );
    assert!(r.contains("use of moved value `a`"), "{r}");
}

#[test]
fn moving_out_of_array_elements_and_for_of_bindings() {
    let r = err_src("function main() { const xs = [[1]]; let s = xs[0]; console.log(s); }");
    assert!(
        r.contains("cannot move out of an array element; use .clone() or pop()"),
        "{r}"
    );
    // A `const` refers to the element in place (`body::const_borrow`).
    ok_src("function main() { const xs = [[1]]; const s = xs[0]; console.log(s); }");
    let r = err_src(
        "function main() { const xs = [[1]]; const out: i64[][] = []; for (const s of xs) { out.push(s); } }",
    );
    assert!(
        r.contains("cannot move out of `s`, which borrows an array element"),
        "{r}"
    );
    ok_src("function main() { const xs = [[1]]; const out: i64[][] = []; for (const s of xs) { out.push(s.clone()); } }");
    // Strings are values: elements are copied out.
    ok_src("function main() { const xs = [\"a\"]; const s = xs[0]; const out: string[] = []; for (const t of xs) { out.push(t); } console.log(s, xs, out); }");
}

#[test]
fn class_fields_cannot_be_moved_out() {
    let r = err_src(
        "class U { items: i64[] = []; }
         function main() { const u = new U(); const out: i64[][] = []; out.push(u.items); }",
    );
    assert!(
        r.contains("cannot move a field out of a class instance"),
        "{r}"
    );
    ok_src(
        "class U { items: i64[] = []; }
         function main() { const u = new U(); const n = u.items; console.log(n); }",
    );
    let r = err_src(
        "class U { items: i64[] = []; }
         function main() { const u = new U(); const n = u.items; u.items = [2]; console.log(n); }",
    );
    assert!(
        r.contains("cannot modify `u.items` while `n` refers to `u.items`"),
        "{r}"
    );
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
    let r = err_src(
        "struct P { a: i64[]; b: i64[]; }
         function main() { const p = P { a: [1], b: [2] }; const a = p.a; console.log(p.a, a); }",
    );
    assert!(r.contains("use of moved value `p`"), "{r}");
    let r = err_src(
        "struct P { a: i64[]; b: i64[]; }
         function main() { const p = P { a: [1], b: [2] }; const a = p.a; console.log(p, a); }",
    );
    assert!(r.contains("use of moved value `p`"), "{r}");
    ok_src(
        "struct P { a: string; b: string; }
         function main() { const p = P { a: \"x\", b: \"y\" }; const a = p.a; console.log(p, p.a, a); }",
    );
}

#[test]
fn copy_structs_copy_and_classes_move() {
    ok_src("struct P { x: f64; } function main() { const p = P { x: 1.0 }; const q = p; console.log(p.x, q.x); }");
    let r = err_src("class C { x: f64 = 1.0; } function main() { const p = new C(); const q = p; console.log(p.x, q.x); }");
    assert!(r.contains("use of moved value `p`"), "{r}");
    assert!(r.contains("clone()"), "{r}");
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
    let r = err_src(
        "extend<T> Array<T> { take(): T[] { return this; } }
         function main() { const xs = [1, 2]; const ys = xs.take(); console.log(xs.length, ys.length); }",
    );
    assert!(r.contains("use of moved value `xs`"), "{r}");
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
fn escaping_closures_move_captures() {
    let r = err_src(
        "function main() { const s = [1]; const f = () => s; console.log(s); console.log(f()); }",
    );
    assert!(r.contains("use of moved value `s`"), "{r}");
    assert!(r.contains("shared(s)"), "{r}");
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
fn closures_cannot_move_their_params_or_captures() {
    let r = err_src(
        "function main() { const out: i64[][] = []; const xs = [[1]]; xs.forEach((x) => { out.push(x); }); }",
    );
    assert!(
        r.contains("cannot move out of `x`, which is borrowed"),
        "{r}"
    );
    let r = err_src("function main() { const s = [1]; const f = () => { const t = s; return t; }; console.log(f()); }");
    assert!(r.contains("cannot move captured variable `s`"), "{r}");
    // Strings are copied instead.
    ok_src("function main() { const out: string[] = []; const xs = [\"a\"]; xs.forEach((x) => { out.push(x); }); }");
    ok_src("function main() { const s = `a${1}`; const f = () => { const t = s; return t; }; console.log(f()); }");
}

#[test]
fn virtual_method_params_are_borrowed() {
    let r = err_src(
        "class A { items: i64[][] = []; add(s: i64[]) { this.items.push(s); } }
         class B extends A { override add(s: i64[]) { this.items.push(s); } }
         function main() { let b = new B(); b.add([1]); }",
    );
    assert!(
        r.contains("cannot move out of `s`, which is borrowed"),
        "{r}"
    );
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
    let r = err_src(
        "function take(s: i64[]): i64[] { return s; }
         function main() { const s = [1]; for (let i = 0; i < 2; i++) { take(s); } }",
    );
    assert!(r.contains("use of moved value `s`"), "{r}");
    ok_src(
        "function take(s: i64[]): i64[] { return s; }
         function main() { let s = [1]; for (let i = 0; i < 2; i++) { take(s); s = [2]; } }",
    );
}

#[test]
fn try_catch_moves_are_joined() {
    let r = err_src(
        "class E { m: string = \"e\"; }
         function take(s: i64[]): i64[] { return s; }
         function risky(): i64 { throw new E(); }
         function main() { const s = [1]; try { take(s); risky(); } catch (e) { console.log(s); } }",
    );
    assert!(r.contains("use of moved value `s`"), "{r}");
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
fn moving_a_field_out_of_a_borrow_names_the_place() {
    let r = err_src("function main() { const xs = [{ a: [1] }]; const ys = xs.map((t) => t.a); }");
    assert!(
        r.contains("cannot move `t.a` out of `t`, which is borrowed"),
        "{r}"
    );
    assert!(r.contains("`t.a.clone()`"), "{r}");
    ok_src("function main() { const xs = [{ a: [1] }]; const ys = xs.map((t) => t.a.clone()); }");
    ok_src("function main() { const xs = [{ a: `x${1}` }]; const ys = xs.map((t) => t.a); }");
}

#[test]
fn object_types_are_not_copy() {
    let r = err_src(
        "type P = { x: i64 };
         function main() { const a: P = { x: 1 }; const b = a; console.log(a.x, b.x); }",
    );
    assert!(r.contains("use of moved value `a`"), "{r}");
    ok_src(
        "struct S { x: i64; }
         function main() { const a = S { x: 1 }; const b = a; console.log(a.x, b.x); }",
    );
}
