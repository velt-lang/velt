//! Destructured parameters (`function f([a, b]: T)`, `([k, v]) => …`), parameter types taken
//! from a default (`status = 302`) and rest parameters of arrows.

mod common;

use common::*;

fn func(m: &Module, i: usize) -> &FnDecl {
    match &m.items[i].kind {
        ItemKind::Function(f) => f,
        k => panic!("expected a function, got {k:?}"),
    }
}

#[test]
fn function_parameters_may_be_patterns() {
    let m = parse_ok("function f([a, b = 1]: [string, number], { c, d: e }: P, x: T) {}");
    let ps = &func(&m, 0).sig.params;
    assert_eq!(ps[0].name.name, "#param0");
    assert_eq!(pat(ps[0].pattern.as_ref().unwrap()), "[a, b = 1]");
    assert_eq!(pat(ps[1].pattern.as_ref().unwrap()), "{c: c, d: e}");
    assert!(ps[0].pattern.is_some() && ps[1].pattern.is_some() && ps[2].pattern.is_none());
    assert_eq!(ps[1].name.name, "#param1");
    assert_eq!(ty(&ps[1].ty), "P");
    assert_eq!(ps[2].name.name, "x");
}

#[test]
fn a_default_gives_the_parameter_its_type() {
    let m = parse_ok("function f(location: string, status = 302, live = true, s = \"\") {}");
    let ps = &func(&m, 0).sig.params;
    assert_eq!(ty(&ps[1].ty), "number");
    assert_eq!(ty(&ps[2].ty), "boolean");
    assert_eq!(ty(&ps[3].ty), "string");
    let errs = errors("function f(x = g()) {}");
    assert!(
        errs.iter()
            .any(|e| e.contains("cannot infer the type of parameter `x` from its default")),
        "{errs:?}"
    );
}

#[test]
fn arrow_parameters_may_be_patterns_and_rest() {
    let e = expr("xs.map(([k, v]) => k)");
    let ExprKind::Call { args, .. } = &e.kind else {
        panic!("expected a call")
    };
    let ExprKind::Arrow { params, .. } = &args[0].kind else {
        panic!("expected an arrow")
    };
    assert_eq!(pat(params[0].pattern.as_ref().unwrap()), "[k, v]");
    let e = expr("({ a, b }: P, ...rest: number[]) => a");
    let ExprKind::Arrow { params, .. } = &e.kind else {
        panic!("expected an arrow")
    };
    assert!(params[0].pattern.is_some() && params[1].rest);
    assert!(errors("const f = (...xs: number[], y: number) => y;")
        .iter()
        .any(|e| e.contains("a rest parameter must be the last parameter")));
}

#[test]
fn a_parameter_property_is_not_a_pattern() {
    let errs = errors("class P { constructor(private [a]: [string]) {} }");
    assert!(
        errs.iter()
            .any(|e| e.contains("a parameter property cannot be declared using a binding pattern")),
        "{errs:?}"
    );
}

#[test]
fn defaults_that_would_run_out_of_order_are_errors() {
    let has = |src: &str, msg: &str| {
        let errs = errors(src);
        assert!(errs.iter().any(|e| e.contains(msg)), "{src}: {errs:?}");
    };
    let order =
        "a parameter default after a destructured parameter with defaults must be a constant";
    has("function f({ x = g() }: P, y: number = h()) {}", order);
    has(
        "const f = ([a = g()]: number[], b: number = h()) => a;",
        order,
    );
    has(
        "class K { m({ a: [b = 1] }: P, { c }: Q = mk()) {} }",
        order,
    );
    has(
        "const o = { m({ x = 1 }: P, y: number = h()) { return 1; } };",
        order,
    );
    let earlier = "a parameter default cannot use an earlier parameter (`p`) yet";
    has(
        "function h({ p }: { p: number }, q: number = p + 1) {}",
        earlier,
    );
    has("function k(p: number, q: number = f({ p })) {}", earlier);
    has("const k = (p: number, q = [p]) => q;", earlier);
    has(
        "class C { constructor([p]: number[], q: number = p) {} }",
        earlier,
    );
    // Constants may follow, and defaults after a pattern without defaults are evaluated as
    // usual (before the body, where the pattern is destructured).
    parse_ok("function f({ x = g() }: P, y: number = -1, z = \"z\", w: number[] = [1, 2]) {}");
    parse_ok("function f({ x }: P, { y }: Q = mk(), z: number = g()) {}");
    parse_ok("function f(a: number, b: number = g(), c?: string, ...xs: number[]) {}");
}

#[test]
fn a_rest_parameter_of_an_object_literal_method_names_the_method() {
    let errs = errors("const o = { sum(...xs: number[]): number { return 0; } };");
    assert!(
        errs.iter().any(|e| e.contains(
            "rest parameters of object-literal methods (`sum(...xs)`) are not supported yet"
        )),
        "{errs:?}"
    );
}
