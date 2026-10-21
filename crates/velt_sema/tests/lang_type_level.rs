//! Conditional types with `infer`, mapped types and template literal types (#934), evaluated
//! where their type arguments are known: each use reduces to an ordinary object type, union or
//! literal type, as TypeScript evaluates it. A form whose argument is still a type parameter is
//! an error, never an approximation.

mod common;

use common::hir_walk::func;
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Def, LitValue, Nothing, Program, TyId, TyKind};

/// `t` for assertions: primitives by name, nothings, literals, unions (sorted) and object types
/// by fields (`?` shown; `readonly` is erased before HIR).
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
        TyKind::Literal(LitValue::Int(_, i)) => i.to_string(),
        TyKind::Literal(LitValue::Float(_, bits)) => f64::from_bits(*bits).to_string(),
        TyKind::Literal(LitValue::Bool(b)) => b.to_string(),
        TyKind::Array(e) => format!("{}[]", show(p, *e)),
        TyKind::FnPtr { ret, .. } => format!("() => {}", show(p, *ret)),
        TyKind::Adt(d, _) => match p.def(*d) {
            Def::Adt(a) => {
                let fs: Vec<String> = a
                    .fields
                    .iter()
                    .map(|f| {
                        let opt = if f.optional { "?" } else { "" };
                        let ty = match (f.optional, p.types.kind(f.ty)) {
                            (true, TyKind::Option(x, _)) => *x,
                            _ => f.ty,
                        };
                        format!("{}{opt}: {}", f.name, show(p, ty))
                    })
                    .collect();
                format!("{{ {} }}", fs.join("; "))
            }
            Def::Enum(e) if e.is_union => {
                let mut ms: Vec<String> =
                    e.variants.iter().map(|v| show(p, v.payload[0])).collect();
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
    assert_eq!(
        param(&format!("{decls} function f(x: IsStr<string>) {{}}")),
        "\"y\""
    );
    assert_eq!(
        param(&format!("{decls} function f(x: IsStr<number>) {{}}")),
        "\"n\""
    );
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
    assert_eq!(
        param("function f(x: NonNullable<string | undefined>) {}"),
        "string"
    );
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
        param(&format!(
            "{user} type M<T> = {{ [K in keyof T]: T[K] }}; function f(x: M<User>) {{}}"
        )),
        "{ id: string; name: string; age?: number }"
    );
    assert_eq!(
        param(&format!("{user} type M<T> = {{ -readonly [K in keyof T]-?: T[K] }}; function f(x: M<User>) {{}}")),
        "{ id: string; name: string; age: number }"
    );
    assert_eq!(
        param(&format!("{user} type K<T> = {{ [K in keyof T]: undefined extends T[K] ? K : never }}[keyof T]; function f(x: K<User>) {{}}")),
        // As in TypeScript: the optional field's `undefined` is a member too.
        "\"age\" | undefined"
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
fn inline_forms_over_type_parameters_are_deferred() {
    // Evaluated per instantiation (`TyOp::Inline`), as TypeScript accepts them.
    ok_src(
        "function g<T>(x: T extends string ? 1 : 2) {}
function main() {}",
    );
    ok_src(
        "class Box<T> { v: { [K in keyof T]: T[K] } | null = null; }
function main() {}",
    );
    // Declaring such an alias without using it is fine.
    ok_src(
        "type IsStr<T> = T extends string ? 1 : 2;
type M<T> = { [K in keyof T]: T[K] };
function main() {}",
    );
    let e = err_src(
        "type Bad = infer X;
function main() {}",
    );
    assert!(e.contains("only permitted in the `extends` clause"), "{e}");
}

/// The type of local `name` of function `f` in `src`.
fn local(src: &str, f: &str, name: &str) -> String {
    let p = ok_src(src);
    let f = func(&p, f);
    let l = f
        .body
        .locals
        .iter()
        .find(|l| l.name == name)
        .expect("local");
    show(&p, l.ty)
}

/// An alias over a type parameter in a generic signature, class or interface is deferred
/// (`TyKind::Op`) and evaluated at each instantiation, to what tsc evaluates it to there.
#[test]
fn aliases_over_type_parameters_are_deferred() {
    let decls = "type IsStr<T> = T extends string ? \"y\" : \"n\";
type RO<T> = { readonly [K in keyof T]: T[K] };
type Opt<T> = { [K in keyof T]: undefined extends T[K] ? K : never }[keyof T];
type Ev<T> = { [K in keyof T as `on${Capitalize<string & K>}`]?: (d: T[K]) => void };
function g<T>(x: T): IsStr<T> { throw new Error(); }
function ro<T>(x: T): RO<T> { throw new Error(); }
function opt<T>(x: T): Opt<T> { throw new Error(); }
function ev<T>(x: T): Ev<T> { throw new Error(); }
interface Ctx<P> { props: RO<P> }
function ctx<P>(p: P): Ctx<P> { throw new Error(); }";
    let at = |init: &str| {
        local(
            &format!(
                "{decls}
function main() {{ const v = {init}; }}"
            ),
            "main",
            "v",
        )
    };
    // tsc: "y", "n", { readonly a: number }, "b" | undefined, { onClick?: (d: number) => void }
    assert_eq!(at("g(\"s\")"), "\"y\"");
    assert_eq!(at("g(1)"), "\"n\"");
    assert_eq!(at("ro({ a: 1 })"), "{ a: number }");
    assert_eq!(
        at("opt<{ a: string; b?: number }>({ a: \"\" })"),
        "\"b\" | undefined"
    );
    // (`show` prints a function type's result only.)
    assert_eq!(at("ev({ click: 1 })"), "{ onClick?: () => Unit }");
    assert_eq!(at("ctx({ a: \"s\" }).props"), "{ a: string }");
    // In the generic signature the type stays deferred; lowering gets its reductions.
    let p = ok_src(&format!(
        "{decls}
function main() {{ g(1); }}"
    ));
    let g = func(&p, "g");
    assert!(
        matches!(p.types.kind(g.ret), TyKind::Op(..)),
        "{:?}",
        p.types.kind(g.ret)
    );
    assert!(
        p.type_ops.values().any(|t| show(&p, *t) == "\"n\""),
        "{:?}",
        p.type_ops
    );
}

/// The type of `f`'s first parameter for each alias in `cases` (`(alias, tsc's type)`), the
/// declarations `decls` in scope; every mismatch is reported together.
fn check_all(decls: &str, cases: &[(&str, &str)]) {
    let mut bad = vec![];
    for (ty, want) in cases {
        let got = param(&format!("{decls}\nfunction f(x: {ty}) {{}}"));
        if got != *want {
            bad.push(format!("{ty}: got {got}, tsc {want}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// The cases below are each compared with tsc 5.9 `--strict` (`checker.typeToString`); unions
// are shown sorted.

#[test]
fn number_literals_extend_number() {
    check_all(
        "type IsNum<T> = T extends number ? 1 : 2;
         type Inner<T> = T extends number ? \"n\" : \"o\";
         type Outer<T> = { [K in keyof T]: Inner<T[K]> }[keyof T];
         type TupNum<T> = T extends number[] ? 1 : 2;",
        &[
            ("IsNum<1>", "1"),
            ("IsNum<1 | 2.5>", "1"),
            ("IsNum<-1>", "1"),
            ("IsNum<\"1\">", "2"),
            ("Exclude<1 | \"a\" | 2.5, number>", "\"a\""),
            ("Extract<1 | \"a\", number>", "1"),
            ("TupNum<[1, 2]>", "1"),
            ("Outer<{ a: 5 }>", "\"n\""),
        ],
    );
}

#[test]
fn boolean_distributes_as_true_or_false() {
    check_all(
        "type IsTrue<T> = T extends true ? 1 : 2;
         type ToArr<T> = T extends any ? T[] : never;",
        &[
            ("IsTrue<boolean>", "1 | 2"),
            ("Exclude<boolean, true>", "false"),
            ("Exclude<boolean | string, \"x\">", "boolean | string"),
            ("ToArr<boolean>", "false[] | true[]"),
        ],
    );
}

#[test]
fn homomorphic_mapped_types_distribute_over_unions() {
    check_all(
        "type Id<T> = { [K in keyof T]: T[K] };
         type Partial2<T> = { [K in keyof T]?: T[K] };",
        &[
            ("Id<{ a: 1 } | { b: 2 }>", "{ a: 1 } | { b: 2 }"),
            ("Partial2<{ a: 1 } | { b: 2 }>", "{ a?: 1 } | { b?: 2 }"),
        ],
    );
    // Not the empty object type: `{}` is not one of the members.
    let e = err_src(
        "type Id<T> = { [K in keyof T]: T[K] };\nfunction main() { const x: Id<{ a: 1 } | { b: 2 }> = {}; }",
    );
    assert!(!e.is_empty());
}

#[test]
fn colliding_as_keys_union_their_values() {
    check_all(
        "type Merge<T> = { [K in keyof T as \"x\"]: T[K] };",
        &[
            (
                "{ [K in \"a\" | \"b\" as \"z\"]: K }",
                "{ z: \"a\" | \"b\" }",
            ),
            ("Merge<{ a: 1; b: \"s\" }>", "{ x: \"s\" | 1 }"),
        ],
    );
}

#[test]
fn infer_in_parameter_positions_intersects() {
    check_all(
        "type BothFn<T> = T extends { a: (x: infer U) => void; b: (x: infer U) => void } ? U : never;
         type Param<T> = T extends (x: infer P) => void ? P : never;",
        &[
            ("BothFn<{ a: (x: string) => void; b: (x: number) => void }>", "never"),
            ("BothFn<{ a: (x: { p: 1 }) => void; b: (x: { q: 2 }) => void }>", "{ p: 1; q: 2 }"),
            ("Param<(x: string) => void>", "string"),
        ],
    );
}

#[test]
fn function_parameters_relate_contravariantly() {
    check_all(
        "type A = (x: \"a\") => void; type S = (x: string) => void;
         type Ext<X, Y> = X extends Y ? 1 : 2;
         type AnyP<X> = X extends (x: any) => void ? 1 : 2;
         type UnknownP<X> = X extends (x: unknown) => void ? 1 : 2;",
        &[
            ("Ext<A, S>", "2"),
            ("Ext<S, A>", "1"),
            ("AnyP<S>", "1"),
            ("UnknownP<S>", "2"),
        ],
    );
}

#[test]
fn optional_fields_relate_as_in_typescript() {
    check_all(
        "type Ext<X, Y> = X extends Y ? 1 : 2;",
        &[
            ("Ext<{ a?: 1 }, { a: 1 | undefined }>", "2"),
            ("Ext<{ a: 1 | undefined }, { a?: 1 }>", "1"),
            ("Ext<{ a?: 1 }, { a?: 1 }>", "1"),
            // A weak type (all fields optional) needs a property in common.
            ("Ext<{ b: 1 }, { a?: 1 }>", "2"),
        ],
    );
}

#[test]
fn template_patterns_without_holes_and_number_holes() {
    check_all(
        "type N<S> = S extends `${number}` ? 1 : 2;
         type Abc<S> = S extends `abc` ? 1 : 2;",
        &[
            ("Abc<\"abc\">", "1"),
            ("Abc<\"abcd\">", "2"),
            ("N<\" 12\">", "1"),
            ("N<\"12 \">", "1"),
            ("N<\"0x1F\">", "1"),
            ("N<\"0b11\">", "1"),
            ("N<\"1e3\">", "1"),
            ("N<\".5\">", "1"),
            ("N<\"+1\">", "1"),
            ("N<\"-0\">", "1"),
            ("N<\"Infinity\">", "2"),
            ("N<\"NaN\">", "2"),
            ("N<\"\">", "2"),
            ("N<\"1_0\">", "2"),
            ("N<\"inf\">", "2"),
        ],
    );
}

#[test]
fn numbers_in_templates_print_as_javascript_does() {
    check_all(
        "",
        &[
            ("`${1e21}`", "\"1e+21\""),
            ("`${1.5e-7}`", "\"1.5e-7\""),
            ("`${1.2345678901234568e20}`", "\"123456789012345680000\""),
            ("`${0.000001}`", "\"0.000001\""),
            ("`${2.5}`", "\"2.5\""),
        ],
    );
}

#[test]
fn numeric_mapped_keys_stay_numbers() {
    check_all("", &[("{ [K in \"a\" | 1]: K }", "{ 1: 1; a: \"a\" }")]);
}

#[test]
fn empty_object_and_record_patterns() {
    check_all(
        "type Ext<X, Y> = X extends Y ? 1 : 2;",
        &[
            ("Ext<\"a\", {}>", "1"),
            ("Ext<string, {}>", "1"),
            ("Ext<1, {}>", "1"),
            ("Ext<{ a: 1 }, {}>", "1"),
            ("Ext<Record<\"a\" | \"b\", number>, { a: number }>", "1"),
            ("Ext<{ a: number }, Record<\"a\" | \"b\", number>>", "2"),
        ],
    );
}

#[test]
fn related_forms_around_the_fixes() {
    check_all(
        "type Id<T> = { [K in keyof T]: T[K] };
         type Ext<X, Y> = X extends Y ? 1 : 2;
         type Param<T> = T extends (x: infer P) => void ? P : never;
         type Ret<T> = T extends (a: any) => infer R ? R : never;
         type F1 = (x: string) => void; type F2 = (x: string, y: number) => void;",
        &[
            ("Id<{ a: 1 } | undefined>", "{ a: 1 } | undefined"),
            ("Id<string>", "string"),
            ("Ext<{ a?: number }, Record<string, number>>", "1"),
            ("Ext<{ a: number; b: string }, Record<string, number>>", "2"),
            ("Ext<Record<string, number>, { a: number }>", "2"),
            ("Ext<{ a: 1; b: 2 }, Record<\"a\" | \"b\", number>>", "1"),
            ("Param<(x: string | number) => void>", "number | string"),
            ("Ret<(x: string) => number>", "number"),
            ("Ext<F1, F2>", "1"),
            ("Ext<F2, F1>", "2"),
            ("Ext<boolean, true | false>", "1"),
            ("Ext<boolean, string | boolean>", "1"),
        ],
    );
}

// The second review's cases (each compared with tsc 5.9.3 `--strict`).

#[test]
fn number_literals_compare_by_value() {
    check_all(
        "",
        &[
            ("1 extends 1.0 ? \"y\" : \"n\"", "\"y\""),
            ("1.0 extends 1 ? \"y\" : \"n\"", "\"y\""),
            ("0 extends 0.0 ? \"y\" : \"n\"", "\"y\""),
            ("Exclude<1 | 2 | 3, 2.0>", "1 | 3"),
            ("Extract<0 | 1, 1.0>", "1"),
            ("1.5 extends 1 ? \"y\" : \"n\"", "\"n\""),
        ],
    );
    let e = err_src("type R = Exclude<1 | 2 | 3, 2.0>;\nfunction main() { const r: R = 2; }");
    assert!(!e.is_empty());
}

#[test]
fn weak_types_need_a_common_property() {
    check_all(
        "type HasOptA<T> = T extends { a?: number } ? 1 : 2;
         type Opt<T> = { [K in keyof T]?: T[K] };",
        &[
            ("{ b: 2 } extends { a?: 1 } ? 1 : 2", "2"),
            ("HasOptA<{ b: 1 }>", "2"),
            ("{ c: 1 } extends { a?: 1; b?: 2 } ? 1 : 2", "2"),
            ("{ a?: 1 } extends { b?: 1 } ? 1 : 2", "2"),
            ("Opt<{ a: 1 }> extends Opt<{ b: 1 }> ? 1 : 2", "2"),
            // No properties at all, or one in common: related as before.
            ("{} extends { a?: 1 } ? 1 : 2", "1"),
            ("{ a: 1; b: 2 } extends { a?: 1 } ? 1 : 2", "1"),
            ("HasOptA<{ a: 1 }>", "1"),
        ],
    );
}

#[test]
fn boolean_holes_match_true_and_false() {
    check_all(
        "type BoolT<S> = S extends `${boolean}` ? 1 : 2;",
        &[
            ("BoolT<\"true\">", "1"),
            ("BoolT<\"false\">", "1"),
            ("BoolT<\"true\" | \"no\">", "1 | 2"),
            ("BoolT<\"false\" | \"true\">", "1"),
            ("BoolT<\"yes\">", "2"),
            ("`true` extends `${boolean}` ? 1 : 2", "1"),
            ("\"truex\" extends `${boolean}x` ? 1 : 2", "1"),
            ("\"falsey\" extends `${boolean}${string}` ? 1 : 2", "1"),
            (
                "\"ab-c\" extends `${\"a\" | \"ab\"}-${string}` ? 1 : 2",
                "1",
            ),
            ("`${boolean}`", "\"false\" | \"true\""),
        ],
    );
}

#[test]
fn keyof_keeps_number_keys() {
    check_all(
        "",
        &[
            ("keyof { [K in 1 | 2]: K }", "1 | 2"),
            ("keyof { [K in 1 | \"a\"]: K }", "\"a\" | 1"),
            ("keyof { [K in \"a\" | 1 as K]: K }", "\"a\" | 1"),
            ("Exclude<keyof { [K in 1 | \"b\"]: 0 }, string>", "1"),
            ("Extract<keyof { [K in 1 | \"b\"]: 0 }, number>", "1"),
            ("{ [K in \"a\" | 1]: K }[1]", "1"),
            ("keyof { \"1\": 0 }", "\"1\""),
        ],
    );
    ok_src("type Num = Extract<keyof { [K in 1 | \"b\"]: 0 }, number>;\nfunction main() { const n: Num = 1; }");
}

#[test]
fn colliding_as_keys_keep_undefined_of_a_later_optional_key() {
    check_all(
        "type Flat<T> = { [K in keyof T as \"all\"]: T[K] };",
        &[
            ("Flat<{ a: 1; b?: 2 }>", "{ all: 1 | 2 | undefined }"),
            (
                "{ [K in keyof { a: 1; b?: 2 } as \"z\"]: { a: 1; b?: 2 }[K] }",
                "{ z: 1 | 2 | undefined }",
            ),
            ("Flat<{ a?: 1; b: 2 }>", "{ all?: 1 | 2 }"),
        ],
    );
    ok_src("type Flat<T> = { [K in keyof T as \"all\"]: T[K] };\ntype F = Flat<{ a: 1; b?: 2 }>;\nfunction main() { const f: F = { all: undefined }; }");
}

#[test]
fn closed_records_relate_to_open_ones() {
    check_all(
        "type Ext<X, Y> = X extends Y ? 1 : 2;",
        &[
            (
                "Record<\"a\" | \"b\", 1> extends Record<string, number> ? 1 : 2",
                "1",
            ),
            ("Ext<Record<\"a\" | \"b\", 1>, Record<string, string>>", "2"),
            ("Ext<Record<string, 1>, Record<string, number>>", "1"),
            // By the type arguments first, as tsc relates two instances of one alias.
            (
                "Ext<Record<string, number>, Record<\"a\" | \"b\", number>>",
                "1",
            ),
            ("Ext<Record<string, number>, Record<\"a\" | \"b\", 1>>", "2"),
            (
                "Ext<Record<\"a\" | \"c\", number>, Record<\"a\" | \"b\", number>>",
                "2",
            ),
            (
                "Ext<Record<\"a\" | \"b\" | \"c\", 1>, Record<\"a\" | \"b\", number>>",
                "1",
            ),
        ],
    );
}

#[test]
fn mapped_types_and_indexed_access_over_records() {
    check_all(
        "type Id<T> = { [K in keyof T]: T[K] };
         type Opt<T> = { [K in keyof T]?: T[K] };
         type ValOf<T> = T[keyof T];",
        &[
            ("Id<Record<\"x\" | \"y\", 1>>", "{ x: 1; y: 1 }"),
            ("Opt<Record<\"x\" | \"y\", 1>>", "{ x?: 1; y?: 1 }"),
            ("ValOf<Record<\"x\" | \"y\", 1>>", "1"),
            ("Record<\"x\" | \"y\", \"v\">[\"x\"]", "\"v\""),
            ("Record<string, 2>[\"any\"]", "2"),
        ],
    );
}

#[test]
fn a_call_breaking_a_key_constraint_is_not_instantiated() {
    // One error, at the argument that widened `K` (as tsc reports it), and none from the body.
    let e = err_src(
        "function swap<T extends object, K extends keyof T>(o: T, a: K, b: K): void {
           const t = o[a]; o[a] = o[b]; o[b] = t;
         }
         function get<T extends object, K extends keyof T>(o: T, k: K): T[K] { return o[k]; }
         type Pt = { x: number; y: number };
         function main() { const p: Pt = { x: 1, y: 2 }; swap(p, \"x\", \"w\"); get(p, \"w\"); }",
    );
    assert_eq!(e.matches("error:").count(), 2, "{e}");
    assert_eq!(
        e.matches("does not satisfy the constraint").count(),
        2,
        "{e}"
    );
    assert!(!e.contains("has no field"), "{e}");
}

#[test]
fn an_instantiation_error_through_another_generic_labels_each_call() {
    let e = err_src(
        "function getA<T extends object, K extends keyof T>(o: T, k: K): T[K] { return o[k]; }
         function outer<T extends object, K extends keyof T>(o: T, k: K): T[K] { return getA(o, k); }
         type Pt = { x: number; name: string };
         function main() {
           const p: Pt = { x: 1, name: \"n\" };
           const k: keyof Pt = p.x > 2 ? \"x\" : \"name\";
           outer(p, k);
         }",
    );
    assert!(e.contains("`getA` is instantiated here"), "{e}");
    assert!(e.contains("`outer` is instantiated here"), "{e}");
    assert!(e.contains("in `getA` at this instantiation"), "{e}");
    assert!(e.contains("reached through `outer`"), "{e}");
}
