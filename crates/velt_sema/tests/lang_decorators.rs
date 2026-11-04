//! Compile-time decorators (#2203): what sema records in `Program::decorators`, by target, with
//! the arguments folded. The errors are goldens (`tests/golden/lang/decorators_*`).

mod common;

use common::programs::{err_src, ok_src};
use velt_sema::hir::{ConstTree as C, Decorator, DecoratorTarget as T, Def, DefId, Program};

const DECLS: &str = r#"
declare decorator function name(key: string): FieldDecorator;
declare decorator function policy(p: "a" | "b", n: i32 = 3, o?: string): TypeDecorator;
declare decorator function tag(t: string): Repeatable<FunctionDecorator | MethodDecorator>;
declare decorator function level(l: Level): ParamDecorator;
declare decorator function opts(o: { a: i64; b?: f64[] }): MethodDecorator;
declare decorator function many(...xs: i64[]): MethodDecorator;
enum Level { Low, High }
const KEY = "k" as const;
const ALIAS = KEY;
"#;

fn def_named(p: &Program, wanted: &str) -> DefId {
    let name = |d: &Def| match d {
        Def::Fn(f) => f.name.clone(),
        Def::ExternFn(f) => f.name.clone(),
        Def::Adt(a) => a.name.clone(),
        Def::Enum(e) => e.name.clone(),
        _ => String::new(),
    };
    let i = p.defs.iter().position(|d| name(d) == wanted);
    DefId(i.unwrap_or_else(|| panic!("no definition `{wanted}`")) as u32)
}

fn program(rest: &str) -> Program {
    ok_src(&format!("{DECLS}{rest}\nfunction main() {{}}"))
}

fn on(p: &Program, target: T) -> &[Decorator] {
    p.decorators.get(&target).map_or(&[], |d| d.as_slice())
}

#[test]
fn an_undecorated_program_records_nothing() {
    assert!(ok_src("class C { m(a: i64) {} } function main() {}")
        .decorators
        .is_empty());
    assert!(program("class C { m(a: i64) {} }").decorators.is_empty());
}

#[test]
fn a_type_with_defaults_filled_in() {
    let p = program(r#"@policy("b") class C {}"#);
    let uses = on(&p, T::Def(def_named(&p, "C")));
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].def, def_named(&p, "policy"));
    assert_eq!(uses[0].args, [C::Str("b".into()), C::Int(3), C::Undefined]);
}

#[test]
fn a_field_by_its_index_with_the_base_class_first() {
    let p = program(
        r#"class B { first: i64 = 0; }
           class C extends B { plain: i64 = 0; @name("id") id: i64 = 1; }"#,
    );
    let uses = on(&p, T::Field(def_named(&p, "C"), 2));
    assert_eq!(uses[0].args, [C::Str("id".into())]);
    assert_eq!(p.decorators.len(), 1);
}

#[test]
fn a_parameter_by_its_written_position() {
    let p = program("class C { m(a: i64, @level(Level.High) b: i64) {} }");
    let m = def_named(&p, "C.m");
    let uses = on(&p, T::Param(m, 1));
    assert_eq!(uses[0].args, [C::EnumMember(def_named(&p, "Level"), 1)]);
    assert_eq!(p.decorators.len(), 1);
}

#[test]
fn repeated_decorators_stay_in_source_order() {
    let p = program(r#"@tag("x") @tag(KEY) @tag(ALIAS) function f() {}"#);
    let args: Vec<_> = on(&p, T::Def(def_named(&p, "f")))
        .iter()
        .map(|u| u.args.clone())
        .collect();
    assert_eq!(
        args,
        [
            [C::Str("x".into())],
            [C::Str("k".into())],
            [C::Str("k".into())]
        ]
    );
}

#[test]
fn object_array_and_rest_arguments() {
    let p =
        program("class C { @opts({ a: -1, b: [1.5, 2] }) m() {} @many(1, 2) r() {} @many n() {} }");
    assert_eq!(
        on(&p, T::Def(def_named(&p, "C.m")))[0].args,
        [C::Object(vec![
            ("a".into(), C::Int(-1)),
            (
                "b".into(),
                C::Array(vec![C::Float(1.5f64.to_bits()), C::Int(2)])
            ),
        ])]
    );
    assert_eq!(
        on(&p, T::Def(def_named(&p, "C.r")))[0].args,
        [C::Array(vec![C::Int(1), C::Int(2)])]
    );
    assert_eq!(
        on(&p, T::Def(def_named(&p, "C.n")))[0].args,
        [C::Array(vec![])]
    );
}

#[test]
fn an_overload_keeps_its_decorators_on_the_implementation() {
    let p = program(
        r#"@tag("o") function f(a: i64): i64; function f(a: string): string;
           function f(a: i64 | string): i64 | string { return a; }"#,
    );
    assert_eq!(on(&p, T::Def(def_named(&p, "f"))).len(), 1);
}

#[test]
fn a_constant_cycle_is_no_constant() {
    let e = err_src(&format!(
        r#"{DECLS} const A: string = B; const B: string = A;
           class C {{ @name(A) f: i64 = 0; }} function main() {{}}"#
    ));
    assert!(e.contains("must be a constant"), "{e}");
}

#[test]
fn constants_doubling_through_each_other_stop_at_a_limit() {
    let mut chain = String::from("const C0 = [1, 1];\n");
    for i in 1..60 {
        let p = i - 1;
        chain.push_str(&format!("const C{i} = [C{p}, C{p}];\n"));
    }
    let e = err_src(&format!(
        "declare decorator function list(x: any): FieldDecorator;
         {chain}
         class C {{ @list(C59) f: i64 = 0; }}
         function main() {{}}"
    ));
    assert!(e.contains("must be a constant"), "{e}");
}

#[test]
fn the_result_type_is_only_a_word_in_a_declaration() {
    // A program's own type of that name is untouched, and the word is no type elsewhere.
    ok_src(
        r#"class FieldDecorator { x: i64 = 1; }
              declare decorator function name(): FieldDecorator;
              class C { @name f: FieldDecorator = new FieldDecorator(); }
              function main() { console.log(new C().f.x); }"#,
    );
    let e = err_src("function f(a: FieldDecorator) {} function main() {}");
    assert!(e.contains("FieldDecorator"), "{e}");
}

#[test]
fn a_local_named_in_a_nested_class_is_run_time() {
    let e = err_src("function f(dec: any) { @dec class C {} } function main() {}");
    assert!(
        e.contains("TC39 run-time decorators are not supported yet"),
        "{e}"
    );
}
