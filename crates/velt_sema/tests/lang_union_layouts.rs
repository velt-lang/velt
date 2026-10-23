//! Unions and object types that erasure (`readonly`, field-only interfaces) makes alike are one
//! layout for lowering: the variants of one erased payload are one variant, and object types
//! whose fields erase alike are one type (#1447).

mod common;

use common::hir_walk::{exprs, func, pats};
use velt_sema::hir::{Def, DefId, ExprKind as E, PatKind, Program, TyId, TyKind};

const DECLS: &str = "interface A { a: number }\n\
    type R = { readonly a: number };\n\
    type AR = (R | number)[] | (number | A)[] | string;\n\
    type OA = { v: number | A };\n\
    type OR = { v: number | R };\n";

fn program(body: &str) -> Program {
    common::programs::ok_src(&format!("{DECLS}{body}\nfunction main() {{}}"))
}

/// The type of `f`'s first parameter.
fn param(p: &Program, f: &str) -> TyId {
    func(p, f).params[0].ty
}

/// The union def of `t`.
fn union_def(p: &Program, t: TyId) -> DefId {
    match p.types.kind(t) {
        TyKind::Adt(d, _) if matches!(p.def(*d), Def::Enum(e) if e.is_union) => *d,
        other => panic!("not a union: {other:?}"),
    }
}

/// Every variant number of `def` that `f` builds, unwraps or matches.
fn variants_used(p: &Program, f: &str, def: DefId) -> Vec<u32> {
    let fd = func(p, f);
    let mut out = vec![];
    for e in exprs(fd) {
        match &e.kind {
            E::Variant { def: d, variant, .. } if *d == def => out.push(*variant),
            E::UnwrapVariant { expr, variant, .. }
                if matches!(p.types.kind(expr.ty), TyKind::Adt(d, _) if *d == def) =>
            {
                out.push(*variant)
            }
            _ => {}
        }
    }
    for q in pats(fd) {
        if let PatKind::Variant { def: d, variant, .. } = &q.kind {
            if *d == def {
                out.push(*variant);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[test]
fn variants_of_one_erased_payload_are_the_first_of_them() {
    let p = program(
        "function w(x: AR): string {\n\
           if (typeof x === \"string\") return x;\n\
           const y = x as (R | number)[];\n\
           return `${y.length}`;\n\
         }\n\
         function build(): AR[] {\n\
           const a: (number | A)[] = [1];\n\
           const r: (R | number)[] = [2];\n\
           return [a, r, \"s\"];\n\
         }",
    );
    let u = union_def(&p, param(&p, "w"));
    let Def::Enum(e) = p.def(u) else {
        unreachable!()
    };
    let payloads: Vec<TyId> = e.variants.iter().map(|v| v.payload[0]).collect();
    assert_eq!(payloads.len(), 3);
    let first = |t: TyId| payloads.iter().position(|q| *q == t).unwrap() as u32;
    let firsts: Vec<u32> = payloads.iter().map(|t| first(*t)).collect();
    assert_eq!(
        (firsts.iter().enumerate()).filter(|(i, f)| *i as u32 != **f).count(),
        1,
        "the two array members erase alike: {payloads:?}"
    );
    for f in ["w", "build"] {
        let used = variants_used(&p, f, u);
        assert!(!used.is_empty(), "{f} uses the union");
        assert!(
            used.iter().all(|&v| firsts[v as usize] == v),
            "{f} uses variants {used:?} of payloads {payloads:?}"
        );
    }
}

#[test]
fn object_types_whose_fields_erase_alike_are_one_type() {
    let p = program(
        "function fa(x: OA): number { return typeof x.v === \"number\" ? x.v : 0; }\n\
         function fr(x: OR): number { return typeof x.v === \"number\" ? x.v : 0; }",
    );
    assert_eq!(param(&p, "fa"), param(&p, "fr"));
}

#[test]
fn a_union_of_alike_object_types_is_that_type() {
    let p = program(
        "function f(x: OA | OR): string { return JSON.stringify(x); }\n\
         function fa(x: OA): number { return typeof x.v === \"number\" ? x.v : 0; }",
    );
    assert_eq!(param(&p, "f"), param(&p, "fa"));
}

#[test]
fn a_union_of_alike_objects_and_another_member_has_one_object_variant() {
    let p = program(
        "function f(x: OA | OR | string): string {\n\
           if (typeof x === \"string\") return x;\n\
           const o = x as OR;\n\
           return JSON.stringify(o);\n\
         }\n\
         function build(a: OA, r: OR): (OA | OR | string)[] { return [a, r, \"s\"]; }",
    );
    let u = union_def(&p, param(&p, "f"));
    let Def::Enum(e) = p.def(u) else {
        unreachable!()
    };
    let payloads: Vec<TyId> = e.variants.iter().map(|v| v.payload[0]).collect();
    let fa = param(&p, "build");
    let objects: Vec<u32> = (0..payloads.len() as u32)
        .filter(|&i| payloads[i as usize] == fa)
        .collect();
    assert!(!objects.is_empty(), "an object variant: {payloads:?}");
    for f in ["f", "build"] {
        let used = variants_used(&p, f, u);
        let obj: Vec<u32> = used.into_iter().filter(|v| objects.contains(v)).collect();
        assert_eq!(obj, vec![objects[0]], "{f}: one tag for the object members");
    }
}
