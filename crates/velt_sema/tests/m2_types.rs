//! M2 type definitions: structs, anonymous objects, classes (constructors, inheritance,
//! overrides, dispatch), enums + switch + discriminated unions, generics, interfaces, modules and intrinsics.

mod common;

use common::hir_walk::{adt, calls, def_id, exprs, func};
use common::programs::{err_src, load_src_at, ok_src, repo_root};
use velt_sema::hir::{AdtKind, Callee, Def, ExprKind as E, Intrinsic, PassMode, TyKind, UseMode};

// ───────────────────────────── structs & objects ─────────────────────────────

#[test]
fn struct_literals_fields_and_defaults() {
    let p = ok_src(
        "struct P { x: f64; y: f64 = 2.0; tag?: string; }
         function main() { const p = P { x: 1.0 }; const q: P = { y: 3.0, x: 4.0 }; console.log(p.x, q.y, p.tag ?? \"-\"); }",
    );
    let lits: Vec<usize> = exprs(func(&p, "main"))
        .iter()
        .filter_map(|e| match &e.kind {
            E::AdtLit { fields, .. } => Some(fields.len()),
            _ => None,
        })
        .collect();
    assert_eq!(
        lits,
        vec![3, 3],
        "omitted fields take defaults; order = declaration order"
    );
    assert!(
        !adt(&p, "P").is_copy,
        "an optional string field is not Copy"
    );
}

#[test]
fn struct_literal_errors() {
    let r = err_src("struct P { x: f64; } function main() { const p = P { }; }");
    assert!(r.contains("missing field `x` in `P` literal"), "{r}");
    let r = err_src("struct P { x: f64; } function main() { const p = P { x: 1.0, z: 2 }; }");
    assert!(r.contains("no field `z` on type `P`"), "{r}");
    let r = err_src("class C { } function main() { const c = C { }; }");
    assert!(r.contains("`C` is a class"), "{r}");
}

#[test]
fn anonymous_objects_share_a_def_per_shape() {
    let p = ok_src(
        "function main() { const a = { x: 1, s: \"a\" }; const b = { x: 2, s: \"b\" }; const c = { s: \"c\", x: 3 }; console.log(a.x, b.s, c.x); }",
    );
    let anon: Vec<&str> = p
        .defs
        .iter()
        .filter_map(|d| match d {
            Def::Adt(a) if a.kind == AdtKind::Anon => Some(a.name.as_str()),
            _ => None,
        })
        .collect();
    // The prelude's own object types (e.g. `PromiseSettledResult`) are not this program's.
    let own: Vec<&&str> = anon.iter().filter(|n| n.contains("x: i64")).collect();
    assert_eq!(own.len(), 2, "{anon:?}");
}

#[test]
fn adding_a_property_is_an_error_suggesting_map() {
    let r = err_src("function main() { let o = { a: 1 }; o.b = 2; }");
    assert!(r.contains("no field `b`"), "{r}");
    assert!(r.contains("Map"), "{r}");
}

#[test]
fn modifying_methods_and_receivers_are_inferred() {
    let p = ok_src(
        "struct C { n: i64; inc() { this.n += 1; } get(): i64 { return this.n; } }
         function f(c: C) { c.inc(); }
         class K { c: C = C { n: 0 }; }
         function g(k: K) { k.c.inc(); }
         function h(k: K): i64 { return k.c.get(); }
         function main() { const c = C { n: 0 }; c.inc(); console.log(c.get()); f(c);
           const k = new K(); g(k); console.log(h(k)); }",
    );
    assert_eq!(func(&p, "C.inc").params[0].mode, PassMode::BorrowMut);
    assert_eq!(func(&p, "C.get").params[0].mode, PassMode::Borrow);
    // Structs are objects (semantics stage 2): the caller sees the change.
    assert_eq!(func(&p, "f").params[0].mode, PassMode::BorrowMut);
    assert_eq!(func(&p, "g").params[0].mode, PassMode::BorrowMut);
    assert_eq!(func(&p, "h").params[0].mode, PassMode::Borrow);
}

#[test]
fn clone_on_everything() {
    let p = ok_src(
        "class C { n: i64 = 0; } function main() { const c = new C(); const d = c.clone(); const s = \"x\".clone(); console.log(d.n, s, [1].clone().length); }",
    );
    assert!(calls(func(&p, "main"))
        .iter()
        .any(|(c, _)| matches!(c, Callee::Intrinsic(Intrinsic::Clone))));
}

// ───────────────────────────── classes ─────────────────────────────

#[test]
fn constructor_rules() {
    let r = err_src("class C { a: i64; constructor() { } } function main() {}");
    assert!(
        r.contains("field `a` is not initialized by the constructor of `C`"),
        "{r}"
    );
    ok_src("class C { a: i64; constructor(x: bool) { if (x) { this.a = 1; } else { this.a = 2; } } } function main() {}");
    let r = err_src("class C { a: i64; } function main() {}");
    assert!(
        r.contains("field `a` of class `C` has no default value"),
        "{r}"
    );
    let r = err_src(
        "class A { n: i64; constructor(n: i64) { this.n = n; } }
         class B extends A { constructor() { console.log(1); super(2); } }
         function main() {}",
    );
    assert!(
        r.contains("`super(...)` must be the first statement"),
        "{r}"
    );
    let r = err_src("class C { constructor(a: i64) {} } function main() { new C(); }");
    assert!(
        r.contains("takes 1 argument but 0 arguments were supplied"),
        "{r}"
    );
    let r = err_src("class C { readonly a: i64 = 1; f() { this.a = 2; } } function main() {}");
    assert!(r.contains("readonly field"), "{r}");
}

#[test]
fn inherited_constructor_and_fields() {
    let p = ok_src(
        "class A<T> { v: T; constructor(v: T) { this.v = v; } }
         class B extends A<string> { extra: i64 = 1; }
         function main() { const b = new B(\"x\"); console.log(b.v, b.extra); }",
    );
    let b = adt(&p, "B");
    assert_eq!(b.ctor, Some(def_id(&p, "A.constructor")));
    assert_eq!(b.fields.len(), 2);
}

#[test]
fn override_rules() {
    let r = err_src(
        "class A { f(): i64 { return 1; } } class B extends A { f(): i64 { return 2; } } function main() {}",
    );
    assert!(r.contains("redefines a base class method"), "{r}");
    assert!(r.contains("override"), "{r}");
    let r = err_src(
        "class A { } class B extends A { override f(): i64 { return 2; } } function main() {}",
    );
    assert!(r.contains("no base class has a method `f`"), "{r}");
    let r = err_src(
        "class A { f(): i64 { return 1; } } class B extends A { override f(): f64 { return 2.0; } } function main() {}",
    );
    assert!(r.contains("does not have the same signature"), "{r}");
}

#[test]
fn devirtualized_unless_overridden_and_static_class_has_slot() {
    let p = ok_src(
        "class A { f(): i64 { return 1; } g(): i64 { return 2; } }
         class B extends A { override f(): i64 { return 3; } }
         class C extends B { }
         function main() { const a: A = new C(); const b = new B(); console.log(a.f(), a.g(), b.f(), b.g()); }",
    );
    let main = func(&p, "main");
    let cs = calls(main);
    let virt = cs
        .iter()
        .filter(|(c, _)| matches!(c, Callee::Virtual { .. }))
        .count();
    assert_eq!(virt, 2, "a.f() and b.f() dispatch; g() is never overridden");
    assert_eq!(adt(&p, "C").vtable, vec![def_id(&p, "B.f")]);
    let g = def_id(&p, "A.g");
    let (_, args) = cs
        .iter()
        .find(|(c, _)| matches!(c, Callee::Def(d, _) if *d == g))
        .unwrap();
    assert!(matches!(args[0].kind, E::Local(_, UseMode::Borrow)));
    assert!(cs.iter().any(
        |(c, a)| matches!(c, Callee::Def(d, _) if *d == g) && matches!(a[0].kind, E::Upcast(_))
    ));
}

#[test]
fn super_method_calls_are_direct() {
    let p = ok_src(
        "class A { f(): i64 { return 1; } }
         class B extends A { override f(): i64 { return super.f() + 1; } }
         function main() { console.log(new B().f()); }",
    );
    let af = def_id(&p, "A.f");
    assert!(calls(func(&p, "B.f"))
        .iter()
        .any(|(c, _)| matches!(c, Callee::Def(d, _) if *d == af)));
}

// ─────────────────────── switch, enums, discriminated unions ───────────────────────

#[test]
fn non_exhaustive_switch_lists_missing_cases() {
    let r = err_src(
        "type S = { kind: \"a\"; n: i64 } | { kind: \"b\" } | { kind: \"c\" };
         function f(s: S): i64 { switch (s.kind) { case \"a\": return s.n; } } function main() {}",
    );
    assert!(r.contains("non-exhaustive switch on `s.kind`"), "{r}");
    assert!(r.contains("missing cases: \"b\", \"c\""), "{r}");
    let r = err_src(
        "function f(x: \"a\" | \"b\" | null): i64 { switch (x) { case \"a\": case \"b\": return 0; } } function main() {}",
    );
    assert!(r.contains("missing cases: null"), "{r}");
    let r = err_src(
        "enum E { A, B } function f(e: E): i64 { switch (e) { case E.A: return 1; } } function main() {}",
    );
    assert!(r.contains("missing cases: E.B"), "{r}");
    // Numbers, strings and bools need no `default`.
    ok_src(
        "function f(n: i64): i64 { switch (n) { case 1: return 1; } return 0; } function main() {}",
    );
}

#[test]
fn negative_case_values_are_twos_complement() {
    let p = ok_src("function f(n: i8): i64 { switch (n) { case -1: return 1; case -128: return 2; default: return 3; } } function main() {}");
    let lits: Vec<u128> = common::hir_walk::pats(func(&p, "f"))
        .iter()
        .filter_map(|x| match &x.kind {
            velt_sema::hir::PatKind::Lit(velt_sema::hir::Lit::Int(v)) => Some(*v),
            _ => None,
        })
        .collect();
    assert_eq!(lits, vec![0xff, 0x80]);
}

#[test]
fn switch_breaks_leave_a_run_once_loop() {
    // Without `break`, the switch is a match; a `break` makes it a labelled one-pass loop.
    let p = ok_src("function f(n: i64): i64 { switch (n) { case 1: return 1; default: return 2; } } function main() {}");
    assert!(!common::hir_walk::has_while(func(&p, "f")));
    let p = ok_src(
        "function f(n: i64): i64 { let r = 0; switch (n) { case 1: r = 1; break; default: r = 2; } return r; } function main() {}",
    );
    assert!(common::hir_walk::has_while(func(&p, "f")));
    let r = err_src("function f(n: i64) { switch (n) { case 1: continue; } } function main() {}");
    assert!(r.contains("`continue` outside of a loop"), "{r}");
}

#[test]
fn enums_cast_and_compare() {
    let p = ok_src("enum C { R, G = 5 } function main() { console.log(C.G as i64, C.R == C.G); }");
    assert!(calls(func(&p, "main"))
        .iter()
        .any(|(c, _)| matches!(c, Callee::Intrinsic(Intrinsic::Same))));
    let r = err_src("enum S { A = \"a\" } function main() { console.log(S.A as i64); }");
    assert!(r.contains("cannot cast"), "{r}");
    ok_src("enum S { A = \"a\" } function main() { const s: string = S.A; console.log(s); }");
}

#[test]
fn object_literals_pick_the_union_member_by_discriminant() {
    let p = ok_src(
        "type S = { kind: \"a\"; n: i64 } | { kind: \"b\"; n: i64 };
         function main() { const s: S = { kind: \"b\", n: 1 }; console.log(s.n); }",
    );
    let variants: Vec<u32> = common::hir_walk::exprs(func(&p, "main"))
        .iter()
        .filter_map(|e| match &e.kind {
            E::Variant { variant, .. } => Some(*variant),
            _ => None,
        })
        .collect();
    assert_eq!(variants.len(), 1);
    let r = err_src(
        "type S = { kind: \"a\" } | { kind: \"b\" }; function main() { const s: S = { kind: \"c\" }; }",
    );
    assert!(r.contains("`\"c\"` is not a valid `kind`"), "{r}");
}

// ───────────────────────────── generics ─────────────────────────────

#[test]
fn generic_functions_infer_and_check_bounds() {
    let p = ok_src(
        "function id<T>(x: T): T { return x; }
         function first<T>(xs: T[]): T | null { return xs.length > 0 ? xs[0].clone() : null; }
         function main() { const n = id(5); const s = id<string>(\"a\"); console.log(n, s, first([1.5]) ?? 0.0); }",
    );
    let id = def_id(&p, "id");
    let tys: Vec<String> = calls(func(&p, "main"))
        .iter()
        .filter_map(|(c, _)| match c {
            Callee::Def(d, ts) if *d == id => Some(format!("{:?}", p.types.kind(ts[0]))),
            _ => None,
        })
        .collect();
    assert_eq!(tys, vec!["Int(I64)", "Str"]);
    let r = err_src(
        "interface N { n(): i64; } function f<T extends N>(x: T): i64 { return x.n(); } function main() { f(5); }",
    );
    assert!(r.contains("the type `i64` does not implement `N`"), "{r}");
    let r = err_src("function f<T>(x: T): i64 { return x.n(); } function main() {}");
    assert!(r.contains("no method named `n` found for type `T`"), "{r}");
    let r = err_src("function mk<T>(): T[] { return []; } function main() { const x = mk(); }");
    assert!(
        r.contains("cannot infer type parameter `T` of function `mk`"),
        "{r}"
    );
    ok_src("function mk<T>(): T[] { return []; } function main() { const x: i64[] = mk(); console.log(x.length); }");
}

#[test]
fn generic_equality_uses_the_same_intrinsic() {
    // `==` is JS `===` (objects by identity, semantics stage 2): `Intrinsic::Same`.
    let p = ok_src("function same<T>(a: T, b: T): bool { return a == b; } function main() { console.log(same(1, 2)); }");
    assert!(calls(func(&p, "same"))
        .iter()
        .any(|(c, _)| matches!(c, Callee::Intrinsic(Intrinsic::Same))));
}

#[test]
fn generic_classes_and_methods() {
    let p = ok_src(
        "class Box<T> { v: T; constructor(v: T) { this.v = v; } map<U>(f: (x: T) => U): Box<U> { return new Box(f(this.v)); } }
         function main() { const b = new Box(2); const c = b.map((x) => `${x}`); console.log(c.v); }",
    );
    let map = def_id(&p, "Box.map");
    let ts = calls(func(&p, "main"))
        .iter()
        .find_map(|(c, _)| match c {
            Callee::Def(d, ts) if *d == map => Some(ts.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(ts.len(), 2, "class type args then the method's own");
    assert!(matches!(p.types.kind(ts[1]), TyKind::Str));
}

// ───────────────────────────── interfaces ─────────────────────────────

#[test]
fn implements_is_checked() {
    let r = err_src("interface I { f(): i64; } class C implements I { } function main() {}");
    assert!(
        r.contains("`C` is missing method `f` required by `I`"),
        "{r}"
    );
    let r = err_src("interface I { f(): i64; } class C implements I { f(): f64 { return 1.0; } } function main() {}");
    assert!(r.contains("different signature"), "{r}");
    let r = err_src("interface I { name: string; } class C implements I { } function main() {}");
    assert!(r.contains("must have a field `name: string`"), "{r}");
    let r =
        err_src("interface I { f(): i64; } struct S { } function main() { const x: I = S { }; }");
    assert!(r.contains("mismatched types"), "{r}");
}

#[test]
fn interface_values_and_default_methods() {
    let p = ok_src(
        "interface Named { name: string; greet(): string { return `hi ${this.name}`; } }
         class U implements Named { name: string = \"u\"; }
         class A extends U { override greet(): string { return \"admin\"; } }
         function all(xs: Named[]) { for (const x of xs) { console.log(x.greet()); } }
         function main() { const u = new U(); console.log(u.greet()); all([new U(), new A()]); }",
    );
    let greet = def_id(&p, "Named.greet");
    let main = func(&p, "main");
    // `A` overrides the default `greet` that `U` gets from `Named`: `U` gets a vtable slot.
    assert!(calls(main)
        .iter()
        .any(|(c, _)| matches!(c, Callee::Virtual { slot: 0 })));
    // ... holding a synthesized forwarder `U.greet` that calls the default with `Self = U`.
    assert_eq!(adt(&p, "U").vtable, vec![def_id(&p, "U.greet")]);
    assert!(calls(func(&p, "U.greet"))
        .iter()
        .any(|(c, _)| matches!(c, Callee::Def(d, _) if *d == greet)));
    assert_eq!(adt(&p, "A").vtable, vec![def_id(&p, "A.greet")]);
    assert!(calls(func(&p, "all"))
        .iter()
        .any(|(c, _)| matches!(c, Callee::Dyn { slot: 0 })));
    // `new A()` has no impl of its own: upcast to U, then ToDyn with U's impl.
    assert!(exprs(main)
        .iter()
        .any(|e| matches!(&e.kind, E::ToDyn { expr, .. } if matches!(expr.kind, E::Upcast(_)))));
    // `this.name` in the default method calls the field's getter slot (after the 1 method).
    assert!(calls(func(&p, "Named.greet"))
        .iter()
        .any(|(c, _)| matches!(c, Callee::ParamMethod { slot: 1, .. })));
    let iface = match p.def(def_id(&p, "Named")) {
        Def::Interface(i) => i,
        _ => panic!(),
    };
    assert_eq!(iface.methods.len(), 2);
    let u_impl = p
        .impls
        .iter()
        .find(|i| i.iface == def_id(&p, "Named"))
        .unwrap();
    // The impl entry for a method with a vtable slot dispatches virtually.
    assert_eq!(u_impl.methods[0], def_id(&p, "U.<dyn greet>"));
    assert!(calls(func(&p, "U.<dyn greet>"))
        .iter()
        .any(|(c, _)| matches!(c, Callee::Virtual { slot: 0 })));
    let getter = func(&p, "U.<name>");
    assert_eq!(u_impl.methods[1], def_id(&p, "U.<name>"));
    assert!(calls(getter)
        .iter()
        .any(|(c, _)| matches!(c, Callee::Intrinsic(Intrinsic::Share))));
}

// ───────────────────────────── modules & intrinsics ─────────────────────────────

#[test]
fn imports_exports_and_intrinsics() {
    let root = repo_root().join("tests/golden/m2/errors/private_import.vlt");
    let l = load_src_at(
        &root,
        "import { hidden } from \"./_private\"; function main() {}",
    );
    let (p, d) = l.check();
    assert!(p.is_none());
    assert!(l.render(&d).contains("`hidden` is not exported"));
    let r = err_src("function main() { const xs: i64[] = __intrinsic_array_with_capacity(4); }");
    assert!(
        r.contains("intrinsics can only be called from the standard library"),
        "{r}"
    );
    let r = err_src("import { nope } from \"velt:math\"; function main() {}");
    assert!(r.contains("has no member `nope`"), "{r}");
    ok_src("import { clamp as c } from \"velt:math\"; function main() { console.log(c(1, 2, 3), Math.PI); }");
    let r = err_src("const X: i64 = 1 + f(); function f(): i64 { return 1; } function main() {}");
    assert!(r.contains("constant expressions"), "{r}");
}

#[test]
fn static_methods() {
    let p = ok_src(
        "class K { static make(): K { return new K(); } n: i64 = 1; }
         function main() { const k = K.make(); console.log(k.n, Math.max(1.0, 2.0)); }",
    );
    assert!(calls(func(&p, "main"))
        .iter()
        .any(|(c, a)| matches!(c, Callee::Def(..)) && a.is_empty()));
    let r = err_src("class K { f(): i64 { return 1; } } function main() { K.f(); }");
    assert!(r.contains("is an instance method"), "{r}");
    let r = err_src(
        "class K { static f(): i64 { return 1; } } function main() { const k = new K(); k.f(); }",
    );
    assert!(r.contains("static method"), "{r}");
}

#[test]
fn default_parameters_are_filled_in() {
    let p = ok_src("function f(a: i64, b: i64 = 7): i64 { return a + b; } function main() { console.log(f(1), [\"a\"].join()); }");
    let f = def_id(&p, "f");
    let (_, args) = calls(func(&p, "main"))
        .into_iter()
        .find(|(c, _)| matches!(c, Callee::Def(d, _) if *d == f))
        .unwrap();
    assert_eq!(args.len(), 2);
    let r =
        err_src("function f(a: i64, b: i64 = 7): i64 { return a + b; } function main() { f(); }");
    assert!(
        r.contains("takes 1 to 2 arguments but 0 arguments were supplied"),
        "{r}"
    );
}

#[test]
fn recursive_value_types_and_generic_overrides() {
    let r = err_src(
        "struct Cons { kind: \"cons\"; head: i64; tail: Cons | Nil; } struct Nil { kind: \"nil\"; } function main() {}",
    );
    assert!(r.contains("recursive type `Cons` has infinite size"), "{r}");
    let r = err_src("struct S { next: S | null; } function main() {}");
    assert!(r.contains("recursive type `S` has infinite size"), "{r}");
    ok_src(
        "class N { next: N | null = null; } struct Leaf { kind: \"leaf\"; } struct Node { kind: \"node\"; kids: (Leaf | Node)[]; } function main() {}",
    );
    let r = err_src(
        "class A { f<T>(x: T): i64 { return 1; } } class B extends A { override f<T>(x: T): i64 { return 2; } } function main() { const a: A = new B(); a.f(1); }",
    );
    assert!(r.contains("would need dynamic dispatch"), "{r}");
}

#[test]
fn printing_rejects_nested_function_values() {
    let r = err_src("struct H { f: (x: i64) => i64; } function main() { const h = H { f: (x: i64) => x }; console.log(h); }");
    assert!(r.contains("cannot print a value of type `H`"), "{r}");
}

#[test]
fn interface_fields_through_values_and_generics() {
    let p = ok_src(
        "interface Aged { age: i64; }
         class P implements Aged { age: i64 = 3; }
         function viaDyn(a: Aged): i64 { return a.age; }
         function viaGeneric<T extends Aged>(a: T): i64 { return a.age; }
         function main() { console.log(viaDyn(new P()), viaGeneric(new P())); }",
    );
    assert!(calls(func(&p, "viaDyn"))
        .iter()
        .any(|(c, _)| matches!(c, Callee::Dyn { slot: 0 })));
    assert!(calls(func(&p, "viaGeneric"))
        .iter()
        .any(|(c, _)| matches!(c, Callee::ParamMethod { slot: 0, .. })));
    let r = err_src(
        "interface Aged { age: i64; } function f<T extends Aged>(a: T) { a.age = 1; } function main() {}",
    );
    assert!(
        r.contains("cannot modify `age` through an interface or generic value"),
        "{r}"
    );
}

#[test]
fn literal_cases_on_nullable_values_match_the_payload() {
    let p = ok_src(
        "function f(x: i64 | null): i64 { switch (x) { case null: return 0; case 1: case 2: return 1; default: return 3; } } function main() {}",
    );
    let pats = common::hir_walk::pats(func(&p, "f"));
    let somes = pats
        .iter()
        .filter(|x| matches!(x.kind, velt_sema::hir::PatKind::Some(_)))
        .count();
    assert_eq!(somes, 2);
    let r = err_src("function f(x: i64): i64 { switch (x) { case null: return 1; } return 0; } function main() {}");
    assert!(r.contains("never null"), "{r}");
}

#[test]
fn dispose_is_a_drop_hook() {
    let p = ok_src(
        "struct Handle { fd: i64; [Symbol.dispose]() { console.log(\"close\", this.fd); } }
         class Conn { h: Handle = Handle { fd: 1 }; [Symbol.dispose]() {} }
         class Tls extends Conn {}
         function main() { const h = Handle { fd: 3 }; const c = new Tls(); }",
    );
    let h = adt(&p, "Handle");
    assert!(!h.is_copy, "types with dispose are never Copy");
    assert_eq!(h.dispose, Some(def_id(&p, "Handle.[Symbol.dispose]")));
    assert_eq!(
        func(&p, "Handle.[Symbol.dispose]").params[0].mode,
        PassMode::BorrowMut
    );
    assert_eq!(
        adt(&p, "Tls").dispose,
        Some(def_id(&p, "Conn.[Symbol.dispose]")),
        "inherited"
    );
    // An explicit call drops the value there: it is moved, so a later use is an error.
    ok_src("struct H { fd: i64; [Symbol.dispose]() {} } function main() { let h = H { fd: 1 }; h[Symbol.dispose](); }");
    let r = err_src("struct H { fd: i64; [Symbol.dispose]() {} } function main() { const h = H { fd: 1 }; h[Symbol.dispose](); console.log(h.fd); }");
    assert!(r.contains("use of moved value `h`"), "{r}");
    // A method merely named `dispose` is an ordinary method, not a drop hook.
    let p = ok_src("class D { dispose() {} } function main() { const d = new D(); d.dispose(); }");
    assert_eq!(adt(&p, "D").dispose, None);
    let r = err_src("struct H { fd: i64; [Symbol.dispose](x: i64) {} } function main() {}");
    assert!(
        r.contains("`[Symbol.dispose]` must not be `async`, take no parameters and return `void`"),
        "{r}"
    );
    // Semantics stage 2: a value with `dispose` is shared (disposed at the last reference).
    ok_src("struct H { fd: i64; [Symbol.dispose]() {} } function main() { const a = H { fd: 1 }; const b = a; console.log(a.fd, b.fd); }");
}

#[test]
fn array_literal_takes_the_common_base_class() {
    common::programs::ok_src(
        "class A { n: i64 = 1; } class B extends A { m: i64 = 2; }
         function main() { const xs = [new B(), new A()]; const a: A[] = xs; console.log(a.length); }",
    );
}

#[test]
fn reduce_accumulator_follows_the_elements() {
    common::programs::ok_src(
        "function main() { const lens = [\"ab\"].map((w) => w.length);
           const n: usize = lens.reduce((a, b) => a + b, 0); console.log(n); }",
    );
}

#[test]
fn assignment_expressions_have_the_assigned_value() {
    common::programs::ok_src(
        "function next(): string | null { return null; }
         function main() { let line: string | null = null; let n: usize = 0;
           while ((line = next()) != null) { n += line.length; }
           let x = 0; const y = (x = 5) + 1; console.log(n, x, y); }",
    );
}

#[test]
fn literal_unions_use_their_base_members() {
    common::programs::ok_src(
        "type Lvl = \"lo\" | \"mid\";
         function main() { const l: Lvl = \"mid\"; const n: usize = l.length;
           const ls: Lvl[] = [l]; console.log(n, l.toUpperCase(), ls.join(\",\")); }",
    );
}

#[test]
fn bitwise_operators_convert_floats_like_to_int32() {
    common::programs::ok_src(
        "function main() { const a = 41; const q = (a / 13) | 0; const n: i64 = q + 1; console.log(n); }",
    );
}

#[test]
fn generic_alias_shows_its_parameter_names() {
    let r = err_src(
        r#"type Outcome<T, E> = { status: "ok"; value: T } | { status: "failed"; error: E };
function describe<T, E>(o: Outcome<T, E>): string { return o.missing; }
function main() { }"#,
    );
    assert!(
        r.contains(r#"{ status: "ok"; value: T } | { status: "failed"; error: E }"#),
        "{r}"
    );
    let r = err_src(
        r#"type Outcome<T, E> = { status: "ok"; value: T } | { status: "failed"; error: E };
function describe<A, B>(o: Outcome<B, A>): string { return o.missing; }
function main() { }"#,
    );
    assert!(
        r.contains(r#"{ status: "ok"; value: B } | { status: "failed"; error: A }"#),
        "{r}"
    );
}
