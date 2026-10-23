//! The root file's statements and declarations run in source order (#1438): a statement that
//! uses a declaration below it directly is an error; one inside a function it makes is not.

mod common;

use common::programs::{err_src, ok_src};

/// The "used before its declaration" lines of the rendered diagnostics.
fn early(rendered: &str) -> Vec<&str> {
    let mut out: Vec<&str> = rendered
        .lines()
        .filter(|l| l.contains("before its declaration"))
        .map(|l| l.split("error: ").nth(1).unwrap_or(l))
        .collect();
    out.dedup();
    out
}

#[test]
fn direct_uses_before_a_declaration_are_errors() {
    let r = err_src("console.log(K);\nconst K = 1;\nfunction f(): number { return K; }\n");
    assert_eq!(early(&r), ["`K` is used before its declaration"], "{r}");
    let r = err_src("const o = { K };\nconsole.log(o);\nconst K = 1;\n");
    assert_eq!(early(&r), ["`K` is used before its declaration"], "{r}");
    let r = err_src("if (true) { new C(); }\nclass C {}\n");
    assert_eq!(
        early(&r),
        ["class `C` is used before its declaration"],
        "{r}"
    );
    let r = err_src("const a = b + 1;\nconst b = 2;\nconsole.log(a);\n");
    assert_eq!(early(&r), ["`b` is used before its declaration"], "{r}");
}

#[test]
fn deferred_and_shadowed_uses_are_not() {
    ok_src(
        "const f = (): number => K;\n\
         const g = function (): number { return K; };\n\
         const o = { m(): number { return K; } };\n\
         { const K = 2; console.log(K); }\n\
         for (const C of [1]) { console.log(C); }\n\
         const K = 1;\n\
         class C { v: number = K; static readonly w: number = K; }\n\
         console.log(f(), g(), o.m(), K);\n",
    );
    ok_src("setTimeout(() => console.log(L.length), 0);\nconst L = [1, 2];\n");
}

/// A destructuring declaration stays among the statements (it cannot be module state yet): a
/// direct use above it is an early use, and a function made above it cannot read it. Each is
/// one error, not also "cannot find".
#[test]
fn variables_left_among_the_statements() {
    let r = err_src("console.log(x);\nconst { x } = { x: 1 };\n");
    assert_eq!(early(&r), ["`x` is used before its declaration"], "{r}");
    assert!(!r.contains("cannot find"), "{r}");
    let r = err_src("const g = (): number => x + 1;\nconst { x } = { x: 1 };\nconsole.log(g());\n");
    assert!(
        r.contains("`x` is declared below the function that reads it"),
        "{r}"
    );
    assert!(!r.contains("cannot find"), "{r}");
}

/// A parameter named like a declaration below shadows it.
#[test]
fn parameters_shadow_later_declarations() {
    ok_src(
        "console.log([1, 2].map((K) => K * 2));\n\
         const o = { m(K: number): number { return K; } };\n\
         console.log(o.m(1), [3].map(function (K) { return K; }));\n\
         const K = [9];\n\
         const { x } = { x: 1 };\n\
         console.log([4].map((x) => x), K, x);\n",
    );
}

/// A name bound in a nested scope (a parameter, a block's local, a loop's or a `catch` clause's
/// binding) hides the declaration below only in that scope: the statement's other uses of the
/// name are the declaration's.
#[test]
fn nested_bindings_hide_only_their_scope() {
    ok_src(
        "const f = (): number => { for (const K of [1]) { console.log(K); } return K; };\n\
         const g = (): number => [5].map((K) => K)[0] + K;\n\
         const h = { a: (K: number): number => K, b: (): number => K };\n\
         const c = (): number => { try { throw new Error(); } catch (K) { console.log(K); } return K; };\n\
         const b = (): number => { { const K = 1; console.log(K); } return K; };\n\
         function item(): number { return [5].map((K) => K)[0] + K; }\n\
         const K = 2;\n\
         console.log(f(), g(), h.a(1), h.b(), c(), b(), item());\n",
    );
    let r = err_src("console.log([1].map((K) => K), K);\nconst K = 5;\n");
    assert_eq!(early(&r), ["`K` is used before its declaration"], "{r}");
    assert!(!r.contains("cannot find"), "{r}");
    let r = err_src("{ const K = 1; console.log(K); }\nconsole.log(K);\nconst K = 2;\n");
    assert_eq!(early(&r), ["`K` is used before its declaration"], "{r}");
}

/// `import.meta` and the properties of `globalThis` exist before the module's code runs: a
/// top-level statement using them is not an early use.
#[test]
fn generated_declarations_are_not_below_the_statement() {
    ok_src(
        "console.log(import.meta.url.length > 0);\n\
         type Seam = { __X__?: number };\n\
         (globalThis as Seam).__X__ = 1;\n\
         console.log((globalThis as Seam).__X__);\n\
         const K = 1;\n\
         console.log(K);\n",
    );
}
