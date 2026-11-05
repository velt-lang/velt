//! The check of stores by key follows a function value however it reaches a call (#854). Each row
//! is a pair: a store by key puts the captured value in `k7`, so changing `c.k7` must be reported,
//! naming the store, and changing `c.other`, which no key names, must compile. All the rows are
//! run, and every wrong one is listed. The goldens `http_handler_store_converted_*` and
//! `http_handler_store_callback_*` run some of these end to end.

mod common;

use common::programs::{load_src_at, Loaded};

const HEAD: &str = r#"import { onEachWorker } from "./pool";
class App { plugins: string[] = ["a"]; }
class Holder { app: App; k7: string[] = []; other: string[] = []; constructor(a: App) { this.app = a; } }
class S { put(p: any, v: any): void { p["k7"] = v; } }
type Hk = (o: Holder, v: string[]) => void;
type Op = (o: any, v: any) => void;
function noop(o: Holder, v: string[]): void {}
"#;

/// `g` takes `any`s, so that where a place of type `Hk` holds it, it holds a conversion.
const G: &str = "function g(o: any, v: any): void { const w: string[] = v; new S().put(o, w); }\n";
/// A table of `Hk` holding a closure of that type.
const T: &str = "const t: Hk[] = [(o: Holder, v: string[]): void => { new S().put(o, v); }];\n";
const RUN: &str = "function run(f: Hk, o: Holder, v: string[]): void { f(o, v); }\n";
const CALL: &str = "t[0](c, l.plugins);";
const IN_FOR_EACH: &str = "t.forEach((h: Hk): void => { h(c, l.plugins); });";

/// Reports the store in `c.k7` and compiles with `c.other`? A message for each that doesn't.
fn pair(name: &str, decl: &str, call: &str) -> Vec<String> {
    let mut wrong = vec![];
    for (field, must_report) in [("k7", true), ("other", false)] {
        let loaded = program(decl, call, field);
        let (compiled, diags) = loaded.check();
        let text = loaded.render(&diags);
        let reported = text.contains("changes `c.k7`") && text.contains("by key");
        let right = match must_report {
            true => compiled.is_none() && reported,
            false => compiled.is_some(),
        };
        if !right {
            wrong.push(format!("{name}, c.{field}:\n{text}"));
        }
    }
    wrong
}

fn program(decl: &str, call: &str, field: &str) -> Loaded {
    let dir = std::env::temp_dir().join(format!("velt-keyed-rows-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("pool.vlt"),
        "export async function onEachWorker(work: () => Promise<void>) {\n  await spawn(work());\n}\n",
    )
    .unwrap();
    let src = format!(
        "{HEAD}{decl}\nasync function main() {{\n  const l = new App();\n  await onEachWorker(async (): Promise<void> => {{\n    const c = new Holder(l);\n    {call}\n    c.{field}.push(\"x\");\n  }});\n}}\nmain();\n"
    );
    let mut loaded = load_src_at(&dir.join("main.vlt"), &src);
    for m in &mut loaded.modules {
        m.is_dependency |= m.path.ends_with("pool");
    }
    loaded
}

fn all_right(rows: &[(&str, String, &str)]) {
    let wrong: Vec<String> = rows
        .iter()
        .flat_map(|(name, decl, call)| pair(name, decl, call))
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// With `g` of `any` parameters, or others that differ from the place's type.
#[test]
fn a_store_by_key_through_a_converted_function_is_reported() {
    let g = |rest: &str| format!("{G}{rest}");
    let in_table = |decl: &str| format!("{decl}const t: Hk[] = [noop];\nt[0] = g;\n");
    let rows: Vec<(&str, String, &str)> = vec![
        ("const", g(""), "const f: Hk = g; f(c, l.plugins);"),
        ("let", g(""), "let w: Hk = noop; w = g; w(c, l.plugins);"),
        (
            "let at module level",
            g("let w: Hk = noop;\nw = g;\n"),
            "w(c, l.plugins);",
        ),
        (
            "let assigned in a function",
            g("let w: Hk = noop;\nfunction set(): void { w = g; }\nset();\n"),
            "w(c, l.plugins);",
        ),
        ("parameter", g(RUN), "run(g, c, l.plugins);"),
        (
            "parameter, two levels",
            format!("{G}{RUN}function run2(f: Hk, o: Holder, v: string[]): void {{ run(f, o, v); }}\n"),
            "run2(g, c, l.plugins);",
        ),
        (
            "parameter of a method",
            g("class Rn { run(f: Hk, o: Holder, v: string[]): void { f(o, v); } }\n"),
            "new Rn().run(g, c, l.plugins);",
        ),
        (
            "constructor parameter in a field",
            g("class Rn { f: Hk; constructor(f: Hk) { this.f = f; } go(o: Holder, v: string[]): void { this.f(o, v); } }\n"),
            "new Rn(g).go(c, l.plugins);",
        ),
        (
            "reg(g)",
            g("const t: Hk[] = [];\nfunction reg(f: Hk): void { t.push(f); }\nreg(g);\n"),
            CALL,
        ),
        ("t[0] = g", in_table(G), CALL),
        (
            "r.h = g",
            g("class R { h: Hk = noop; }\nconst r = new R();\nr.h = g;\n"),
            "r.h(c, l.plugins);",
        ),
        (
            "m.set",
            g("const m = new Map<string, Hk>();\nm.set(\"g\", g);\n"),
            "m.get(\"g\")!(c, l.plugins);",
        ),
        (
            "return",
            g("function pick(): Hk { return g; }\n"),
            "pick()(c, l.plugins);",
        ),
        (
            "ternary",
            g(""),
            "const h: Hk = l.plugins.length > 0 ? g : noop; h(c, l.plugins);",
        ),
        ("as any as", g(""), "const f = g as any as Hk; f(c, l.plugins);"),
        (
            "as unknown as",
            g(""),
            "const f = g as unknown as Hk; f(c, l.plugins);",
        ),
        (
            "v: any",
            in_table("function g(o: Holder, v: any): void { const w: string[] = v; new S().put(o, w); }\n"),
            CALL,
        ),
        (
            "fewer parameters",
            in_table("function g(o: Holder): void { new S().put(o, o.app.plugins); }\n"),
            CALL,
        ),
        (
            "rest parameter",
            in_table("function g(o: Holder, v: string[], ...r: number[]): void { new S().put(o, v); }\n"),
            CALL,
        ),
        // The same conversion written out, with a maker followed or not.
        (
            "a wrapper in a local",
            g(""),
            "const g0 = g; const f: Hk = (o: Holder, v: string[]): void => { g0(o, v); }; f(c, l.plugins);",
        ),
        (
            "a wrapper registered",
            g("const t: Hk[] = [];\nfunction reg(f: Op): void { t.push((o: Holder, v: string[]): void => { f(o, v); }); }\nreg(g);\n"),
            CALL,
        ),
        (
            "a wrapper returned",
            g("function conv(f: Op): Hk { return (o: Holder, v: string[]): void => { f(o, v); }; }\nconst h = conv(g);\n"),
            "h(c, l.plugins);",
        ),
        (
            "a nullable table",
            g("const t: (Hk | null)[] = [];\nfunction init(): void { t.push(g); }\ninit();\n"),
            "t[0]!(c, l.plugins);",
        ),
    ];
    all_right(&rows);
}

/// `h` is a parameter of the callback: nothing that runs the callback here says what it is.
#[test]
fn a_store_by_key_through_a_function_called_in_a_callback_is_reported() {
    let t = |rest: &str| format!("{T}{rest}");
    let nullable =
        "const t: (Hk | null)[] = [(o: Holder, v: string[]): void => { new S().put(o, v); }];\n";
    let rows: Vec<(&str, String, &str)> = vec![
        ("forEach", t(""), IN_FOR_EACH),
        (
            "forEach, no annotation",
            t(""),
            "t.forEach((h) => { h(c, l.plugins); });",
        ),
        (
            "map",
            t(""),
            "t.map((h: Hk): number => { h(c, l.plugins); return 1; });",
        ),
        (
            "reduce",
            t(""),
            "t.reduce((n: number, h: Hk): number => { h(c, l.plugins); return n; }, 0);",
        ),
        (
            "Map.forEach",
            "const m = new Map<string, Hk>();\nm.set(\"a\", (o: Holder, v: string[]): void => { new S().put(o, v); });\n".to_string(),
            "m.forEach((h: Hk, k: string): void => { h(c, l.plugins); });",
        ),
        (
            "a user helper",
            t("function each(hs: Hk[], f: (h: Hk) => void): void { for (const h of hs) { f(h); } }\n"),
            "each(t, (h: Hk): void => { h(c, l.plugins); });",
        ),
        (
            "a generic helper",
            t("function each<X>(xs: X[], f: (x: X) => void): void { for (const x of xs) { f(x); } }\n"),
            "each(t, (h: Hk): void => { h(c, l.plugins); });",
        ),
        (
            "a method helper",
            t("class Rg { each(f: (h: Hk) => void): void { for (const h of t) { f(h); } } }\n"),
            "new Rg().each((h: Hk): void => { h(c, l.plugins); });",
        ),
        (
            "a callback kept in a field",
            t("class Ev { cb: (h: Hk) => void; constructor(cb: (h: Hk) => void) { this.cb = cb; } fire(): void { for (const h of t) { this.cb(h); } } }\n"),
            "new Ev((h: Hk): void => { h(c, l.plugins); }).fire();",
        ),
        (
            "a helper returning the callback",
            t("function caller(o: Holder, v: string[]): (h: Hk) => void { return (h: Hk): void => { h(o, v); }; }\n"),
            "t.forEach(caller(c, l.plugins));",
        ),
        (
            "passed on to a function",
            format!("{T}{RUN}"),
            "t.forEach((h: Hk): void => { run(h, c, l.plugins); });",
        ),
        (
            "nested callback",
            t(""),
            "t.forEach((h: Hk): void => { [1].forEach((n: number): void => { h(c, l.plugins); }); });",
        ),
        (
            "a closure in the callback",
            t(""),
            "t.forEach((h: Hk): void => { const f = (): void => { h(c, l.plugins); }; f(); });",
        ),
        (
            "a nullable parameter",
            nullable.to_string(),
            "t.forEach((h: Hk | null): void => { if (h) { h(c, l.plugins); } });",
        ),
        (
            "an optional call",
            nullable.to_string(),
            "t.forEach((h: Hk | null): void => { h?.(c, l.plugins); });",
        ),
        (
            "a member that is a conversion",
            format!("{G}const t: Hk[] = [];\nfunction init(): void {{ t.push(g); }}\ninit();\n"),
            IN_FOR_EACH,
        ),
        // A named function given to std, which nothing else runs.
        (
            "a named callback in a local",
            t("let cur: Holder | null = null;
function visit(h: Hk): void { h(cur!, cur!.app.plugins); }
"),
            "cur = c; const v2 = visit; t.forEach(v2);",
        ),
        (
            "a named callback chosen",
            t("let cur: Holder | null = null;
function visit(h: Hk): void { h(cur!, cur!.app.plugins); }
"),
            "cur = c; t.forEach(l.plugins.length > 0 ? visit : visit);",
        ),
        (
            "a named callback through an any",
            t("let cur: Holder | null = null;
function visit(h: Hk): void { h(cur!, cur!.app.plugins); }
"),
            "cur = c; const a: any = visit; t.forEach(a);",
        ),
        (
            "a named callback",
            t("let cur: Holder | null = null;\nfunction visit(h: Hk): void { h(cur!, cur!.app.plugins); }\n"),
            "cur = c; t.forEach(visit);",
        ),
    ];
    all_right(&rows);
}

/// A member that only reads what it is given feeds nothing, however it is reached.
#[test]
fn a_function_that_only_reads_is_not_reported() {
    let reads = "const t: Hk[] = [(o: Holder, v: string[]): void => { const m = o.k7.length + v.length; }];\n";
    for (decl, call) in [
        (reads, IN_FOR_EACH),
        (
            "function g(o: any, v: any): void { const m = o.k7.length; }\nconst t: Hk[] = [g];\n",
            CALL,
        ),
    ] {
        let loaded = program(decl, call, "k7");
        let (compiled, diags) = loaded.check();
        assert!(compiled.is_some(), "{}", loaded.render(&diags));
    }
}

/// A function nothing else names is bound by its callers, so a helper given one that only reads runs
/// only that: not every value of the parameter's type, one of which stores by key.
#[test]
fn a_helper_given_a_function_that_only_reads_runs_only_that() {
    let decl = "const t: Hk[] = [(o: Holder, v: string[]): void => { new S().put(o, v); }];\n\
                function reads(o: Holder, v: string[]): void { const m = o.k7.length + v.length; }\n\
                function stores(o: Holder, v: string[]): void { new S().put(o, v); }\n\
                class Rn { run(f: Hk, o: Holder, v: string[]): void { f(o, v); } }\n";
    let decl = format!("{decl}{RUN}");
    let mut wrong = vec![];
    for call in [
        "run(reads, c, l.plugins);",
        "new Rn().run(reads, c, l.plugins);",
    ] {
        let loaded = program(&decl, call, "k7");
        let (compiled, diags) = loaded.check();
        if compiled.is_none() {
            wrong.push(format!("{call}:\n{}", loaded.render(&diags)));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a helper given the store",
            decl.clone(),
            "run(stores, c, l.plugins);",
        ),
        (
            "a method given the store",
            decl.clone(),
            "new Rn().run(stores, c, l.plugins);",
        ),
    ];
    all_right(&rows);
}
