//! The iteration protocol (std/prelude/iter.vlt, docs/internals/design/iteration.md): type
//! parameter defaults, `throws` clauses naming a generic interface's parameters, `bool`
//! discriminant narrowing and `for...of` over iterables.

mod common;

use common::programs::{err_src, ok_src};

const RANGE: &str = "
class RangeIter implements Iterator<i64> {
  i: i64 = 0;
  n: i64;
  constructor(n: i64) { this.n = n; }
  next(): IteratorResult<i64> {
    if (this.i >= this.n) { return { done: true }; }
    this.i += 1;
    return { done: false, value: this.i };
  }
}
class Range implements Iterable<i64> {
  n: i64;
  constructor(n: i64) { this.n = n; }
  [Symbol.iterator](): Iterator<i64> { return new RangeIter(this.n); }
}";

#[test]
fn type_parameter_defaults_fill_missing_arguments() {
    ok_src(
        "interface Box<T, U = T[]> { get(): U; }
         type Pair<A, B = A> = [A, B];
         class B implements Box<i64> { get(): i64[] { return [1]; } }
         function main() { const p: Pair<string> = [\"a\", \"b\"]; const b: Box<i64, i64[]> = new B(); console.log(p, b.get()); }",
    );
    let r =
        err_src("interface Box<T, U = T[]> { get(): U; } function f(b: Box) {} function main() {}");
    assert!(
        r.contains("type `Box` takes 2 type argument(s) but 0 were supplied"),
        "{r}"
    );
}

#[test]
fn bool_discriminants_narrow_as_conditions() {
    ok_src(
        "function f(r: IteratorResult<string>): string {
           if (r.done) { return \"done\"; }
           return r.value;
         }
         function g(r: IteratorResult<string>): string {
           if (!r.done) { return r.value; }
           return \"done\";
         }
         function main() { console.log(f({ done: true }), g({ done: false, value: \"x\" })); }",
    );
}

#[test]
fn for_of_over_an_iterable_throws_what_next_throws() {
    ok_src(&format!(
        "{RANGE}
         function sum<E>(xs: Iterable<i64, E>): i64 throws E {{
           let s = 0; for (const x of xs) {{ s += x; }} return s; }}
         function main() {{ for (const x of new Range(3)) {{ console.log(x); }} console.log(sum(new Range(2))); }}"
    ));
    let r = err_src(
        "class Bad extends Error {}
         class It implements Iterator<i64, Bad> {
           next(): IteratorResult<i64> throws Bad { throw new Bad(\"x\"); } }
         class Src implements Iterable<i64, Bad> {
           [Symbol.iterator](): Iterator<i64, Bad> { return new It(); } }
         function f(s: Src): i64 throws never { let n = 0; for (const x of s) { n += x; } return n; }
         function main() {}",
    );
    assert!(r.contains("`f` throws `Bad`"), "{r}");
}

#[test]
fn interface_throws_may_name_the_interface_parameters_only() {
    let r = err_src(
        "class Bad extends Error {}
         class It implements Iterator<i64> {
           next(): IteratorResult<i64> throws Bad { throw new Bad(\"x\"); } }
         function main() {}",
    );
    assert!(
        r.contains("`It.next` throws `Bad`, but `Iterator.next` does not allow it"),
        "{r}"
    );
    assert!(r.contains("implement `Iterator` with `Bad` as `E`"), "{r}");
    let r = err_src(
        "class Base<E> { m(): void throws E {} }
         class Sub extends Base<i64> { override m(): void {} }
         function main() {}",
    );
    assert!(
        r.contains("the `throws` clause of an overridden method cannot mention type parameters"),
        "{r}"
    );
}

#[test]
fn for_of_reports_what_is_not_iterable() {
    let r = err_src(&format!(
        "{RANGE} function main() {{ for (const x of new RangeIter(2)) {{ console.log(x); }} }}"
    ));
    assert!(
        r.contains("cannot iterate over a value of type `RangeIter`"),
        "{r}"
    );
    assert!(r.contains("looks like an iterator"), "{r}");
    let r = err_src(
        "class S { [Symbol.iterator](): i64 { return 1; } }
         function main() { for (const x of new S()) { console.log(x); } }",
    );
    assert!(
        r.contains("`[Symbol.iterator]()` must return an `Iterator<T>`, found `i64`"),
        "{r}"
    );
}

#[test]
fn builtin_iterables_convert_and_keep_their_loops() {
    let p = ok_src(
        "function sum(xs: Iterable<f64>): f64 { let s = 0.0; for (const x of xs) { s += x; } return s; }
         function first<I extends Iterable<string>>(xs: I): string { for (const x of xs) { return x; } return \"\"; }
         function arrays(xs: f64[]): f64 { let s = 0.0; for (const x of xs) { s += x; } return s; }
         function strings(t: string): i64 { let n = 0; for (const _ of t) { n++; } return n; }
         function maps(m: Map<string, i64>): i64 { let n = 0; for (const [_, v] of m) { n += v; } return n; }
         function main() {
           const m = new Map<string, f64>();
           console.log(sum([1, 2]), sum(m.values()), first(\"ab\"), first([\"c\"]));
           const it: Iterator<f64> = [1.5][Symbol.iterator]();
           const chars: Iterable<string> = \"xyz\";
           const entries: Iterable<[string, f64]> = m;
           console.log(it.next(), arrays([]), strings(\"\"), maps(new Map<string, i64>()));
         }",
    );
    // `for...of` over an array or a string keeps its `ForOf` loop; over a map place it is the
    // live cursor's `while`, and over an `Iterable<T>` value the protocol's (inside a block).
    let for_of = |f: &str| {
        let f = common::hir_walk::func(&p, f);
        f.body
            .block
            .stmts
            .iter()
            .any(|s| matches!(s.kind, velt_sema::hir::StmtKind::ForOf { .. }))
    };
    for f in ["arrays", "strings"] {
        assert!(for_of(f), "{f}");
    }
    assert!(!for_of("maps"));
    assert!(!for_of("sum"));
}

#[test]
fn an_extend_block_with_symbol_iterator_makes_an_iterable() {
    ok_src(&format!(
        "{RANGE}
         struct Pair {{ a: i64; b: i64; }}
         extend Pair {{ [Symbol.iterator](): Iterator<i64> {{ return new RangeIter(this.a + this.b); }} }}
         function count(xs: Iterable<i64>): i64 {{ let n = 0; for (const _ of xs) {{ n++; }} return n; }}
         function main() {{ const p: Pair = {{ a: 1, b: 2 }}; console.log(count(p)); }}"
    ));
}

#[test]
fn iterator_result_takes_no_return_type() {
    ok_src(
        "function f(r: IteratorResult<i64, void>, s: IteratorResult<string, any>): bool { return r.done && s.done; }
         function* g(): IterableIterator<i64, unknown> { yield 1; }
         function main() { const it: IterableIterator<i64> = g(); console.log(f({ done: true }, { done: true }), it.next()); }",
    );
    let r = err_src("function f(r: IteratorResult<i64, string>) {}");
    assert!(
        r.contains("`IteratorResult` takes no return type: `string` is TypeScript's `TReturn`"),
        "{r}"
    );
}
