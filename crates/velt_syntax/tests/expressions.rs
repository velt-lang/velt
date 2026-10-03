//! Expression forms: calls, members, generic calls, `new`, arrows, literals, `match`.

mod common;

use common::*;

#[test]
fn calls_members_index() {
    check("f()", "(call f [])");
    check("f(1, 2,)", "(call f [1 2])");
    check("f(...xs, 1)", "(call f [...xs 1])");
    check("a.b.c(d)[e]", "([] (call (. (. a b) c) [d]) e)");
    check("f(a)(b)", "(call (call f [a]) [b])");
    check("x.type", "(. x type)");
    check("p.new", "(. p new)");
    check("this.x", "(. this x)");
    check("s.length", "(. s length)");
}

#[test]
fn generic_calls_vs_comparisons() {
    check("f<T>(x)", "(call f<T> [x])");
    check("f<Map<K, V>>(x)", "(call f<Map<K, V>> [x])");
    check("f<Map<K, Array<V>>>()", "(call f<Map<K, Array<V>>> [])");
    check("a.b<i64, string>(1)", "(call (. a b)<i64, string> [1])");
    check("a < b", "(< a b)");
    check("a < b > c", "(> (< a b) c)");
    check("i < n && j > 0", "(&& (< i n) (> j 0))");
    check("a < b >= c", "(>= (< a b) c)");
    check("f(a < b, c > d)", "(call f [(< a b) (> c d)])");
    check("x >>= y", "(>>= x y)");
    check("f<\"x\" | null>(s)", "(call f<(\"x\" | null)> [s])");
    check("f<1, true>()", "(call f<1, true> [])");
    check("i < 10 && j > 0", "(&& (< i 10) (> j 0))");
    check("a < 1 > b", "(> (< a 1) b)");
    check("a < \"x\" | b", "(| (< a \"x\") b)");
    // A negative number is a literal type too (TS): `g<-1>(5)`, but `a < -1` compares.
    check("g<-1>(5)", "(call g<-1> [5])");
    check("g<-1 | 2, -0.5>()", "(call g<(-1 | 2), -0.5> [])");
    check("a < -1", "(< a (- 1))");
    check("a<-1", "(< a (- 1))");
    check("i < -1 && j > 0", "(&& (< i (- 1)) (> j 0))");
    check("a < -1 > b", "(> (< a (- 1)) b)");
    check("a < -b > (c)", "(> (< a (- b)) (paren c))");
}

/// Object type literals as explicit type arguments (#238, #277), with either separator and
/// generic member types; a comparison with an object literal stays a comparison.
#[test]
fn object_type_arguments() {
    check("f<{ n: i64 }>(x)", "(call f<{n: i64}> [x])");
    check(
        "JSON.parse<{ k: i64, m: Map<string, i64> }>(s)",
        "(call (. JSON parse)<{k: i64; m: Map<string, i64>}> [s])",
    );
    check(
        "JSON.parse<{ k: i64; m: Map<string, i64>; }>(s)",
        "(call (. JSON parse)<{k: i64; m: Map<string, i64>}> [s])",
    );
    check(
        "f<{ a: Array<Map<K, V>> }, { b?: string }>()",
        "(call f<{a: Array<Map<K, V>>}, {b: (string | null)}> [])",
    );
    check("f<{ p: { q: i64[] } }[]>()", "(call f<{p: {q: i64[]}}[]> [])");
    check("a < { n: 1 }", "(< a {n: 1})");
}

#[test]
fn new_expressions() {
    check("new Foo()", "(new Foo [])");
    check("new Foo(1, 2)", "(new Foo [1 2])");
    check("new Map<string, i64>()", "(new Map<string, i64> [])");
    check("new ns.Foo", "(new ns.Foo [])");
    check("new Foo().bar()", "(call (. (new Foo []) bar) [])");
}

#[test]
fn arrow_functions() {
    check("x => x + 1", "(arrow (x) (+ x 1))");
    check("() => 1", "(arrow () 1)");
    check("(a, b) => a + b", "(arrow (a, b) (+ a b))");
    check(
        "(a: i64, b: f64): f64 => b",
        "(arrow (a: i64, b: f64): f64 b)",
    );
    check("(s: string) => s", "(arrow (s: string) s)");
    check("() => {}", "(arrow () {0 stmts})");
    check("(x) => { return x; }", "(arrow (x) {1 stmts})");
    check("() => ({ a: 1 })", "(arrow () (paren {a: 1}))");
    check(
        "async () => await f()",
        "(async arrow () (await (call f [])))",
    );
    check("async x => x", "(async arrow (x) x)");
    check(
        "async (x: i64): Promise<i64> => x",
        "(async arrow (x: i64): Promise<i64> x)",
    );
    check(
        "f(x => x * 2, (a, b) => a)",
        "(call f [(arrow (x) (* x 2)) (arrow (a, b) a)])",
    );
    check("x => y => x + y", "(arrow (x) (arrow (y) (+ x y)))");
    check("(a) + 1", "(+ (paren a) 1)");
    check("(a)", "(paren a)");
    check("c ? (a) : b", "(? c (paren a) b)");
    check(
        "(f: (x: i64) => i64) => f(1)",
        "(arrow (f: fn(i64) => i64) (call f [1]))",
    );
    check("(xs: i64[]) => xs", "(arrow (xs: i64[]) xs)");
}

#[test]
fn object_array_struct_literals() {
    check("[]", "[]");
    check("[1, 2, ...xs,]", "[1, 2, ...xs]");
    check("({})", "(paren {})");
    check("({ a: 1, b, ...c })", "(paren {a: 1, b, ...c})");
    check(
        "({ \"key with space\": 1, if: 2 })",
        "(paren {key with space: 1, if: 2})",
    );
    check("Point { x: 1, y: 2 }", "Point {x: 1, y: 2}");
    check("Point { x, y }", "Point {x, y}");
    check("Point {}", "Point {}");
    check("Point { ...p, x: 0 }", "Point {...p, x: 0}");
    check("f(Point { x: 1 })", "(call f [Point {x: 1}])");
    check("[[1, 2], [3]]", "[[1, 2], [3]]");
}

#[test]
fn await_and_spread() {
    check("await f()", "(await (call f []))");
    check("await a + b", "(+ (await a) b)");
}

#[test]
fn match_is_an_identifier() {
    // `match` is no longer a keyword: `match(x)` is an ordinary call.
    check("match(x)", "(call match [x])");
    check("match + 1", "(+ match 1)");
    let errs = errors("function f() { match (x) { 1 => a }; }");
    assert!(errs[0].contains("`match` is not supported"), "{errs:?}");
}
