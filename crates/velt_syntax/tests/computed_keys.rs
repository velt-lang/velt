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
