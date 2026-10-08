//! Canonical instances (docs/internals/design/deferred-types.md, P1): an instance of a generic
//! object type, union or nullable type is the type written directly. Optional fields (P2): `?`
//! is a flag over the declared type, part of an object type's identity, and `a?: T | null` keeps
//! an absent key apart from a present `null` (`FieldDef::presence`).

mod common;

use common::hir_walk::{adt, func};
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Def, FieldDef, Program, TyId, TyKind};

/// The types of `f`'s parameters in `src` (which declares `function f(…)`).
fn params(src: &str) -> (Program, Vec<TyId>) {
    let p = ok_src(&format!("{src}\nfunction main() {{}}"));
    let tys = func(&p, "f").params.iter().map(|x| x.ty).collect();
    (p, tys)
}

/// The fields of object type `t`.
fn fields(p: &Program, t: TyId) -> Vec<FieldDef> {
    match p.types.kind(t) {
        TyKind::Adt(d, _) => match p.def(*d) {
            Def::Adt(a) => a.fields.clone(),
            other => panic!("not an object type: {other:?}"),
        },
        other => panic!("not an object type: {other:?}"),
    }
}

#[test]
fn a_generic_object_types_instance_is_the_written_type() {
    let (_, t) = params("type Box<T> = { v: T }; function f(a: Box<string>, b: { v: string }) {}");
    assert_eq!(t[0], t[1]);
    // A generic function's result, and a generic field-only interface's instance, are values of
    // the written object type.
    ok_src(
        "interface Pair<A, B> { first: A; second: B }
         function wrap<U>(x: U): { a: U } { return { a: x }; }
         function main() {
           const o: { a: string } = wrap<string>(\"hi\");
           const p: Pair<string, f64> = { first: o.a, second: 1.5 };
           const q: { first: string; second: f64 } = p;
           console.log(q.first);
         }",
    );
}

#[test]
fn readonly_and_optional_flags_survive_instantiation() {
    let (p, t) = params(
        "type Ro<T> = { readonly v: T; w?: T };
         function f(a: Ro<string>, b: { readonly v: string; w?: string }) {}",
    );
    assert_eq!(t[0], t[1]);
    assert!(fields(&p, t[0])[1].optional);
    let msg = err_src(
        "type Ro<T> = { readonly v: T };
         function f(a: Ro<string>) { a.v = \"x\"; }
         function main() {}",
    );
    assert!(msg.contains("readonly"), "{msg}");
}

#[test]
fn a_generic_unions_instance_is_the_written_union() {
    let (_, t) = params(
        "type Or<A> = A | string;
         type Both<A, B> = A | B;
         function f(a: Or<i64>, b: string | i64, c: Both<string, string>, d: string) {}",
    );
    assert_eq!(t[0], t[1]);
    assert_eq!(t[2], t[3]);
}

#[test]
fn a_nullable_instance_of_a_nullable_parameter_has_one_null() {
    let (p, t) =
        params("type N<T> = T | null; function f(a: N<string | null>, b: string | null) {}");
    assert_eq!(t[0], t[1]);
    let TyKind::Option(inner) = p.types.kind(t[0]) else {
        panic!("not nullable");
    };
    assert!(matches!(p.types.kind(*inner), TyKind::Str));
}

#[test]
fn required_clears_only_the_optional_flag() {
    let (p, t) = params(
        "type U = { a?: string | null; b?: f64; c: string | null };
         function f(x: Required<U>) {}",
    );
    let fs = fields(&p, t[0]);
    assert!(fs.iter().all(|f| !f.optional));
    assert!(
        matches!(p.types.kind(fs[0].ty), TyKind::Option(_)),
        "a keeps its written null"
    );
    assert!(matches!(p.types.kind(fs[1].ty), TyKind::Float(_)));
    assert!(matches!(p.types.kind(fs[2].ty), TyKind::Option(_)));
}

#[test]
fn only_nullable_optional_fields_of_object_types_keep_presence() {
    let (p, t) = params(
        "type U = { a?: string | null; b?: string; c: string | null };
         function f(x: U, y: Partial<U>) {}",
    );
    let presence: Vec<bool> = fields(&p, t[0]).iter().map(|f| f.presence).collect();
    assert_eq!(presence, [true, false, false]);
    let presence: Vec<bool> = fields(&p, t[1]).iter().map(|f| f.presence).collect();
    assert_eq!(
        presence,
        [true, false, true],
        "Partial makes `c` optional over `string | null`"
    );
    let p =
        ok_src("class C { a?: string | null; b = 1 } function main() { console.log(new C().b); }");
    assert!(adt(&p, "C").fields.iter().all(|f| !f.presence));
}

#[test]
fn optional_and_nullable_fields_are_different_types() {
    let (_, t) = params(
        "function f(a: { a?: string }, b: { a: string | null }, c: { a?: string | null }) {}",
    );
    assert_ne!(t[0], t[1]);
    assert_ne!(t[0], t[2]);
    assert_ne!(t[1], t[2]);
    let msg = err_src(
        "function f(y: { a: string | null }) { const x: { a?: string } = y; console.log(x); }
         function main() {}",
    );
    assert!(
        msg.contains("an optional field (`a?: T`) may be absent"),
        "{msg}"
    );
    assert!(!msg.contains("in another order"), "{msg}");
}
