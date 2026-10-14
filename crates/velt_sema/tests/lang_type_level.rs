//! Conditional types with `infer`, mapped types and template literal types (#934), evaluated
//! where their type arguments are known: each use reduces to an ordinary object type, union or
//! literal type, as TypeScript evaluates it. A form whose argument is still a type parameter is
//! an error, never an approximation.

mod common;

use common::hir_walk::func;
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Def, LitValue, Nothing, Program, TyId, TyKind};

/// `t` for assertions: primitives by name, nothings, literals, unions (sorted) and object types
/// by fields (`?` and `readonly` shown).
fn show(p: &Program, t: TyId) -> String {
    match p.types.kind(t) {
        TyKind::Str => "string".into(),
        TyKind::Float(_) => "number".into(),
        TyKind::Bool => "boolean".into(),
        TyKind::Never => "never".into(),
        TyKind::Option(x, n) => format!(
            "{}{}",
            show(p, *x),
            match n {
                Nothing::Null => " | null",
                Nothing::Undefined => " | undefined",
                Nothing::Both => " | null | undefined",
            }
        ),
        TyKind::Literal(LitValue::Str(s)) => format!("{s:?}"),
        TyKind::Literal(v) => format!("{v:?}"),
        TyKind::FnPtr { ret, .. } => format!("() => {}", show(p, *ret)),
        TyKind::Adt(d, _) => match p.def(*d) {
            Def::Adt(a) => {
                let fs: Vec<String> = a
                    .fields
                    .iter()
                    .map(|f| {
                        let ro = if f.readonly { "readonly " } else { "" };
                        let opt = if f.optional { "?" } else { "" };
                        let ty = match (f.optional, p.types.kind(f.ty)) {
                            (true, TyKind::Option(x, _)) => *x,
                            _ => f.ty,
                        };
                        format!("{ro}{}{opt}: {}", f.name, show(p, ty))
                    })
                    .collect();
                format!("{{ {} }}", fs.join("; "))
            }
            Def::Enum(e) if e.is_union => {
                let mut ms: Vec<String> = e
                    .variants
                    .iter()
                    .map(|v| show(p, v.payload[0]))
                    .collect();
                ms.sort();
                ms.join(" | ")
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
fn conditional_types_distribute_over_a_naked_parameter() {
    let decls = "type IsStr<T> = T extends string ? \"y\" : \"n\";";
    assert_eq!(param(&format!("{decls} function f(x: IsStr<string>) {{}}")), "\"y\"");
    assert_eq!(param(&format!("{decls} function f(x: IsStr<number>) {{}}")), "\"n\"");
    assert_eq!(
        param(&format!("{decls} function f(x: IsStr<\"a\" | 1>) {{}}")),
        "\"n\" | \"y\""
    );
    // `[T]` is not naked: the union is tested as a whole.
    assert_eq!(
        param("type W<T> = [T] extends [string] ? \"y\" : \"n\"; function f(x: W<\"a\" | 1>) {}"),
        "\"n\""
    );
    // `Exclude` and `NonNullable` filter members, `undefined` included.
    assert_eq!(
        param("function f(x: Exclude<\"a\" | \"b\" | \"c\", \"a\" | \"c\">) {}"),
        "\"b\""
    );
    assert_eq!(param("function f(x: NonNullable<string | undefined>) {}"), "string");
}

#[test]
fn infer_binds_through_objects_generics_and_templates() {
    assert_eq!(
        param("type U<T> = T extends { v?: infer M } ? M : never; function f(x: U<{ v?: number }>) {}"),
        "number"
    );
    assert_eq!(
        param("type B<T> = { b: T }; type U<T> = T extends B<infer V> | undefined ? V : 0; function f(x: U<B<string>>) {}"),
        "string"
    );
    assert_eq!(
        param("type C<S> = S extends `${infer F}${infer R}` ? `${Uppercase<F>}${R}` : S; function f(x: C<\"click\">) {}"),
        "\"Click\""
    );
    assert_eq!(
        param("type Id<S> = S extends `id-${infer N}` ? N : never; function f(x: Id<\"id-7\" | \"x\">) {}"),
        "\"7\""
    );
}

#[test]
fn homomorphic_mapped_types_keep_and_change_modifiers() {
    let user = "type User = { readonly id: string; name: string; age?: number };";
    assert_eq!(
        param(&format!("{user} type M<T> = {{ [K in keyof T]: T[K] }}; function f(x: M<User>) {{}}")),
        "{ readonly id: string; name: string; age?: number }"
    );
    assert_eq!(
        param(&format!("{user} type M<T> = {{ -readonly [K in keyof T]-?: T[K] }}; function f(x: M<User>) {{}}")),
        "{ id: string; name: string; age: number }"
    );
    assert_eq!(
        param(&format!("{user} type K<T> = {{ [K in keyof T]: undefined extends T[K] ? K : never }}[keyof T]; function f(x: K<User>) {{}}")),
        "\"age\""
    );
    assert_eq!(
        param(&format!("{user} type G<T> = {{ [K in keyof T as K extends \"id\" ? never : `get${{Capitalize<string & K>}}`]: () => T[K] }}; function f(x: G<User>) {{}}")),
        "{ getName: () => string; getAge?: () => number | undefined }"
    );
}

#[test]
fn mapped_types_over_literal_unions() {
    assert_eq!(
        param("type P<N extends string, T> = { [K in N]?: T }; function f(x: P<\"a\" | \"b\", number>) {}"),
        "{ a?: number; b?: number }"
    );
    assert_eq!(
        param("type E = \"click\" | \"focus\"; function f(x: { [K in E as `on${Capitalize<K>}`]: boolean }) {}"),
        "{ onClick: boolean; onFocus: boolean }"
    );
}

#[test]
fn forms_over_type_parameters_are_errors() {
    let e = err_src("type IsStr<T> = T extends string ? 1 : 2;\nfunction g<T>(x: IsStr<T>) {}\nfunction main() {}");
    assert!(
        e.contains("a conditional type over a type parameter is not supported yet"),
        "{e}"
    );
    let e = err_src("class Box<T> { v: { [K in keyof T]: T[K] } | null = null; }\nfunction main() {}");
    assert!(e.contains("a mapped type over a type parameter is not supported yet"), "{e}");
    // Declaring such an alias without using it is fine.
    ok_src("type IsStr<T> = T extends string ? 1 : 2;\ntype M<T> = { [K in keyof T]: T[K] };\nfunction main() {}");
    let e = err_src("type Bad = infer X;\nfunction main() {}");
    assert!(e.contains("only permitted in the `extends` clause"), "{e}");
}
