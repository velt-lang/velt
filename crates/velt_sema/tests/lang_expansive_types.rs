//! A generic type that refers to itself at a growing type argument (`L<T>` holding an `L<T[]>`)
//! has infinitely many instantiations: a function mentioning one is reported (at a call that
//! instantiates a generic function with it, when there is one) instead of overflowing the
//! compiler's stack in lowering. Recursive types whose arguments do not grow still compile.

mod common;

use common::programs::{err_src, ok_src};

const L: &str = "type L<T> = { v: T; n: L<T[]> | null };\n";

fn errors(body: &str) -> String {
    err_src(&format!("{L}{body}"))
}

#[test]
fn json_parse_of_a_growing_type_is_reported_at_the_call() {
    let e = errors(
        "function main() {
           const x = JSON.parse<L<number>>('{\"v\":1,\"n\":null}');
           console.log(x.v);
         }",
    );
    assert!(
        e.contains("`L<T>` refers to itself at a growing type argument (`L<T[]>`), so `JSON.parse` cannot read it"),
        "{e}"
    );
    assert!(
        e.contains("each `L<T>` needs one with a longer type argument"),
        "{e}"
    );
    assert_eq!(e.matches("error:").count(), 1, "{e}");
}

#[test]
fn deep_equal_of_a_growing_type_is_reported_at_the_call() {
    let e = errors(
        "function main() {
           const a: L<number> = { v: 1, n: null };
           console.log(deepEqual(a, a));
         }",
    );
    assert!(e.contains("so `deepEqual` cannot compare it"), "{e}");
}

#[test]
fn a_value_of_a_growing_type_is_reported_where_it_is_made() {
    let e = errors(
        "function main() {
           const a: L<number> = { v: 1, n: { v: [2], n: null } };
           console.log(a.v);
         }",
    );
    assert!(
        e.contains("(`L<T[]>`), so a value of `L<f64>` cannot be compiled"),
        "{e}"
    );
}

#[test]
fn a_type_holding_a_growing_type_is_reported_too() {
    let e = errors(
        "class W { l: L<string> | null = null; }
         function main() { const w = new W(); console.log(w.l === null); }",
    );
    assert!(e.contains("so a value of `W` cannot be compiled"), "{e}");
}

#[test]
fn growth_through_another_generic_type_and_a_union() {
    let e = err_src(
        "class Pair<A> { constructor(public a: A, public rest: Many<A> | null) {} }
         type Many<B> = Pair<Map<string, B>> | string;
         function main() { const p = new Pair<i64>(1, null); console.log(p.a); }",
    );
    assert!(
        e.contains(
            "`Pair<A>` refers to itself at a growing type argument (`Pair<Map<string, A>>`)"
        ),
        "{e}"
    );
}

#[test]
fn a_generic_function_instantiating_it_is_reported() {
    let e = errors(
        "function make<T>(v: T): L<T> { return { v, n: null }; }
         function main() { console.log(make<number>(1).v); }",
    );
    assert!(
        e.contains("refers to itself at a growing type argument"),
        "{e}"
    );
}

#[test]
fn recursive_types_that_do_not_grow_still_compile() {
    ok_src(
        "type List<T> = { v: T; next: List<T> | null };
         type Tree<T> = { v: T; kids: Tree<T>[] };
         type Two<T> = { x: T; other: Two<string> | null };
         function main() {
           const l: List<number> = { v: 1, next: { v: 2, next: null } };
           const t: Tree<string> = { v: \"a\", kids: [] };
           const w: Two<number> = { x: 1, other: { x: \"s\", other: null } };
           console.log(JSON.stringify(l), deepEqual(t, t), w.x);
         }",
    );
}

#[test]
fn a_module_constant_of_a_growing_type_is_reported() {
    let e = errors(
        "const ROOT: L<string> = { v: \"a\", n: null };
         function main() { console.log(1); }",
    );
    assert!(
        e.contains("so a value of `L<string>` cannot be compiled"),
        "{e}"
    );
}

#[test]
fn a_growing_type_converted_to_any_is_reported() {
    let e = errors(
        "function main() {
           const a: L<number> = { v: 1, n: null };
           const x: any = a;
           console.log(x.v);
         }",
    );
    assert!(
        e.contains("so a value of `L<f64>` cannot be compiled"),
        "{e}"
    );
    // Only that error: the walk that looks for a lock in it stopped without one.
    assert_eq!(e.matches("error:").count(), 1, "{e}");
    let e = errors(
        "function main() {
           const a: L<number> = { v: 1, n: null };
           const u: unknown = a;
           console.log(typeof u);
         }",
    );
    assert!(!e.contains("lock"), "{e}");
    assert_eq!(e.matches("error:").count(), 1, "{e}");
}

#[test]
fn a_growing_type_that_is_never_used_is_accepted() {
    ok_src(&format!("{L}function main() {{ console.log(1); }}"));
    // Nor are the methods of a growing class (and the closures in them) on their own.
    ok_src(
        "class G<T> {
           n: G<T[]> | null = null;
           constructor(public v: T) {}
           next(): G<T[]> | null { return this.n; }
           later(): () => G<T[]> | null { return () => this.n; }
         }
         function main() { console.log(1); }",
    );
}

/// `Box<Box<…<inner>…>>`, `n` boxes deep: a legitimate type whose type arguments nest deeper
/// than the walks' cutoff for growing types (`MAX_ARG_DEPTH`, 64).
fn boxed(n: usize, inner: &str) -> String {
    format!("{}{inner}{}", "Box<".repeat(n), ">".repeat(n))
}

const BOX: &str = "class Box<T> { constructor(public v: T) {} }\n";

#[test]
fn a_deep_type_that_does_not_grow_is_walked_to_its_end() {
    // The lock 80 boxes down is found: converting the value to `any` is still an error.
    let deep = boxed(80, "shared<Mutex<number>>");
    let e = err_src(&format!(
        "{BOX}function f(b: {deep}): any {{ return b; }}
         function main() {{ console.log(1); }}"
    ));
    assert!(e.contains("it holds a lock"), "{e}");
    assert!(!e.contains("growing type argument"), "{e}");
    // Without one, the same conversion compiles.
    let plain = boxed(80, "number");
    ok_src(&format!(
        "{BOX}function f(b: {plain}): any {{ return b; }}
         function main() {{ console.log(1); }}"
    ));
}
