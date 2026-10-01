//! Member rules: `private`, `readonly`, getters and `static readonly` fields.

mod common;

use common::hir_walk::{calls, def_id, exprs, func};
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Callee, Def, ExprKind as E};

#[test]
fn private_members_are_usable_inside_the_body_only() {
    ok_src(
        "class C { private a: i64 = 1; private f(): i64 { return this.a; }
           g(): i64 { const xs = [1, 2]; return xs.reduce((acc, x) => acc + x * this.f(), this.a); }
           static make(): C { const c = new C(); console.log(c.a); return c; } }
         function main() { console.log(C.make().g()); }",
    );
    let r = err_src("class C { private a: i64 = 1; } function main() { console.log(new C().a); }");
    assert!(r.contains("`a` is private"), "{r}");
    assert!(r.contains("inside the body of `C`"), "{r}");
    let r = err_src(
        "class C { private f(): i64 { return 1; } } function main() { console.log(new C().f()); }",
    );
    assert!(r.contains("`f` is private"), "{r}");
    let r = err_src(
        "class C { private static f(): i64 { return 1; } } function main() { console.log(C.f()); }",
    );
    assert!(r.contains("`f` is private"), "{r}");
}

#[test]
fn private_is_not_visible_to_subclasses_literals_or_patterns() {
    let r = err_src(
        "class A { private a: i64 = 1; } class B extends A { f(): i64 { return this.a; } }
         function main() {}",
    );
    assert!(r.contains("`a` is private"), "{r}");
    let r = err_src("struct S { private a: i64; } function main() { const s = S { a: 1 }; }");
    assert!(r.contains("`a` is private"), "{r}");
    let r = err_src(
        "struct S { private a: i64; static make(): S { return S { a: 1 }; } }
         function main() { const { a } = S.make(); console.log(a); }",
    );
    assert!(r.contains("`a` is private"), "{r}");
}

#[test]
fn readonly_fields_are_assignable_in_the_constructor_only() {
    ok_src(
        "class C { readonly a: i64; constructor() { this.a = 1; } }
         function main() { console.log(new C().a); }",
    );
    let r =
        err_src("class C { readonly a: i64 = 1; } function main() { let c = new C(); c.a = 2; }");
    assert!(
        r.contains("cannot assign to `a`: it is a readonly field"),
        "{r}"
    );
}

#[test]
fn getters_read_as_properties() {
    let p = ok_src(
        "class C { n: i64 = 2; get twice(): i64 { return this.n * 2; } }
         function main() { const c = new C(); console.log(c.twice); }",
    );
    let getter = def_id(&p, "C.twice");
    let called = calls(func(&p, "main"))
        .iter()
        .any(|(c, args)| matches!(c, Callee::Def(d, _) if *d == getter) && args.len() == 1);
    assert!(
        called,
        "`c.twice` is a call of the getter with the receiver only"
    );
    let r = err_src(
        "class C { get x(): i64 { return 1; } } function main() { let c = new C(); c.x = 2; }",
    );
    assert!(r.contains("cannot assign to `x`: it is a getter"), "{r}");
    let r = err_src(
        "class C { get x(): i64 { return 1; } } function main() { console.log(new C().x()); }",
    );
    assert!(r.contains("`x` is a getter, not a method"), "{r}");
    let r = err_src("class C { x: i64 = 1; get x(): i64 { return 1; } } function main() {}");
    assert!(r.contains("is both a field and a method"), "{r}");
}

#[test]
fn interface_getters_dispatch_and_must_match() {
    ok_src(
        "interface Sized { get size(): i64; get big(): bool { return this.size > 10; } }
         class A implements Sized { get size(): i64 { return 11; } }
         function f<T extends Sized>(x: T): bool { return x.big && x.size > 0; }
         function main() { const xs: Sized[] = [new A()]; console.log(xs[0].size, f(new A())); }",
    );
    let r = err_src(
        "interface Sized { get size(): i64; }
         class A implements Sized { size(): i64 { return 1; } } function main() {}",
    );
    assert!(
        r.contains("must be a getter to implement `Sized.size`"),
        "{r}"
    );
}

#[test]
fn static_readonly_fields_are_globals() {
    let p = ok_src(
        "class K { static readonly N: i64 = 3; private static readonly M: i64 = 4;
           static both(): i64 { return K.N + K.M; } }
         function main() { console.log(K.N, K.both(), Math.PI); }",
    );
    let n = def_id(&p, "K.N");
    assert!(matches!(p.def(n), Def::Global(_)));
    let reads = exprs(func(&p, "main"))
        .iter()
        .filter(|e| matches!(e.kind, E::Global(g) if g == n))
        .count();
    assert_eq!(reads, 1);
    let r = err_src(
        "class K { private static readonly M: i64 = 4; } function main() { console.log(K.M); }",
    );
    assert!(r.contains("`M` is private"), "{r}");
    let r = err_src("class K { static N: i64 = 1; } function main() {}");
    assert!(r.contains("static fields must be `readonly`"), "{r}");
    let r = err_src("class K { static readonly N: i64 = 1; } function main() { K.N = 2; }");
    assert!(r.contains("static fields are readonly"), "{r}");
}

#[test]
fn prelude_internals_are_private() {
    let r = err_src("function main() { const m = new Map<string, i64>(); console.log(m.slots); }");
    assert!(r.contains("`slots` is private"), "{r}");
    let r = err_src("function main() { const m = new Mutex<i64>(1); console.log(m.value); }");
    assert!(r.contains("`value` is private"), "{r}");
    ok_src("function main() { const m = new Map<string, i64>(); console.log(m.size); }");
}

#[test]
fn optional_chains_short_circuit() {
    ok_src(
        "class User { name: string = \"ann\"; }
         function len(u: User | null): usize { return u?.name.length ?? 0; }
         function up(u: User | null): string { return u?.name.toUpperCase() ?? \"-\"; }
         function main() { console.log(len(null), up(new User())); }",
    );
}
