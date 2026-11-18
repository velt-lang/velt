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
    let TyKind::Option(inner, _) = p.types.kind(t[0]) else {
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
        matches!(p.types.kind(fs[0].ty), TyKind::Option(..)),
        "a keeps its written null"
    );
    assert!(matches!(p.types.kind(fs[1].ty), TyKind::Float(_)));
    assert!(matches!(p.types.kind(fs[2].ty), TyKind::Option(..)));
}

#[test]
fn optional_fields_of_object_types_keep_presence() {
    let (p, t) = params(
        "type U = { a?: string | null; b?: string; c: string | null };
         function f(x: U, y: Partial<U>) {}",
    );
    let presence: Vec<bool> = fields(&p, t[0]).iter().map(|f| f.presence).collect();
    assert_eq!(presence, [true, true, false]);
    let presence: Vec<bool> = fields(&p, t[1]).iter().map(|f| f.presence).collect();
    assert_eq!(
        presence,
        [true, true, true],
        "Partial makes `c` optional over `string | null`"
    );
    let p =
        ok_src("class C { a?: string | null; b = 1 } function main() { console.log(new C().b); }");
    assert!(adt(&p, "C").fields.iter().all(|f| !f.presence));
}

#[test]
fn optional_and_nullable_fields_are_different_types() {
    let (_, t) = params(
        "function f(a: { a?: string }, b: { a: string | undefined }, c: { a?: string | null }) {}",
    );
    assert_ne!(t[0], t[1]);
    assert_ne!(t[0], t[2]);
    assert_ne!(t[1], t[2]);
    let msg = err_src(
        "function f(y: { a: string | undefined }) { const x: { a?: string } = y; console.log(x); }
         function main() {}",
    );
    assert!(
        msg.contains("an optional field (`a?: T`) may be absent"),
        "{msg}"
    );
    assert!(!msg.contains("in another order"), "{msg}");
}

/// Every substitution outside `anon.rs` goes through `Ctx::subst`/`subst_known`, which keep
/// types canonical; a raw `cx.ty.subst(…)` would leave a generic instance where the written type
/// is expected (design R3).
#[test]
fn substitution_goes_through_the_canonicalizing_ctx_methods() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![src];
    let mut raw = vec![];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.ends_with(".rs") || name == "anon.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source");
            for (i, line) in text.lines().enumerate() {
                if line.contains(".ty.subst(") || line.contains(".ty.subst_known(") {
                    raw.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        raw.is_empty(),
        "use `cx.subst` / `cx.subst_known`:
{}",
        raw.join(
            "
"
        )
    );
}

/// An optional field keeps an absent key apart from one holding `undefined` (#930 phase 2), so
/// `in` on it is fine wherever a value that may be `undefined` is stored, also under a key that
/// is not an identifier (this was an error while an optional field was `T | null`).
#[test]
fn in_on_an_optional_field_that_may_hold_undefined_is_accepted() {
    ok_src(
        "type B = { kind: string; \"a b\"?: string };
         function mk(v?: string): B { return { kind: \"k\", \"a b\": v }; }
         function set(o: B, v?: string) { o[\"a b\"] = v; }
         function main() { const o = mk(); set(o); console.log(\"a b\" in o); }",
    );
}

/// The members of the union type `t`, as its variants hold them.
fn union_variants(p: &Program, t: TyId) -> Vec<String> {
    match p.types.kind(t) {
        TyKind::Adt(d, _) => match p.def(*d) {
            Def::Enum(e) if e.is_union => e.variants.iter().map(|v| v.name.clone()).collect(),
            other => panic!("not a union: {other:?}"),
        },
        other => panic!("not a union: {other:?}"),
    }
}

/// A union's variants are in an order that is the same in every program (#2943): it doesn't
/// follow which members the program happened to make first. Here the second program makes `"b"`,
/// `C` and `i64[]` long before it writes the union, the first makes them in the union.
#[test]
fn a_unions_variant_order_does_not_depend_on_what_the_program_made_first() {
    let union = "class C { x = 1; }
                 function f(u: \"b\" | C | i64[] | \"a\" | string | boolean) {}";
    let (p1, t1) = params(union);
    let (p2, t2) = params(&format!(
        "function early(): i64[] {{ const xs: i64[] = []; const s: \"b\" = \"b\"; console.log(s, new C().x); return xs; }}
         {union}"
    ));
    let order = union_variants(&p1, t1[0]);
    assert_eq!(order, union_variants(&p2, t2[0]));
    // The built-in types first, as `Types::new` makes them (`boolean | string`, as before), then
    // literals by their spelling (strings quoted, numbers as written: `10` sorts before `9`).
    assert_eq!(&order[..2], ["boolean", "string"], "{order:?}");
    assert_eq!(&order[2..4], ["\"a\"", "\"b\""], "{order:?}");
}

/// A mapped type over `keyof X` has `X`'s fields in `X`'s order, as in TypeScript, whatever the
/// order of the key union's members (#2943).
#[test]
fn a_mapped_type_keeps_the_source_field_order() {
    let (p, t) = params(
        "interface X { zeta: string; alpha: i64; mid: boolean }
         function f(m: { [K in keyof X]: string }) {}",
    );
    let names: Vec<String> = fields(&p, t[0]).into_iter().map(|f| f.name).collect();
    assert_eq!(names, ["zeta", "alpha", "mid"]);
}
