//! Computed member keys in declarations (`["k"]: T`, `[1]: T`, `[Flags.SKIP]?: T`), number
//! property names, inline symbol keys, and type arguments that do not continue across a line
//! break.

mod common;

use common::*;

fn interface(src: &str) -> InterfaceDecl {
    let m = parse_ok(src);
    m.items
        .iter()
        .find_map(|i| match &i.kind {
            ItemKind::Interface(d) => Some(d.clone()),
            _ => None,
        })
        .expect("an interface")
}

fn names(d: &InterfaceDecl) -> Vec<String> {
    d.fields.iter().map(|f| f.name.name.clone()).collect()
}

#[test]
fn literal_keys_name_the_member_plainly() {
    let d = interface(
        "interface H { s: string; [\"content-type\"]: string; [1]: number; [1.50]: number; }",
    );
    assert_eq!(names(&d), ["s", "content-type", "1", "1.5"]);
}

#[test]
fn enum_member_and_constant_keys() {
    let src = "interface T { [ReactiveFlags.SKIP]?: boolean; [KEY]: number; [a.b.C]: string; }";
    let d = interface(src);
    assert_eq!(names(&d), ["[ReactiveFlags.SKIP]", "[KEY]", "[a.b.C]"]);
    // The span is the path alone, as for `[KEY]` (sema tells a computed key by it).
    let f = &d.fields[0];
    let span = &src[f.name.span.lo as usize..f.name.span.hi as usize];
    assert_eq!(span, "ReactiveFlags.SKIP");
    assert!(d.fields[0].optional);
}

#[test]
fn computed_keys_after_a_member_on_the_line_before() {
    let d = interface("interface T {\n  s: S\n  [\"k\"]: number\n  n: N[]\n  [Flags.A]: string\n}");
    assert_eq!(names(&d), ["s", "k", "n", "[Flags.A]"]);
    // An index signature is not a computed key.
    parse_ok("interface T {\n  s: S\n  [k: string]: number\n}");
}

#[test]
fn class_members_and_object_types() {
    let m = parse_ok("class R { public readonly [Flags.IS_REF] = true; [\"size\"] = 3; }\ntype O = { [\"a\"]: number; [Flags.B]?: string };");
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!()
    };
    let names: Vec<_> = c.fields.iter().map(|f| f.name.name.as_str()).collect();
    assert_eq!(names, ["[Flags.IS_REF]", "size"]);
}

#[test]
fn number_property_names() {
    assert_eq!(
        sx(&expr(
            "({ 1.50: y, 1e3: x, 9007199254740993: z, 0x10: w, [2.5]: v })"
        )),
        "(paren {1.5: y, 1000: x, 9007199254740992: z, 16: w, 2.5: v})"
    );
}

#[test]
fn inline_symbol_keys_ask_for_a_module_constant() {
    for src in [
        "const o = { [Symbol(\"x\")]: 1 };",
        "const o = { [Symbol.for(\"q\")]: 1 };",
        "interface I { [Symbol(\"x\")]: number; }",
        "class C { [Symbol(\"x\")] = 1; }",
    ] {
        let e = errors(src);
        assert_eq!(e.len(), 1, "{src}: {e:?}");
        assert!(
            e[0].contains("declare it as a module constant"),
            "{src}: {e:?}"
        );
    }
    // The statement after the literal is still parsed.
    let e = errors(
        "function f() {\n  const o = { [Symbol(\"x\")]: 1, b: 2 };\n  g();\n}\nconst x = 1;",
    );
    assert_eq!(e.len(), 1, "{e:?}");
}

#[test]
fn type_arguments_do_not_continue_across_a_line_break() {
    let d = interface("interface Id {\n  s: string\n  <T>(x: T): T\n}");
    assert_eq!(names(&d), ["s"]);
    assert_eq!(d.call_sigs.len(), 1);
    let d = interface("interface G {\n  f<T>(x: T):\n    T\n  z: number\n}");
    assert_eq!(names(&d), ["z"]);
    // On the same line they are type arguments.
    let d = interface("interface M { m: Map<string, number> }");
    assert_eq!(ty(&d.fields[0].ty), "Map<string, number>");
}

#[test]
fn a_conditional_type_as_a_member_type() {
    let d = interface("interface C<T> { a: T extends string ? number : boolean; b: number }");
    assert_eq!(names(&d), ["a", "b"]);
}

#[test]
fn number_names_in_declarations_and_patterns() {
    let d = interface(
        "interface I { 7: string; 1.5: string; 1e3: number; [-1]: string; [-0]: number }",
    );
    assert_eq!(names(&d), ["7", "1.5", "1000", "-1", "0"]);
    let m = parse_ok("class C { 1.5 = \"a\"; [-2] = \"m\"; }");
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!()
    };
    let fields: Vec<_> = c.fields.iter().map(|f| f.name.name.as_str()).collect();
    assert_eq!(fields, ["1.5", "-2"]);
    assert_eq!(sx(&expr("({ [-1]: a, [-0]: b })")), "(paren {-1: a, 0: b})");
    parse_ok("function f(o: O) { const { 1.5: a, 1e3: b, [-1]: c, [\"k\"]: d } = o; }");
}

#[test]
fn a_pattern_takes_no_path_keys() {
    // `{ [t.tag]: v } = o` reads the property `t.tag` names while the program runs.
    let e = errors("function f() { const { [t.tag]: v, w } = o; g(); }");
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(e[0].contains("destructuring pattern"), "{e:?}");
    // Nor another expression; a name (`[KEY]`) is left to sema.
    for src in [
        "function f() { const { [K.A]: v } = o; }",
        "function f() { const { [\"a\" + \"b\"]: v } = o; }",
        "function f() { const { [+1]: v } = o; }",
    ] {
        let e = errors(src);
        assert_eq!(e.len(), 1, "{src}: {e:?}");
        assert!(e[0].contains("destructuring pattern"), "{src}: {e:?}");
    }
    parse_ok("function f() { const { [KEY]: v, [k]: w, [`ab`]: x, [1.5]: y, [-1]: z } = o; }");
    // In declarations they are member names.
    parse_ok("interface T { [Flags.A]: string; [KEY]: number }");
}

#[test]
fn literal_names_are_for_fields_not_methods() {
    for src in [
        "class C { [\"a-b\"](): number { return 1; } }",
        "class C { get [\"c d\"](): number { return 2; } }",
        "class C { 1e3() { return 1; } }",
        "interface I { [2](): void; }",
        "interface I { \"go\"(): void; }",
        "const o = { [\"p-q\"]() { return 1; }, b: 2 };",
        "const o = { 1.5() { return 1; } };",
        "class C { get 1() { return 1; } }",
        "class C { get \"x\"() { return 1; } }",
        "class C { set 2(v: number) {} }",
        "const o = { get 2() { return 1; } };",
        "const o = { get \"x\"() { return 1; } };",
    ] {
        let e = errors(src);
        assert_eq!(e.len(), 1, "{src}: {e:?}");
        assert!(
            e[0].contains("supported for fields, not methods"),
            "{src}: {e:?}"
        );
    }
}

#[test]
fn type_arguments_after_new_and_in_heritage_clauses_may_follow_a_line_break() {
    // These are expressions with type arguments in TypeScript, with no line restriction.
    let src = "class Base<T> { v: number = 4; }\ninterface I<T> { v: number }\nclass C extends Base\n<number> implements I\n<string> {}\ninterface J extends I\n<number> {}\nfunction f() {\n  const m = new Map\n  <string, number>();\n}";
    let (m, d) = parse(src);
    assert!(
        d.is_empty(),
        "{:?}",
        d.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let ItemKind::Class(c) = &m.items[2].kind else {
        panic!("{:?}", m.items[2].kind)
    };
    assert_eq!(ty(c.extends.as_ref().unwrap()), "Base<number>");
    assert_eq!(ty(&c.implements[0]), "I<string>");
}

#[test]
fn number_names_after_modifiers() {
    let m = parse_ok("class C { static readonly 2 = 3; readonly 7 = 4; private 8 = 1; }");
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!()
    };
    let fields: Vec<_> = c.fields.iter().map(|f| f.name.name.as_str()).collect();
    assert_eq!(fields, ["2", "7", "8"]);
    let d = interface("interface I { readonly 1: string; readonly 1e3: number }");
    assert_eq!(names(&d), ["1", "1000"]);
    parse_ok("type T = { readonly 0x10: boolean };");
}
