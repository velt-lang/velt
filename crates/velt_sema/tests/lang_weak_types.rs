//! `weak T` in written types (docs/internals/design/cycles.md "`weak T`", velt_sema
//! `weak_types`): where it may go and what it may refer to. Its lowering is not implemented yet,
//! so a well-formed one is reported as such: each test counts the diagnostics a shape gives.

mod common;

use common::programs::err_src;

const NOT_YET: &str = "`weak` references are not implemented yet (#3119)";
const MISPLACED: &str = "`weak` is allowed only at the top of the type of a field or a variable";

/// The diagnostics of `src`, one per `error:` line.
fn errors(src: &str) -> Vec<String> {
    err_src(src)
        .lines()
        .filter_map(|l| l.split_once("error: ").map(|(_, m)| m.to_string()))
        .collect()
}

#[test]
fn a_parameter_property_is_a_field() {
    // One diagnostic, the field's: its parameter takes the type without `weak`.
    let e = errors(
        "class Node { constructor(public owner: weak Node | null) {} }\n\
         console.log(new Node(null).owner);",
    );
    assert_eq!(e, [NOT_YET], "{e:?}");
}

#[test]
fn interface_values_are_targets() {
    let e = errors(
        "interface View { refresh(): void }\n\
         class Button { view: weak View | null = null; }\n\
         console.log(new Button().view);",
    );
    assert_eq!(e, [NOT_YET], "{e:?}");
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
    let e = errors(
        "class Node {}\n\
         var g: weak Node | null = null;\n\
         function f(): void { var l: weak Node | null = null; console.log(l); }\n\
         f(); console.log(g);",
    );
    assert_eq!(e, [NOT_YET, NOT_YET], "{e:?}");
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
    assert_eq!(e[1], NOT_YET, "{e:?}");
}

#[test]
fn a_parameter_is_not_a_field() {
    let e = errors("class Node {}\nfunction f(n: weak Node | null): void {}\nf(null);");
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(e[0].starts_with(MISPLACED), "{e:?}");
}
