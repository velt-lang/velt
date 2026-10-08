//! Intersection types (`A & B`, #384): each reduces to one anonymous object type with the
//! parts' fields in TypeScript's order, shared fields intersected; unions distribute; parts
//! that can't be combined, and combinations no value has, are errors with a fix. A primitive
//! `&` an object type is a branded type, erased to the primitive before lowering.

mod common;

use common::hir_walk::func;
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Def, Program, TyId, TyKind};

/// `t` for assertions: primitives by name, `T | null`, literals and object types by fields.
fn show(p: &Program, t: TyId) -> String {
    match p.types.kind(t) {
        TyKind::Str => "string".into(),
        TyKind::Float(_) => "f64".into(),
        TyKind::Int(_) => "int".into(),
        TyKind::Bool => "boolean".into(),
        TyKind::Option(x) => format!("{} | null", show(p, *x)),
        TyKind::Literal(v) => format!("{v:?}"),
        TyKind::Adt(d, _) => match p.def(*d) {
            Def::Adt(a) => {
                let fs: Vec<String> = a
                    .fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name, show(p, f.ty)))
                    .collect();
                format!("{{ {} }}", fs.join("; "))
            }
            other => format!("{other:?}"),
        },
        other => format!("{other:?}"),
    }
}

/// The type of `f`'s first parameter in `src` (which declares `function f(x: …)`).
fn param(src: &str) -> String {
    let p = ok_src(&format!("{src}\nfunction main() {{}}"));
    let f = func(&p, "f");
    show(&p, f.params[0].ty)
}

#[test]
fn fields_are_the_first_parts_then_the_new_ones() {
    assert_eq!(
        param("type A = { a: string; s: f64 }; type B = { b: boolean; s: f64 }; function f(x: A & B) {}"),
        "{ a: string; s: f64; b: boolean }"
    );
    assert_eq!(
        param("interface I { id: string } function f(x: { n: f64 } & I & { z: boolean }) {}"),
        "{ n: f64; id: string; z: boolean }"
    );
}

#[test]
fn a_field_is_required_when_any_part_requires_it() {
    assert_eq!(
        param("function f(x: { a?: string; b?: f64 } & { a: string }) {}"),
        "{ a: string; b: f64 | null }"
    );
    assert_eq!(
        param("function f(x: { a?: string } & { a?: string }) {}"),
        "{ a: string | null }"
    );
}

#[test]
fn shared_object_fields_merge_and_literals_meet_their_base() {
    assert_eq!(
        param("function f(x: { p: { x: f64 } } & { p: { y: f64 } }) {}"),
        "{ p: { x: f64; y: f64 } }"
    );
    assert!(
        param("function f(x: { k: string } & { k: \"on\" }) {}").contains("Str(\"on\")"),
        "a literal & its base type is the literal"
    );
}

#[test]
fn unions_distribute_and_impossible_members_drop() {
    let src = "type C = { kind: \"c\"; r: f64 }; type S = { kind: \"s\"; w: f64 };
        function f(x: (C | S) & { kind: \"c\" }) {}";
    assert!(param(src).contains("r: f64"), "{}", param(src));
    // `null & B` has no value, so `(A | null) & B` is `A & B`.
    assert_eq!(
        param("function f(x: ({ a: f64 } | null) & { b: f64 }) {}"),
        "{ a: f64; b: f64 }"
    );
}

#[test]
fn intersections_in_generic_aliases_are_resolved_per_use() {
    assert_eq!(
        param("type WithId<T> = T & { id: string }; function f(x: WithId<{ n: f64 }>) {}"),
        "{ n: f64; id: string }"
    );
}

#[test]
fn readonly_stays_only_when_every_part_says_so() {
    ok_src(
        "type R = { readonly a: string } & { a: string };
         function main() { const r: R = { a: \"x\" }; r.a = \"y\"; console.log(r.a); }",
    );
    let e = err_src(
        "type R = { readonly a: string } & { readonly a: string; b: f64 };
         function main() { const r: R = { a: \"x\", b: 1 }; r.a = \"y\"; }",
    );
    assert!(e.contains("readonly"), "{e}");
}

#[test]
fn conflicting_fields_are_an_error_not_never() {
    let e = err_src("type A = { k: string } & { k: f64 }; function main() {}");
    assert!(
        e.contains("no value has type")
            && e.contains("field `k` is `string` in one part and `f64` in another"),
        "{e}"
    );
    let e = err_src("type A = { kind: \"a\" } & { kind: \"b\" }; function main() {}");
    assert!(e.contains("field `kind`"), "{e}");
}

#[test]
fn parts_that_are_not_object_types_are_errors_with_a_fix() {
    for (src, needle) in [
        (
            "class K { x = 1 } type A = K & { y: f64 };",
            "`K` is a class",
        ),
        ("type A = f64[] & { y: f64 };", "is an array"),
        (
            "type A = (() => void) & (() => void);",
            "function types (overloads)",
        ),
        (
            "interface S { show(): string } type A = S & { y: f64 };",
            "interface with methods",
        ),
        (
            "function g<T>(x: T & { y: f64 }) {}",
            "`T` is a type parameter",
        ),
    ] {
        let e = err_src(&format!("{src} function main() {{}}"));
        assert!(e.contains(needle), "{src}: {e}");
    }
}

#[test]
fn a_wider_object_does_not_convert_and_the_note_offers_the_copy() {
    let e = err_src(
        "type A = { a: f64 }; type AB = A & { b: f64 };
         function g(a: A): f64 { return a.a; }
         function main() { const ab: AB = { a: 1, b: 2 }; g(ab); }",
    );
    assert!(e.contains("`AB` has fields `A` does not (`b`)"), "{e}");
    assert!(e.contains("{ ...ab }"), "{e}");
    ok_src(
        "type A = { a: f64 }; type AB = A & { b: f64 };
         function g(a: A): f64 { return a.a; }
         function main() { const ab: AB = { a: 1, b: 2 }; console.log(g({ ...ab })); }",
    );
}

#[test]
fn literals_are_checked_against_the_alias_by_name() {
    let e =
        err_src("type AB = { a: f64 } & { b: f64 }; function main() { const x: AB = { a: 1 }; }");
    assert!(e.contains("missing field `b` in `AB` literal"), "{e}");
}

#[test]
fn indexed_access_reads_field_types() {
    assert_eq!(
        param("type P = { name: string; age: f64 }; function f(x: P[\"name\"]) {}"),
        "string"
    );
    let e = err_src("type P = { name: string }; type X = P[\"nope\"]; function main() {}");
    assert!(e.contains("has no field `nope`"), "{e}");
}

#[test]
fn brands_are_their_primitive_after_checking() {
    let p = ok_src(
        "type UserId = string & { __brand: \"UserId\" };
         function f(x: UserId): f64 { return x.length; }
         function main() { console.log(f(\"u\" as UserId)); }",
    );
    assert_eq!(show(&p, func(&p, "f").params[0].ty), "string");
}

#[test]
fn a_plain_primitive_or_another_brand_is_not_a_brand() {
    let e = err_src(
        "type UserId = string & { __brand: \"U\" }; function main() { const u: UserId = \"x\"; }",
    );
    assert!(e.contains("brand a value with `x as UserId`"), "{e}");
    let e = err_src(
        "type A = string & { __brand: \"A\" }; type B = string & { __brand: \"B\" };
         function main() { const a = \"x\" as A; const b: B = a; }",
    );
    assert!(e.contains("`A`, another brand,"), "{e}");
    let e =
        err_src("type Id = string & { __brand: \"Id\" }; function main() { const i = 5 as Id; }");
    assert!(e.contains("only a `string` can be branded"), "{e}");
}

#[test]
fn interfaces_do_not_merge_and_the_note_says_so() {
    let e = err_src("interface X { a: f64 } interface X { b: f64 } function main() {}");
    assert!(
        e.contains("Velt does not merge interface declarations"),
        "{e}"
    );
}

#[test]
fn reordered_parts_are_another_type_and_the_note_says_so() {
    let src = "type Named = { name: string }; type Aged = { age: f64 };
         function show(p: Named & Aged): string { return p.name; }";
    let e = err_src(&format!(
        "{src} function main() {{ const an: Aged & Named = {{ age: 3, name: \"x\" }}; show(an); }}"
    ));
    assert!(e.contains("has the same fields as"), "{e}");
    assert!(e.contains("{ ...an }") && e.contains("#651"), "{e}");
    assert!(!e.contains("does not ()"), "{e}");
    ok_src(&format!(
        "{src} function main() {{ const an: Aged & Named = {{ age: 3, name: \"x\" }};
           console.log(show({{ ...an }})); }}"
    ));
}

#[test]
fn an_alias_cannot_refer_to_itself_through_an_intersection() {
    let e = err_src("type T = { kids: T[] } & { v: f64 }; function main() {}");
    assert!(e.contains("type alias `T` refers to itself"), "{e}");
}
