//! Declarations: interface inheritance and nested (block-level) declarations.

mod common;

use common::hir_walk::{def_id, func};
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Callee, Def, ExprKind, Intrinsic, StmtKind, TyKind};

#[test]
fn interface_extends_flattens_slots_and_impls() {
    let p = ok_src(
        "interface A { a: i64; f(): i64; g(): i64 { return this.f() + this.a; } }
         interface B extends A { h(): i64; }
         class C implements B { a: i64 = 1; f(): i64 { return 2; } h(): i64 { return this.g(); } }
         function use<T extends B>(x: T): i64 { return x.g() + x.a; }
         function useA<T extends A>(x: T): i64 { return x.f(); }
         function viaB<T extends B>(x: T): i64 { return useA(x); }
         function main() { const c = new C(); const a: A = new C();
           console.log(use(c), viaB(c), a.g()); }",
    );
    let Def::Interface(b) = p.def(def_id(&p, "B")) else {
        panic!("B")
    };
    let names: Vec<&str> = b.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        names,
        ["h", "f", "g", "<a>"],
        "own slots, inherited ones, field getters"
    );
    assert_eq!(b.fields.len(), 1);
    let fwd = b.methods[2].default.expect("inherited default");
    assert_eq!(
        func(&p, "B.g").generics,
        1,
        "B's default is generic over Self only"
    );
    assert!(matches!(p.def(fwd), Def::Fn(_)));
    let c = def_id(&p, "C");
    let impls: Vec<&str> = p
        .impls
        .iter()
        .filter(|i| matches!(p.types.kind(i.ty), TyKind::Adt(d, _) if *d == c))
        .map(|i| match p.def(i.iface) {
            Def::Interface(x) => x.name.as_str(),
            _ => "?",
        })
        .collect();
    assert_eq!(impls, ["B", "A"], "implementing B implements its ancestors");
}

#[test]
fn interface_extends_generic_and_errors() {
    ok_src(
        "interface Get<T> { get(): T; twice(): T[] { return [this.get(), this.get()]; } }
         interface IntGet extends Get<i64> { }
         class K implements IntGet { get(): i64 { return 3; } }
         function main() { console.log(new K().twice()); }",
    );
    let r = err_src("interface A extends B {} interface B extends A {} function main() {}");
    assert!(r.contains("interface inheritance cycle"), "{r}");
    let r = err_src(
        "interface A { f(): i64; } interface B extends A { f(): string; } function main() {}",
    );
    assert!(r.contains("must have the same signature as in `A`"), "{r}");
    let r = err_src(
        "interface A { f(): i64; } interface B extends A { g(): i64; }
         class C implements B { g(): i64 { return 1; } } function main() {}",
    );
    assert!(r.contains("missing method `f` required by `B`"), "{r}");
}

#[test]
fn nested_declarations_are_hoisted_and_scoped() {
    let p = ok_src(
        "function main() { console.log(twice(2)); function twice(n: i64): i64 { return n * 2; }
           { struct P { a: i64; } const p = P { a: 1 }; console.log(p.a); }
           { struct P { b: string; } const q = P { b: \"x\" }; console.log(q.b); } }",
    );
    func(&p, "main::twice");
    assert!(matches!(p.def(def_id(&p, "main::P")), Def::Adt(_)));
    assert!(matches!(p.def(def_id(&p, "main::2::P")), Def::Adt(_)));
    let r = err_src("function main() { { function f() {} } f(); }");
    assert!(r.contains("cannot find `f` in this scope"), "{r}");
}

#[test]
fn nested_functions_capture_as_closures() {
    // A nested function using a local is a closure (`nested_closures`); ones calling each
    // other form a recursive closure group: each captures the group's variables in one order
    // and makes the members it calls from its own environment (`ClosureSibling`).
    let p = ok_src(
        "function main() { const k = 1; const j = 2;
           function f(n: i64): i64 { return n > k ? g(n - 1) : k; }
           function g(n: i64): i64 { return n > j ? f(n - 1) : f(n); }
           console.log(f(3)); }",
    );
    for name in ["main::{closure#0}", "main::{closure#1}"] {
        let c = func(&p, name);
        let caps: Vec<&str> = c
            .captures
            .iter()
            .map(|c| c.inner)
            .map(|l| c.body.locals[l.0 as usize].name.as_str())
            .collect();
        assert_eq!(
            caps,
            ["j", "k"],
            "{name}: the group's captures, in one order"
        );
        let siblings = c
            .body
            .block
            .stmts
            .iter()
            .filter(|s| {
                matches!(&s.kind, StmtKind::Let { init: Some(e), .. }
                if matches!(&e.kind, ExprKind::Call {
                    callee: Callee::Intrinsic(Intrinsic::ClosureSibling(_)),
                    ..
                }))
            })
            .count();
        assert_eq!(siblings, 1, "{name}: the member it calls, made once");
    }
    // A member's parameter or top-level local may be named like a variable another member
    // captures: the group's captures are in a scope outside the member's own.
    ok_src(
        "function main() { let v = 1;
           function a(v: i64): i64 { const k = v; return k > 0 ? b(k - 1) : 0; }
           function b(k: i64): i64 { v++; const w = 2; return a(k) + w; }
           function c(k: i64): i64 { const v = 100; return k > 0 ? d(k - 1) + v : 0; }
           function d(k: i64): i64 { v++; return c(k); }
           console.log(a(3), c(2), v); }",
    );
    // A generator calling itself is not supported yet, and reported once.
    let r = err_src(
        "function main() { const k = 1; function* f(n: i64): Generator<i64> { if (n > 0) { for (const x of f(n - 1)) yield x + k; } } for (const x of f(2)) console.log(x); }",
    );
    assert!(
        r.contains("`f` uses local variables of the enclosing function and calls itself"),
        "{r}"
    );
    assert!(!r.contains("cannot be captured"), "{r}");
    let r = err_src(
        "function main() { const k = 1; function f<T>(x: T): i64 { return k; } console.log(f(1)); }",
    );
    assert!(
        r.contains("`k` cannot be captured by a nested function"),
        "{r}"
    );
    let r = err_src("function main() { const k = 1; class C { f(): i64 { return k; } } }");
    assert!(r.contains("`k` cannot be captured"), "{r}");
}

#[test]
fn interface_method_signatures_take_no_defaults() {
    // TypeScript's TS2371: the interface declares `b?: i64`, each implementation its default.
    let r = err_src("interface G<T> { f(a: T, b: i64 = 2): i64; } function main() {}");
    assert!(
        r.contains(
            "a parameter initializer is only allowed in a function or constructor implementation"
        ),
        "{r}"
    );
    let p = ok_src(
        "interface G<T> { f(a: T, b?: i64): i64; g(b: i64 = 3): i64 { return b; } }
         interface H extends G<string> { }
         class C implements H { f(a: string, b: i64 | undefined = 5): i64 { return b; } }
         function viaParam<X extends H>(x: X): i64 { return x.f(\"a\") + x.g(); }
         function main() { const g: G<string> = new C(); console.log(g.f(\"a\"), viaParam(new C())); }",
    );
    let lits = |name: &str| -> Vec<u128> {
        common::hir_walk::exprs(func(&p, name))
            .iter()
            .filter_map(|e| match e.kind {
                velt_sema::hir::ExprKind::Lit(velt_sema::hir::Lit::Int(v)) => Some(v),
                _ => None,
            })
            .collect()
    };
    assert!(lits("viaParam").contains(&3), "a default body's default");
}

#[test]
fn abstract_methods_and_declared_functions_take_no_defaults() {
    // TypeScript's TS2371 for every signature without a body; the subclass, or a function that
    // calls the declared one, gives the default.
    let init = "a parameter initializer is only allowed in a function or constructor implementation";
    let r = err_src("abstract class A { abstract m(a: i64 = 1): void; } function main() {}");
    assert!(r.contains(init) && r.contains("an implementation gives its own"), "{r}");
    let r = err_src("declare function d(a: i64 = 3): void; function main() {}");
    assert!(r.contains(init) && r.contains("a function that calls this one"), "{r}");
    ok_src(
        "abstract class A { abstract m(a?: i64): i64; }
         class B extends A { m(a: i64 | undefined = 2): i64 { return a ?? 0; } }
         declare function d(a?: i64): void;
         function main() { const b: A = new B(); console.log(b.m()); }",
    );
}

#[test]
fn field_initializers_use_parameter_defaults() {
    ok_src(
        "class Pat { src: string; flags: string;
           constructor(src: string, flags: string = \"\") { this.src = src; this.flags = flags; } }
         function two(a: i64, b: i64 = 2): i64 { return a + b; }
         class Holder { p: Pat = new Pat(\"a+\"); k: i64 = two(1); }
         function main() { const h = new Holder(); console.log(h.p.src, h.k); }",
    );
}

#[test]
fn type_param_takes_the_rest_of_a_union() {
    ok_src(
        "class E { code: i64 = 1; } class A { n: i64 = 2; }
         function must<T>(r: T | E): T { if (r instanceof E) { throw new Error(\"e\"); } return r; }
         function get(): A | E { return new A(); }
         function main() { console.log(must(get()).n); }",
    );
}

#[test]
fn extend_blocks_add_static_methods() {
    ok_src(
        "class C { n: i64 = 1; }
         extend C { static make(): C { return new C(); } }
         function main() { console.log(C.make().n); }",
    );
}

#[test]
fn untyped_let_takes_the_type_of_its_first_assignment() {
    let p = ok_src(
        "function f(s: string): string { let r; try { r = s + \"!\"; } catch (e) { return \"\"; } return r; }
         function main() { console.log(f(\"a\")); }",
    );
    let f = func(&p, "f");
    let r = f
        .body
        .locals
        .iter()
        .find(|l| l.name == "r")
        .expect("local r");
    assert!(matches!(p.types.kind(r.ty), TyKind::Str), "{:?}", r.ty);
    for (src, why) in [
        (
            "let a; console.log(a); a = 1;",
            "`a` is read here before it is first assigned",
        ),
        (
            "let b; b += 1; b = 2;",
            "`b` is updated here before it is first assigned",
        ),
        (
            "let c; c++;",
            "`c` is updated here before it is first assigned",
        ),
        (
            "let d; const g = () => d; d = 3;",
            "a closure uses `d` before the function first assigns it",
        ),
        ("let e;", "`e` is never assigned"),
        (
            "let x; const g = () => { x = 5; }; g(); console.log(x);",
            "a closure uses `x` before the function first assigns it",
        ),
        (
            "let a; console.log(a); console.log(a);",
            "`a` is read here before it is first assigned",
        ),
    ] {
        let r = err_src(&format!("function main() {{ {src} }}"));
        assert!(r.contains("type annotations needed for"), "{src}: {r}");
        assert!(r.contains(why), "{src}: {r}");
        assert_eq!(
            r.matches("type annotations needed").count(),
            1,
            "{src}: {r}"
        );
    }
    // `let x = []` and `let x; x = []` are rejected alike (the element type is unknown).
    let r = err_src("function main() { let x; x = []; x.push(1); }");
    assert!(r.contains("cannot infer the element type of `[]`"), "{r}");
}
