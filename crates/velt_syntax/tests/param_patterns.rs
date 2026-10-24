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
    // A typed default that uses an earlier parameter is applied in the function (`T | undefined`
    // at calls); an untyped or nullable one still can't be.
    parse_ok("function k(p: number, q: number = f({ p })) {}");
    has("function k(p: string, q: string | null = p) {}", earlier);
}

#[test]
fn an_object_literal_method_may_have_a_rest_parameter() {
    parse_ok("const o = { sum(...xs: number[]): number { return 0; } };");
}

#[test]
fn defaults_that_run_in_the_function_take_undefined_at_calls() {
    let m = parse_ok(
        "function f(a: number, b: number = a * 2, c: string = g(), d = 1, e: any = a, h: () => number = () => 1) {}",
    );
    let ps = &func(&m, 0).sig.params;
    let tys: Vec<String> = ps.iter().map(|p| ty(&p.ty)).collect();
    // `b` uses `a`; `c` runs after it, as in JavaScript; `d` is a constant; `any` already holds
    // `undefined`; `h` makes a closure.
    let undefined = [false, true, true, false, false, true];
    for (t, u) in tys.iter().zip(undefined) {
        assert_eq!(t.contains("undefined"), u, "{tys:?}");
    }
    let m = parse_ok("function f(a: number, c: string = g()) {}");
    assert_eq!(ty(&func(&m, 0).sig.params[1].ty), "string");
}

/// The error and its note for `src`'s first diagnostic.
fn first_error(src: &str) -> (String, String) {
    let (_, d) = parse(src);
    let d = d
        .iter()
        .next()
        .unwrap_or_else(|| panic!("no error for {src}"));
    (d.message.clone(), d.notes.join(" "))
}

#[test]
fn defaults_that_cannot_run_in_the_function_say_why() {
    let earlier = "a parameter default cannot use an earlier parameter (`a`) yet";
    // A destructured parameter's names, an untyped parameter, a type with `null`, a generator.
    let cases = [
        (
            "function f({ a }: P, b: number = a) {}",
            "before it unpacks destructured parameters",
        ),
        (
            "const f = (a: number, b = a) => b;",
            "needs the parameter's written type",
        ),
        (
            "function f(a: number, b: number | null = a) {}",
            "whose type includes `null`",
        ),
        (
            "function* f(a: number, b: number = a) {}",
            "a generator's parameter defaults",
        ),
        (
            "async function* f(a: number, b: number = a) {}",
            "a generator's parameter defaults",
        ),
        (
            "class C { *m(a: number, b: number = a) {} }",
            "a generator's parameter defaults",
        ),
        (
            "const o = { *m(a: number, b: number = a) {} };",
            "a generator's parameter defaults",
        ),
        (
            "const g = function* (a: number, b: number = a) {};",
            "a generator's parameter defaults",
        ),
    ];
    for (src, why) in cases {
        let (msg, note) = first_error(src);
        assert_eq!(msg, earlier, "{src}");
        assert!(note.contains(why), "{src}: {note}");
    }
    // A generator's defaults stay at the call: its parameter keeps its type.
    let m = parse_ok("function* f(a: number, cb: () => number = () => 1) {}");
    assert_eq!(ty(&func(&m, 0).sig.params[1].ty), "() => number");
}

#[test]
fn a_later_default_that_cannot_follow_one_run_in_the_function_is_an_error() {
    // JavaScript runs `c`'s default after `b`'s, which the function applies when the call
    // starts; `c`'s would run first at the call.
    let (msg, note) = first_error("const f = (a: number, b: number = a + 1, c = g()) => c;");
    assert!(msg.contains("needs the parameter's written type"), "{msg}");
    assert!(note.contains("runs parameter defaults in order"), "{note}");
    let src = "function f(a: number, b: number = a + 1, c: number | null = g()) {}";
    let (msg, _) = first_error(src);
    assert!(msg.contains("whose type includes `null`"), "{msg}");
    // A constant may follow, and so may a typed default, which runs in the function too.
    parse_ok("const f = (a: number, b: number = a + 1, c = 1, d: number | null = null) => c;");
    let m = parse_ok("function f(a: number, b: number = a + 1, c: string = g()) {}");
    assert!(ty(&func(&m, 0).sig.params[2].ty).contains("undefined"));
}
