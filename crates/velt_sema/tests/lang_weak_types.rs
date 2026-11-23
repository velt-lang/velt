//! `weak T` in written types (docs/internals/design/cycles.md "`weak T`", velt_sema
//! `weak_types`): where it may go and what it may refer to. Lowering handles a class's own field
//! referring to a class; the other well-formed places and targets are reported as not
//! implemented yet. Each test counts the diagnostics a shape gives.

mod common;

use common::programs::{err_src, ok_src};

const OUTSIDE: &str =
    "`weak` outside a class's own field or a variable is not implemented yet (#3119)";
const NOT_A_CLASS: &str = "`weak` to anything but a class instance is not implemented yet (#3119)";
const MISPLACED: &str = "`weak` is allowed only at the top of the type of a field or a variable";

/// The diagnostics of `src`, one per `error:` line.
fn errors(src: &str) -> Vec<String> {
    err_src(src)
        .lines()
        .filter_map(|l| l.split_once("error: ").map(|(_, m)| m.to_string()))
        .collect()
}

#[test]
fn a_class_field_and_a_parameter_property_are_lowered() {
    // A parameter property is a field: its parameter takes the type without `weak`.
    ok_src(
        "class Node {\n\
           parent: weak Node | null = null;\n\
           constructor(public owner: weak Node | null) {}\n\
         }\n\
         const n = new Node(null);\n\
         console.log(n.owner, n.parent);",
    );
}

#[test]
fn interface_values_are_targets() {
    let e = errors(
        "interface View { refresh(): void }\n\
         class Button { view: weak View | null = null; }\n\
         console.log(new Button().view);",
    );
    assert_eq!(e, [NOT_A_CLASS], "{e:?}");
}

#[test]
fn values_without_an_identity_are_not() {
    let e = errors(
        "interface Pt { x: number }\n\
         class A { p: weak Pt | null = null; }\n\
         class H<T> { h: weak T | null = null; }\n\
         console.log(new A().p, new H<A>().h);",
    );
    assert_eq!(
        e,
        [
            "`weak` refers to an object with an identity: `Pt` is stored by value (a struct, or an interface of fields only)",
            "`weak` refers to an object with an identity: the target is a type parameter, which may stand for a value without one",
        ],
        "{e:?}"
    );
}

#[test]
fn var_is_a_variable() {
    // A function's `var` is lowered (a `weak` local), and so is a module's that a function uses
    // (a module state slot, `hir::GlobalDef::weak`).
    ok_src(
        "class Node {}\n\
         var g: weak Node | null = null;\n\
         function f(): void { var l: weak Node | null = null; console.log(l, g); }\n\
         f(); console.log(g);",
    );
}

#[test]
fn an_object_type_member_is_not_lowered_yet() {
    let e = errors(
        "class Node {}\n\
         type P = { n: weak Node | null };\n\
         const p: P = { n: null };\n\
         console.log(p.n);",
    );
    assert_eq!(e, [OUTSIDE], "{e:?}");
}

#[test]
fn a_module_const_a_function_uses_is_not() {
    // One evaluated at each use would hold nothing: declare it with `let`.
    let e = errors(
        "class Node {}\n\
         const g: weak Node | null = null;\n\
         function f(): void { console.log(g); }\n\
         f();",
    );
    let msg = "`weak` on a module's `const` (declare it with `let`) is not implemented yet (#3119)";
    assert_eq!(e, [msg], "{e:?}");
}

#[test]
fn undefined_without_null_keeps_undefined() {
    // The missing `null` is reported, without a cascade on the `undefined` written.
    let e = errors(
        "class Node {}\n\
         let u: weak Node | undefined = undefined;\n\
         console.log(u);",
    );
    assert_eq!(e.len(), 2, "{e:?}");
    assert!(e[0].starts_with("a `weak` reference reads `null`"), "{e:?}");
    assert_eq!(
        e[1], "`weak` with `undefined` is not implemented yet (#3119)",
        "{e:?}"
    );
}

#[test]
fn a_parameter_is_not_a_field() {
    let e = errors("class Node {}\nfunction f(n: weak Node | null): void {}\nf(null);");
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(e[0].starts_with(MISPLACED), "{e:?}");
}

#[test]
fn dynamic_values_are_not_targets() {
    // `any` and `unknown` may hold a number or a string; `object` is a dynamic value too.
    let e = errors(
        "let a: weak any | null = null;\n\
         let u: weak unknown | null = null;\n\
         let o: weak object | null = null;\n\
         console.log(a, u, o);",
    );
    assert_eq!(
        e,
        [
            "`weak` refers to an object with an identity: `any` may hold a value without an identity (a number, a string)",
            "`weak` refers to an object with an identity: `unknown` may hold a value without an identity (a number, a string)",
            "`weak` refers to an object with an identity: `object` is a dynamic value: name the class, object type or interface it refers to",
        ],
        "{e:?}"
    );
}

#[test]
fn an_abstract_property_is_a_field() {
    // One diagnostic each, the field's: its getter and setter take the type without `weak`.
    let e = errors(
        "class Node {}\n\
         abstract class A {\n  abstract a: weak Node | null;\n  abstract readonly r: weak Node | null;\n}\n\
         console.log(1);",
    );
    let abs = "`weak` on an abstract property is not implemented yet (#3119)";
    assert_eq!(e, [abs, abs], "{e:?}");
}
