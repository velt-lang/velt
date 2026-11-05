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

const TC39: &str = "TC39 run-time decorators are not supported yet";

/// The bodies a nested item can be written in that aren't a function's block, with `ITEM` where
/// it goes, each a whole program.
const INITIALIZERS: &[&str] = &[
    "class K { static { ITEM } }",
    "class K { static { const a = () => { ITEM }; a(); } }",
    "class K { x: number = (() => { ITEM return 1; })(); }",
    "class K { static s: number = (() => { ITEM return 1; })(); }",
    "class K { static readonly r: number = (() => { ITEM return 1; })(); }",
    "class K { f: () => void = function () { ITEM }; }",
    "struct K { x: number = (() => { ITEM return 1; })(); }",
    "function p(a: number = (() => { ITEM return 1; })()) {}",
    "function p({ a = (() => { ITEM return 1; })() }: { a?: number }) {}",
    "class K { m(a: number = (() => { ITEM return 1; })()) {} }",
    "class K { constructor(a: number = (() => { ITEM return 1; })()) {} }",
    "extend number { m(a: number = (() => { ITEM return 1; })()): number { return a; } }",
    "const c = (a: number = (() => { ITEM return 1; })()) => a;",
    "const o = function (a: number = (() => { ITEM return 1; })()) {};",
];

#[test]
fn a_run_time_decorator_is_refused_in_an_initializer_or_a_default() {
    for body in INITIALIZERS {
        let src = format!(
            "function logged(v: any, c: any): any {{ return v; }}
             {}
             function main() {{}}",
            body.replace("ITEM", "@logged class H {}")
        );
        let e = err_src(&src);
        assert!(e.contains(TC39), "{body}: {e}");
    }
}

#[test]
fn an_unknown_decorator_is_refused_in_an_initializer_or_a_default() {
    for body in INITIALIZERS {
        let src = format!(
            "{} function main() {{}}",
            body.replace("ITEM", "@nosuch class H {}")
        );
        let e = err_src(&src);
        assert!(e.contains("cannot find `nosuch`"), "{body}: {e}");
    }
}

#[test]
fn a_compile_time_decorator_is_recorded_in_an_initializer_or_a_default() {
    for body in INITIALIZERS {
        let src = format!(
            "declare decorator function mark(): TypeDecorator;
             {}
             function main() {{}}",
            body.replace("ITEM", "@mark class H {}")
        );
        let p = ok_src(&src);
        assert_eq!(p.decorators.len(), 1, "{body}");
    }
}

/// The module-level decorator `t` and constant `K`, then `rest`.
fn with_module_names(rest: &str) -> String {
    format!(
        r#"declare decorator function t(): Repeatable<TypeDecorator | MethodDecorator | FunctionDecorator>;
           declare decorator function any_(s: string): TypeDecorator;
           const K = "module";
           {rest}
           function main() {{}}"#
    )
}

#[test]
fn a_local_hides_a_decorator() {
    for src in [
        "function f(t: any) { @t class C {} }",
        "function f() { const t: any = 1; @t class C {} }",
        "function f() { @t class C {} const t: any = 1; }",
        "function f() { let g = (t: any) => { @t class C {} }; }",
        "function f() { try {} catch (t) { @t class C {} } }",
        "function f() { for (const t of [1]) { @t class C {} } }",
        "function f() { for (let t = 0; t < 1; t++) { @t class C {} } }",
        "function f() { const { t } = { t: 1 }; @t class C {} }",
        "function f(t: any) { class C { @t m() {} } }",
        "function f(t: any) { class O { m() { @t class C {} } } }",
        "function f(t: any) { function g() { @t class C {} } }",
        "function f(a: i64 = (() => { const t: any = 1; @t class C {} return 1; })()) {}",
        "class K { x: i64 = ((t: any) => { @t class C {} return 1; })(1); }",
        "function f() { switch (1) { case 1: const t: any = 1; break; default: @t class C {} } }",
        "function f(t: any) { { { @t class C {} } } }",
    ] {
        let e = err_src(&with_module_names(src));
        assert!(e.contains(TC39), "{src}: {e}");
    }
    // And a namespace or an imported decorator (the library's is `mark`).
    for src in [
        r#"import * as t from "./_decorators_lib"; function f(t: any) { @t.mark class C {} } function main() {}"#,
        r#"import { mark } from "./_decorators_lib"; function f(mark: any) { @mark class C {} } function main() {}"#,
    ] {
        let e = in_golden_dir(src).expect_err(src);
        assert!(e.contains(TC39), "{src}: {e}");
    }
}

#[test]
fn only_a_local_in_scope_hides_a_decorator() {
    for src in [
        // Before and after the local's scope.
        "function f() { { const t: any = 1; } @t class C {} }",
        "function f() { @t class C {} { const t: any = 1; } }",
        "function f() { [1].map((t: number) => t); @t class C {} }",
        "function f() { for (const t of [1]) {} @t class C {} }",
        "function f() { try {} catch (t) {} @t class C {} }",
        "function f() { try { const t: any = 1; } finally { @t class C {} } }",
        // A function's parameters aren't in scope in its own decorators, as in TypeScript's
        // `resolveName`, which resolves those outside the function.
        "@t function g(t: any) {}",
        "class C { @t m(t: any) {} }",
        "function f() { @t function g(t: any) {} }",
        "function f() { function g(t: any) {} @t class C {} }",
    ] {
        let p = ok_src(&with_module_names(src));
        assert_eq!(p.decorators.len(), 1, "{src}");
    }
}

/// `n` functions with a parameter `t`, which hides nothing outside them.
fn padding(prefix: &str, n: usize) -> String {
    (0..n)
        .map(|i| format!("function {prefix}{i}(t: any): void {{}}\n"))
        .collect()
}

/// The walk finds the item a name is in wherever that item stands, and only that item: the
/// padding around it declares a `t` of its own.
#[test]
fn a_local_is_found_in_any_item_of_a_big_module() {
    const HIDES: &str = "function f(t: any) { @t class C {} }";
    const HIDES_NOTHING: &str = "function f() { @t class C {} }";
    let last_hides = "function main() { const t: any = 1; @t class C {} }";
    let program = |before: usize, item: &str, after: usize, last: &str| {
        let (a, b) = (padding("a", before), padding("b", after));
        format!("declare decorator function t(): TypeDecorator;\n{a}{item}\n{b}{last}\n")
    };
    for (before, after) in [(0, 300), (150, 150), (300, 0), (1, 0), (0, 1)] {
        let e = err_src(&program(before, HIDES, after, "function main() {}"));
        assert!(e.contains(TC39), "{before}, {after}: {e}");
        let p = ok_src(&program(before, HIDES_NOTHING, after, "function main() {}"));
        assert_eq!(p.decorators.len(), 1, "{before}, {after}");
    }
    let e = err_src(&program(300, "", 0, last_hides));
    assert!(e.contains(TC39), "{e}");
    let p = ok_src(&program(300, "", 0, "function main() { @t class C {} }"));
    assert_eq!(p.decorators.len(), 1);
    // A decorator on the item itself, written before it: its parameters don't hide it.
    let p = ok_src(
        &program(300, "@t2 function g(t2: any) {}", 0, "function main() {}").replace(
            "declare decorator function t(): TypeDecorator;",
            "declare decorator function t2(): FunctionDecorator;",
        ),
    );
    assert_eq!(p.decorators.len(), 1);
}

/// The items aren't in source order when sema walks them (`decorators::item_spans`). A script's
/// generated `main` comes last and spans the declarations between its statements, and a
/// function's overload signatures repeat its span.
#[test]
fn a_local_is_found_when_the_items_are_rewritten() {
    for src in [
        // An item inside the span of the script's `main`.
        "console.log(1); function f(t: any) { @t class C {} } console.log(2);",
        "console.log(1); function e(a: any) {} function f(t: any) { @t class C {} } console.log(2);",
        // A variable that moved into `main`, whose span holds the class outside its statements.
        "const t: any = 1; console.log(t); @t class C {} console.log(2);",
        // In `main` itself, where a statement's variable is declared.
        "console.log(1); { const t: any = 1; @t class C {} } console.log(2);",
        // Flattened out of a namespace.
        "namespace N { export interface K { m(t: any): void { @t class C {} } } } function g() {}
         function main() {}",
        // Repeated for each signature.
        "function f(t: any): void; function f(t: any, u: any): void;
         function f(t: any, u?: any): void { @t class C {} }
         function main() {}",
    ] {
        let src = format!("declare decorator function t(): TypeDecorator;\n{src}");
        let e = err_src(&src);
        assert!(e.contains(TC39), "{src}: {e}");
    }
    for src in [
        "console.log(1); function f() { @t class C {} } function g(t: any) {} console.log(2);",
        "console.log(1); function g(t: any) {} @t class C {} console.log(2);",
        "console.log(1); { const t: any = 1; } @t class C {} console.log(2);",
        "namespace N { export interface K { m(): void { @t class C {} } n(t: any): void {} } }
         function main() {}",
    ] {
        let src = format!("declare decorator function t(): TypeDecorator;\n{src}");
        let p = ok_src(&src);
        assert_eq!(p.decorators.len(), 1, "{src}");
    }
}

/// An imported module's statements become chunks, which come last among its items, with its
/// variables; a local in a function between them still hides, and one in a sibling doesn't.
#[test]
fn a_local_is_found_in_an_imported_module_with_statements() {
    let check = |lib_body: &str| {
        let dir = std::env::temp_dir().join(format!(
            "velt-decorator-chunks-{}-{:x}",
            std::process::id(),
            lib_body.len()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let lib = format!(
            "declare decorator function t(): TypeDecorator;\n\
             export let a = 1;\nconsole.log(a);\n{lib_body}\nexport let b = 2;\nconsole.log(b);\n"
        );
        std::fs::write(dir.join("lib.velt"), lib).unwrap();
        let root = dir.join("main.velt");
        let loaded = common::programs::load_src_at(
            &root,
            "import { a } from \"./lib\";\nfunction main() { console.log(a); }\n",
        );
        let (program, diags) = loaded.check();
        let result = match program.filter(|_| diags.iter().all(|d| !d.is_error())) {
            Some(p) => Ok(p),
            None => Err(loaded.render(&diags)),
        };
        std::fs::remove_dir_all(&dir).ok();
        result
    };
    let e = check("function f(t: any) { @t class C {} }").expect_err("a run-time decorator");
    assert!(e.contains(TC39), "{e}");
    let p = check("function f() { @t class C {} }\nfunction g(t: any) {}").unwrap();
    assert_eq!(p.decorators.len(), 1);
}

#[test]
fn a_local_is_no_constant() {
    for src in [
        r#"function g() { const K = "local"; @any_(K) class D {} }"#,
        "function f(K: string) { @any_(K) class C {} }",
        r#"function f() { for (const K of ["a"]) { @any_(K) class C {} } }"#,
        // Through a constant of the nested class, in an object shorthand and a member path.
        "function f(K: string) { class S { static readonly A: string = K; } @any_(S.A) class C {} }",
    ] {
        let e = err_src(&with_module_names(src));
        assert!(e.contains("must be a constant"), "{src}: {e}");
    }
    let e = err_src(
        r#"declare decorator function o(a: { K: string }): TypeDecorator;
           const K = "module";
           function f(K: string) { @o({ K }) class C {} }
           function main() {}"#,
    );
    assert!(e.contains("must be a constant"), "{e}");
    let e = err_src(
        "declare decorator function n(a: i64): TypeDecorator;
         enum E { A }
         function f(E: any) { @n(E.A) class C {} }
         function main() {}",
    );
    assert!(e.contains("must be a constant"), "{e}");
}

#[test]
fn a_constant_out_of_the_locals_scope_keeps_its_value() {
    let p = ok_src(&with_module_names(
        r#"function g() { { const K = "local"; } @any_(K) class D {} }"#,
    ));
    let uses: Vec<_> = p.decorators.values().flatten().collect();
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].args, [C::Str("module".into())]);
}

/// `src` checked as a file next to `tests/golden/lang/_decorators_lib.velt`.
fn in_golden_dir(src: &str) -> Result<Program, String> {
    let file = common::programs::repo_root().join("tests/golden/lang/decorators_probe.velt");
    let loaded = common::programs::load_src_at(&file, src);
    let (program, diags) = loaded.check();
    match program.filter(|_| diags.iter().all(|d| !d.is_error())) {
        Some(p) => Ok(p),
        None => Err(loaded.render(&diags)),
    }
}

#[test]
fn an_import_type_is_no_decorator_and_no_constant() {
    const MESSAGE: &str = "is imported with `import type` and cannot be used as a value";
    for src in [
        r#"import type { mark } from "./_decorators_lib"; @mark class C {}"#,
        r#"import { type mark } from "./_decorators_lib"; @mark class C {}"#,
        r#"import type { mark as m } from "./_decorators_lib"; @m class C {}"#,
        r#"import type { plain } from "./_decorators_lib"; @plain class C {}"#,
        r#"import type { arg, LIB_KEY } from "./_decorators_lib"; class C {}
           @LIB_KEY class D {}"#,
        r#"import { arg } from "./_decorators_lib";
           import type { LIB_KEY } from "./_decorators_lib"; @arg(LIB_KEY) class C {}"#,
        r#"import { arg } from "./_decorators_lib";
           import { type LIB_KEY } from "./_decorators_lib"; @arg([LIB_KEY][0]) class C {}"#,
    ] {
        let src = format!("{src}\nfunction main() {{}}");
        let e = in_golden_dir(&src).expect_err(&src);
        assert!(
            e.contains(MESSAGE) || e.contains("must be a constant"),
            "{src}: {e}"
        );
    }
    // By name, in the message an ordinary value gets.
    let e = in_golden_dir(
        r#"import { arg } from "./_decorators_lib";
           import type { LIB_KEY } from "./_decorators_lib";
           @arg(LIB_KEY) class C {}
           function main() {}"#,
    )
    .expect_err("an argument");
    assert!(e.contains(&format!("`LIB_KEY` {MESSAGE}")), "{e}");
    let e = in_golden_dir(
        r#"import type { mark } from "./_decorators_lib"; @mark class C {} function main() {}"#,
    )
    .expect_err("a decorator");
    assert!(e.contains(&format!("`mark` {MESSAGE}")), "{e}");
}

#[test]
fn a_nested_item_hides_an_import_type() {
    let e = in_golden_dir(
        r#"import type { mark } from "./_decorators_lib";
           function f() { function mark() {} @mark class C {} }
           function main() {}"#,
    )
    .expect_err("a run-time decorator");
    assert!(e.contains(TC39), "{e}");
}

#[test]
fn a_plain_import_is_still_a_decorator_and_a_constant() {
    let p = in_golden_dir(
        r#"import { arg, LIB_KEY } from "./_decorators_lib";
           @arg(LIB_KEY) class C {}
           function main() {}"#,
    )
    .unwrap();
    let uses: Vec<_> = p.decorators.values().flatten().collect();
    assert_eq!(uses[0].args, [C::Str("lib".into())]);
}

#[test]
fn a_default_must_fit_its_parameter() {
    const MESSAGE: &str = "this default of the decorator doesn't fit its parameter's type";
    for decl in [
        r#"g(a: string = 1)"#,
        r#"g(a: i64 = "x")"#,
        "g(a: u8 = 256)",
        r#"g(a: "x" | "y" = "z")"#,
        "g(a: string[] = [1])",
        "g(a: i64, b: boolean = 1)",
    ] {
        // Refused at the declaration, used or not.
        let e = err_src(&format!(
            "declare decorator function {decl}: TypeDecorator; function main() {{}}"
        ));
        assert!(e.contains(MESSAGE), "{decl}: {e}");
    }
    for decl in [
        r#"g(a: string = "x")"#,
        "g(a: i64 = 1)",
        "g(a: f64 = 1)",
        r#"g(a: "x" | "y" = "y")"#,
        "g(a: string[] = [])",
        "g(a: i64 | null = null)",
        "g(a?: string)",
    ] {
        ok_src(&format!(
            "declare decorator function {decl}: TypeDecorator; function main() {{}}"
        ));
    }
}

#[test]
fn type_arguments_on_a_decorator() {
    // Valid TypeScript, and a run-time decorator like any other.
    let e = err_src(
        r#"function fac<T>(a: string): any { return null; }
           @fac<string>("a") class C { @fac<string>("b") m() {} }
           function main() {}"#,
    );
    assert_eq!(e.matches(TC39).count(), 2, "{e}");
    let e = err_src(r#"@nosuch<string>("a") class C {} function main() {}"#);
    assert!(e.contains("cannot find `nosuch`"), "{e}");
    // A compile-time decorator has no type parameters.
    let e = err_src(
        r#"declare decorator function d(a: string): TypeDecorator;
           @d<string>("a") class C {}
           function main() {}"#,
    );
    assert!(
        e.contains("the decorator `d` has no type parameters"),
        "{e}"
    );
}

#[test]
fn a_default_export_is_decorated_on_either_side() {
    for (src, name) in [
        (r#"export default @policy("a") class C {}"#, "C"),
        (r#"@policy("a") export default class C {}"#, "C"),
        (r#"export default @policy("a") class {}"#, "default"),
        (r#"@policy("a") export default class {}"#, "default"),
    ] {
        let p = program(src);
        let uses = on(&p, T::Def(def_named(&p, name)));
        assert_eq!(uses.len(), 1, "{src}");
        assert_eq!(uses[0].def, def_named(&p, "policy"), "{src}");
    }
}

#[test]
fn an_ordinary_function_on_a_default_export_is_run_time() {
    for decorated in [
        "export default @logged class C {}",
        "@logged export default class C {}",
        "export default @logged class {}",
        "@logged export default class {}",
    ] {
        let src = format!("function logged(v: any, c: any): any {{ return v; }}\n{decorated}\nfunction main() {{}}");
        let e = err_src(&src);
        assert!(
            e.contains("TC39 run-time decorators are not supported yet (#2204)"),
            "{decorated}: {e}"
        );
    }
    let e = err_src("export default @nosuch class C {}\nfunction main() {}");
    assert!(e.contains("nosuch"), "{e}");
}

#[test]
fn a_default_functions_overloads_keep_its_decorators_on_the_implementation() {
    for src in [
        r#"@tag("t") export default function f(a: string): string; export default function f(a: any): any { return a; }"#,
        r#"export default function f(a: string): string; @tag("t") export default function f(a: any): any { return a; }"#,
        r#"export default function f(a: string): string; export default @tag("t") function f(a: any): any { return a; }"#,
    ] {
        let p = program(src);
        let uses = on(&p, T::Def(def_named(&p, "f")));
        assert_eq!(uses.len(), 1, "{src}");
        assert_eq!(uses[0].args, [C::Str("t".into())], "{src}");
    }
    // Written on two signatures it is used twice: it can repeat only if it says so.
    let p = program(
        r#"@tag("a") function f(a: string): string; @tag("b") function f(a: any): any { return a; }"#,
    );
    let uses = on(&p, T::Def(def_named(&p, "f")));
    assert_eq!(
        uses.iter().map(|u| &u.args[0]).collect::<Vec<_>>(),
        [&C::Str("a".into()), &C::Str("b".into())]
    );
}
